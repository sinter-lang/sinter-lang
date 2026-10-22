//! SystemVerilog backend.
//!
//! Emits portable, Yosys-synthesizable, Verilator-lint-clean SystemVerilog that
//! matches the `eval` semantics bit-for-bit. Conforms to the
//! `claude-skill-verilog` standard: `` `default_nettype none ``, explicit
//! widths, `always_ff` carrying only simple non-blocking assignments
//! (next-state computed in `always_comb`), named ports, one statement per line.
//!
//! Every operand is sized and signed **explicitly** — operands of `+`/`-`/`*`
//! are sign/zero-extended to the result width with `w'($signed(e))` / `w'(e)`,
//! casts resize the same way, shifts use concatenation / arithmetic `>>>` — so
//! Verilog's context-determined sizing can never silently change a width away
//! from `eval`. (On the legacy same-width `module` path no extension is added,
//! so its emission is unchanged.)

use std::fmt::Write as _;

use sinter_syntax::ast::{BinOp, CastMode, Type, UnaryOp};

use crate::ir::{Expr, ExprKind, Module};

/// Emit the SystemVerilog text for a module. The caller writes it to
/// `<module.name>.sv` (filename matches the module name, per the skill).
#[must_use]
pub fn emit_module(m: &Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "`default_nettype none");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "// Auto-generated from Sinter source. Do not edit by hand.\n\
         // Clock domain: {}",
        m.clock_domain
    );

    emit_ports(&mut out, m);
    emit_decls(&mut out, m);
    emit_mem_init(&mut out, m);
    emit_comb(&mut out, m);
    emit_instances(&mut out, m);
    emit_seq(&mut out, m);
    emit_mem_writes(&mut out, m);
    // `m.asserts` (DESIGN §6.12, FLT0) are sim-only and deliberately emit no
    // Verilog: the compile-out guarantee that keeps this module byte-identical
    // whether or not it carries assertions. The silicon-latched fault path (FLT1)
    // is what will add emitted bits, gated on the `--faults` build knob.

    let _ = writeln!(out, "endmodule");
    let _ = writeln!(out);
    let _ = writeln!(out, "`default_nettype wire");
    out
}

/// The signed/range prefix for a net of `ty` width, e.g. `signed [15:0]`.
fn net_type(ty: Type) -> String {
    let range = format!("[{}:0]", ty.width() - 1);
    if ty.is_signed() {
        format!("signed {range}")
    } else {
        range
    }
}

/// The Q-notation comment for a fixed-point net (CLAUDE.md: "document
/// fixed-point with Q-notation"), or `""` for an integer. `I` counts the sign
/// bit, matching Sinter's `iI.F` spelling; the formula makes that explicit.
fn q_comment(ty: Type, frac_bits: u32) -> String {
    if frac_bits == 0 {
        return String::new();
    }
    let int_bits = ty.width() - frac_bits;
    let sign = if ty.is_signed() { "signed" } else { "unsigned" };
    format!("  // Q{int_bits}.{frac_bits} {sign} fixed-point: value = raw / 2^{frac_bits}")
}

fn emit_ports(out: &mut String, m: &Module) {
    let _ = writeln!(out, "module {} (", m.name);
    let _ = writeln!(
        out,
        "    input  wire         clk,    // {} clock",
        m.clock_domain
    );
    let _ = writeln!(
        out,
        "    input  wire         rst_n,  // active-low asynchronous reset"
    );
    for (i, p) in m.ports.iter().enumerate() {
        let (dir, kw) = match p.dir {
            sinter_syntax::ast::Dir::In => ("input", "wire"),
            sinter_syntax::ast::Dir::Out => ("output", "logic"),
        };
        // The separating comma goes before any trailing comment.
        let comma = if i + 1 < m.ports.len() { "," } else { "" };
        let _ = writeln!(
            out,
            "    {dir:6} {kw:5} {} {}{comma}{}",
            net_type(p.ty),
            p.name,
            q_comment(p.ty, p.frac_bits)
        );
    }
    let _ = writeln!(out, ");");
}

/// The registers that exist in the emitted Verilog: all but the reference-model
/// registers of `#[verilog]` instances (DESIGN §6.15), which the real module
/// replaces.
fn hw_regs(m: &Module) -> impl Iterator<Item = &crate::ir::Reg> {
    m.regs.iter().filter(|r| !m.is_model_only(&r.name))
}

fn emit_decls(out: &mut String, m: &Module) {
    if hw_regs(m).next().is_some() {
        let _ = writeln!(out, "    // Registers");
        for r in hw_regs(m) {
            let _ = writeln!(
                out,
                "    logic {} {};{}",
                net_type(r.ty),
                r.name,
                q_comment(r.ty, r.frac_bits)
            );
            let _ = writeln!(out, "    logic {} {}_next;", net_type(r.ty), r.name);
        }
    }
    if m.lets.iter().any(|l| !m.is_model_only(&l.name)) {
        let _ = writeln!(out, "    // Combinational wires");
        for l in m.lets.iter().filter(|l| !m.is_model_only(&l.name)) {
            let _ = writeln!(out, "    logic {} {};", net_type(l.ty), l.name);
        }
    }
    if !m.mems.is_empty() {
        let _ = writeln!(out, "    // Block-RAM memories (DESIGN §6.6/§9.6)");
        for mem in &m.mems {
            // `logic [W-1:0] name [0:depth-1];` — an indexed array; Yosys infers
            // block RAM, kept out of the async-reset block so inference survives.
            let _ = writeln!(
                out,
                "    logic {} {} [0:{}];",
                net_type(mem.elem_ty),
                mem.name,
                mem.depth - 1
            );
        }
    }
}

/// Zero-initialize each memory array. The array has no async content reset (that
/// would block BRAM inference), so this `initial` block is the sim/Verilator
/// zero-init contract (DESIGN §9.6); on real silicon, rely on write-before-read.
/// Required because the co-sim runs with `--x-initial unique`.
fn emit_mem_init(out: &mut String, m: &Module) {
    for mem in &m.mems {
        let w = mem.elem_ty.width();
        if mem.reset.iter().all(|&v| v == 0) {
            // All-zero (`mem`): a compact `for`-loop. Yosys unrolls it and sets the
            // array INIT; required because the co-sim runs with `--x-initial unique`.
            let _ = writeln!(
                out,
                "    // Zero-initialize {} (sim/Verilator contract)",
                mem.name
            );
            let _ = writeln!(out, "    initial begin");
            let _ = writeln!(
                out,
                "        for (int {0}_i = 0; {0}_i < {1}; {0}_i = {0}_i + 1) begin",
                mem.name, mem.depth
            );
            let _ = writeln!(out, "            {}[{}_i] = {w}'b0;", mem.name, mem.name);
            let _ = writeln!(out, "        end");
            let _ = writeln!(out, "    end");
        } else {
            // `rom` contents: an explicit per-word init. Yosys turns this into the
            // array INIT (→ bitstream ROM); inline, so the SV is self-contained
            // (no runtime `$readmemh`, no external data file).
            let _ = writeln!(
                out,
                "    // Contents of {} (compile-time ROM data)",
                mem.name
            );
            let _ = writeln!(out, "    initial begin");
            for (i, &v) in mem.reset.iter().enumerate() {
                let _ = writeln!(out, "        {}[{i}] = {w}'h{v:x};", mem.name);
            }
            let _ = writeln!(out, "    end");
        }
    }
}

fn emit_comb(out: &mut String, m: &Module) {
    let _ = writeln!(out, "    // Combinational logic");
    let _ = writeln!(out, "    always_comb begin");
    // Model-only wires don't exist in Verilog, and an instance output is driven
    // by its instance rather than by its model's logic.
    for l in &m.lets {
        if !m.is_model_only(&l.name) && m.instance_output(&l.name).is_none() {
            let _ = writeln!(out, "        {} = {};", l.name, expr(&l.value));
        }
    }
    for (name, e) in &m.reg_updates {
        if !m.is_model_only(name) {
            let _ = writeln!(out, "        {name}_next = {};", expr(e));
        }
    }
    // Memory read ports: the read-data register's next value is the combinational
    // array read; flopping it (in the sequential block) makes it the registered,
    // `Static(1)`-latency read. Reads the pre-edge array (READ_FIRST).
    for r in &m.mem_reads {
        let _ = writeln!(
            out,
            "        {}_next = {}[{}];",
            r.rdata,
            r.mem,
            expr(&r.addr)
        );
    }
    for (name, e) in &m.out_drives {
        let _ = writeln!(out, "        {name} = {};", expr(e));
    }
    let _ = writeln!(out, "    end");
}

/// Emit the memory write ports: one `always_ff` per write, kept separate from
/// the async-reset state block so block-RAM inference survives. Non-blocking `<=`
/// means the read in `always_comb` sees the pre-edge contents (READ_FIRST). Like
/// real block RAM the array has **no content reset** (only the read-data register
/// resets), so a write commits whenever `we` is asserted — including during reset.
/// The simulator has no reset cycle (it begins post-reset), and the oracle holds
/// inputs at `0` during the reset pulse, so they agree; the MEM1 contract is that
/// a caller holds `we = 0` while the design is in reset (DESIGN §9.6). (Gating the
/// write with `rst_n` would either trip Verilator `SYNCASYNCNET` or, via a
/// reset-release flag, drop the first post-reset write and break the oracle.)
fn emit_mem_writes(out: &mut String, m: &Module) {
    for w in &m.mem_writes {
        let _ = writeln!(out, "    // Memory write port: {}", w.mem);
        let _ = writeln!(out, "    always_ff @(posedge clk) begin");
        let _ = writeln!(out, "        if ({}) begin", expr(&w.we));
        let _ = writeln!(
            out,
            "            {}[{}] <= {};",
            w.mem,
            expr(&w.addr),
            expr(&w.data)
        );
        let _ = writeln!(out, "        end");
        let _ = writeln!(out, "    end");
    }
}

/// Emit each foreign Verilog module instance (DESIGN §6.15) with named port
/// connections: the clock and reset (for a stateful model), then its inputs and
/// outputs, each wired to its port wire.
fn emit_instances(out: &mut String, m: &Module) {
    for inst in &m.instances {
        let _ = writeln!(
            out,
            "    // Foreign Verilog module `{}`, modelled in simulation by `{}`",
            inst.module, inst.model
        );
        let mut conns: Vec<(&str, &str)> = Vec::new();
        if let Some(clk) = &inst.clk {
            conns.push((clk, "clk"));
        }
        if let Some(rst_n) = &inst.rst_n {
            conns.push((rst_n, "rst_n"));
        }
        conns.extend(inst.inputs.iter().map(|(p, w)| (p.as_str(), w.as_str())));
        conns.extend(inst.outputs.iter().map(|(p, w)| (p.as_str(), w.as_str())));
        let width = conns.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
        let _ = writeln!(out, "    {} {} (", inst.module, inst.name);
        for (i, (port, wire)) in conns.iter().enumerate() {
            let comma = if i + 1 < conns.len() { "," } else { "" };
            let _ = writeln!(out, "        .{port:width$} ({wire}){comma}");
        }
        let _ = writeln!(out, "    );");
    }
}

fn emit_seq(out: &mut String, m: &Module) {
    if hw_regs(m).next().is_none() {
        return;
    }
    let _ = writeln!(out, "    // Sequential state ({} domain)", m.clock_domain);
    let _ = writeln!(out, "    always_ff @(posedge clk or negedge rst_n) begin");
    let _ = writeln!(out, "        if (!rst_n) begin");
    for r in hw_regs(m) {
        let _ = writeln!(
            out,
            "            {} <= {}'h{:x};",
            r.name,
            r.ty.width(),
            r.reset
        );
    }
    let _ = writeln!(out, "        end else begin");
    for r in hw_regs(m) {
        let _ = writeln!(out, "            {} <= {}_next;", r.name, r.name);
    }
    let _ = writeln!(out, "        end");
    let _ = writeln!(out, "    end");
}

/// Render a typed expression as a fully-parenthesized SystemVerilog expression.
fn expr(e: &Expr) -> String {
    match &e.kind {
        // Literals are emitted as sized hex bit-patterns; the surrounding
        // context (net signedness / `$signed`) carries the interpretation.
        ExprKind::Literal(v) => format!("{}'h{v:x}", e.ty.width()),
        ExprKind::Ident(name) => name.clone(),
        ExprKind::Unary { op, expr: inner } => match op {
            UnaryOp::Not => format!("(!{})", expr(inner)),
            UnaryOp::BitNot => format!("(~{})", expr(inner)),
            // `Neg` requires a signed operand (enforced by the checker).
            UnaryOp::Neg => format!("(-$signed({}))", expr(inner)),
        },
        ExprKind::Binary { op, lhs, rhs } => {
            if op.is_comparison() {
                format!("({} {} {})", signed(lhs), bin_sym(*op), signed(rhs))
            } else if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                // Extend both operands to the result width, then operate;
                // mirrors `eval::ext` + `wrapping_{add,sub,mul}`.
                let w = e.ty.width();
                format!("({} {} {})", ext(lhs, w), bin_sym(*op), ext(rhs, w))
            } else {
                // Bitwise: equal-width operands.
                format!("({} {} {})", expr(lhs), bin_sym(*op), expr(rhs))
            }
        }
        ExprKind::If { cond, then, els } => {
            format!("({} ? {} : {})", expr(cond), expr(then), expr(els))
        }
        ExprKind::Cast { expr: inner, mode } => match mode {
            CastMode::Sat => sat(inner, e.ty),
            _ => resize(inner, e.ty),
        },
        // Left shift by `amount`, widening: concatenate `amount` low zero bits.
        ExprKind::Shl {
            expr: inner,
            amount,
        } => {
            if *amount == 0 {
                expr(inner)
            } else {
                format!("{{{}, {amount}'b0}}", expr(inner))
            }
        }
        // Right shift by `amount`, narrowing: arithmetic for signed, then keep
        // the low result bits via a size cast.
        ExprKind::Shr {
            expr: inner,
            amount,
        } => {
            let w = e.ty.width();
            if inner.ty.is_signed() {
                format!("{w}'($signed({}) >>> {amount})", expr(inner))
            } else {
                format!("{w}'({} >> {amount})", expr(inner))
            }
        }
        // Variable (barrel) shift, same width: a native shift by a runtime
        // amount, kept to `w` bits (a shift `>= w` yields 0 / the sign fill).
        // Arithmetic (`>>>`) for a signed right shift, else logical.
        ExprKind::VarShift {
            expr: inner,
            amount,
            left,
        } => {
            let w = e.ty.width();
            if *left {
                format!("{w}'({} << {})", expr(inner), expr(amount))
            } else if inner.ty.is_signed() {
                format!("{w}'($signed({}) >>> {})", expr(inner), expr(amount))
            } else {
                format!("{w}'({} >> {})", expr(inner), expr(amount))
            }
        }
    }
}

/// Extend operand `e` to `w` bits: sign-extend a signed operand, zero-extend an
/// unsigned one. A no-op when `e` is already `w` bits wide (the legacy path).
fn ext(e: &Expr, w: u32) -> String {
    if e.ty.width() == w {
        expr(e)
    } else if e.ty.is_signed() {
        format!("{w}'($signed({}))", expr(e))
    } else {
        format!("{w}'({})", expr(e))
    }
}

/// Resize `inner` to the cast's target type: truncate when narrowing,
/// sign/zero-extend when widening (per the source signedness).
fn resize(inner: &Expr, to: Type) -> String {
    let w = to.width();
    let sw = inner.ty.width();
    if w == sw {
        expr(inner)
    } else if w < sw {
        format!("{w}'({})", expr(inner))
    } else if inner.ty.is_signed() {
        format!("{w}'($signed({}))", expr(inner))
    } else {
        format!("{w}'({})", expr(inner))
    }
}

/// Saturate `inner` (wider than `to`) into `to`'s range, then keep the low bits.
/// Comparisons are at the source width; bounds/results at the target width.
fn sat(inner: &Expr, to: Type) -> String {
    let w = to.width();
    let sw = inner.ty.width();
    let i = expr(inner);
    if to.is_signed() {
        let hi = (1i128 << (w - 1)) - 1;
        let lo = -(1i128 << (w - 1));
        format!(
            "(($signed({i}) > {}) ? {} : (($signed({i}) < {}) ? {} : {w}'({i})))",
            signed_lit(hi, sw),
            signed_lit(hi, w),
            signed_lit(lo, sw),
            signed_lit(lo, w)
        )
    } else {
        let hi = (1u128 << w) - 1;
        format!("(({i} > {sw}'d{hi}) ? {w}'d{hi} : {w}'({i}))")
    }
}

/// Wrap `e` in `$signed(...)` when its type is signed (for comparisons).
fn signed(e: &Expr) -> String {
    if e.ty.is_signed() {
        format!("$signed({})", expr(e))
    } else {
        expr(e)
    }
}

/// A signed decimal literal of `w` bits (SV writes negatives as `-w'sdN`).
fn signed_lit(v: i128, w: u32) -> String {
    if v >= 0 {
        format!("{w}'sd{v}")
    } else {
        format!("-{w}'sd{}", -v)
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::And => "&",
        BinOp::Or => "|",
        BinOp::Xor => "^",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        // Shifts emit `ir::ExprKind::Shl`/`Shr` and concat emits `Shl` + `Or`,
        // never a `Binary` op — see `eval::apply_binary` (DESIGN §6.13 / §9.10).
        BinOp::Shl | BinOp::Shr | BinOp::Concat => {
            unreachable!("shift/concat emit ExprKind nodes, not a Binary op")
        }
    }
}
