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
//! from `eval`. (A same-width operand gets no extension.)

use std::fmt::Write as _;

use sinter_syntax::ast::{BinOp, CastMode, Type, UnaryOp};

use crate::ir::{self, Expr, ExprKind, Module};

/// Emit the SystemVerilog text for a module. The caller writes it to
/// `<module.name>.sv` (filename matches the module name, per the skill).
#[must_use]
pub fn emit_module(m: &Module) -> String {
    let hoisted = hoist_zero_extensions(&without_model_mems(m));
    let m = &hoisted;
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
    emit_reset_syncs(&mut out, m);
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

/// A copy of `m` without the memories that exist only in an instance's model
/// (a `#[module]` holding a `mem`), or their read and write ports: the
/// instantiated module has them instead.
fn without_model_mems(m: &Module) -> Module {
    let mut m = m.clone();
    let model: std::collections::HashSet<String> = m
        .instances
        .iter()
        .flat_map(|i| i.model_mems.iter().cloned())
        .collect();
    m.mems.retain(|x| !model.contains(&x.name));
    m.mem_reads.retain(|r| !model.contains(&r.mem));
    m.mem_writes.retain(|w| !model.contains(&w.mem));
    m
}

/// A copy of `m` in which every cast that zero-extends an unsigned value into
/// a wider signed type is its own wire (`__zx<k>`).
///
/// This works around a Verilator (5.045) optimizer bug: an inline
/// `$signed(N'(v))`, with `v` a narrower unsigned variable, is sign-extended
/// or arithmetic-shifted from `v`'s own top bit (`22'($signed(20'(v)))` with
/// `v = 9'h1f0` gives `3ffff0`, not `0001f0`). Read through a named signed
/// wire, the same value is handled correctly. The semantics are unchanged.
fn hoist_zero_extensions(m: &Module) -> Module {
    fn walk(e: &mut Expr, new: &mut Vec<ir::Let>, taken: &std::collections::HashSet<String>) {
        match &mut e.kind {
            ExprKind::Literal(_) | ExprKind::Ident(_) => {}
            ExprKind::Unary { expr: x, .. }
            | ExprKind::Cast { expr: x, .. }
            | ExprKind::Shl { expr: x, .. }
            | ExprKind::Shr { expr: x, .. } => walk(x, new, taken),
            ExprKind::Binary { lhs, rhs, .. } => {
                walk(lhs, new, taken);
                walk(rhs, new, taken);
            }
            ExprKind::VarShift {
                expr: x, amount, ..
            } => {
                walk(x, new, taken);
                walk(amount, new, taken);
            }
            ExprKind::If { cond, then, els } => {
                walk(cond, new, taken);
                walk(then, new, taken);
                walk(els, new, taken);
            }
        }
        let zero_extends = matches!(&e.kind, ExprKind::Cast { expr: x, mode }
            if *mode != CastMode::Sat
                && !x.ty.is_signed()
                && e.ty.is_signed()
                && x.ty.width() < e.ty.width());
        if zero_extends {
            let name = (new.len()..)
                .map(|k| format!("__zx{k}"))
                .find(|n| !taken.contains(n))
                .unwrap_or_default();
            let value = std::mem::replace(
                e,
                Expr {
                    kind: ExprKind::Ident(name.clone()),
                    ty: e.ty,
                },
            );
            new.push(ir::Let {
                name,
                ty: value.ty,
                frac_bits: 0,
                value,
            });
        }
    }
    let mut m = m.clone();
    let taken: std::collections::HashSet<String> = m
        .ports
        .iter()
        .map(|p| p.name.clone())
        .chain(m.regs.iter().map(|r| r.name.clone()))
        .chain(m.lets.iter().map(|l| l.name.clone()))
        .chain(m.mems.iter().map(|x| x.name.clone()))
        .collect();
    let mut new = Vec::new();
    // Each hoisted wire goes just before the wire that reads it, keeping the
    // wires in dependency order; the rest go after every wire.
    let mut lets = Vec::new();
    for mut l in std::mem::take(&mut m.lets) {
        let before = new.len();
        walk(&mut l.value, &mut new, &taken);
        lets.extend(new[before..].iter().cloned());
        lets.push(l);
    }
    let lets_done = new.len();
    for (_, e) in &mut m.out_drives {
        walk(e, &mut new, &taken);
    }
    for (_, e) in &mut m.reg_updates {
        walk(e, &mut new, &taken);
    }
    for r in &mut m.mem_reads {
        walk(&mut r.addr, &mut new, &taken);
    }
    for w in &mut m.mem_writes {
        walk(&mut w.we, &mut new, &taken);
        walk(&mut w.addr, &mut new, &taken);
        walk(&mut w.data, &mut new, &taken);
    }
    lets.extend(new[lets_done..].iter().cloned());
    m.lets = lets;
    m
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
    if m.is_multi_clock() {
        for d in &m.domains {
            let _ = writeln!(
                out,
                "    input  wire         clk_{0},  // {0} clock (period {1})",
                d.name, d.period
            );
        }
    } else {
        let _ = writeln!(
            out,
            "    input  wire         clk,    // {} clock",
            m.clock_domain
        );
    }
    // With no other ports, `rst_n` is last and takes no comma.
    let comma = if m.ports.is_empty() { " " } else { "," };
    let _ = writeln!(
        out,
        "    input  wire         rst_n{comma}  // active-low asynchronous reset"
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

/// The wires the emitted Verilog needs: those reachable from an output, a
/// register's or memory's next state, or a foreign instance's input.
///
/// The simulator evaluates every wire, but a `let` nothing reads (or one read
/// only by a sim-only `assert!`) would be an unused net, which Verilator's
/// lint rejects. Wires of a `#[verilog]` model never appear (DESIGN §6.15),
/// and instance outputs are driven by their instance, so both are skipped.
fn live_wires(m: &Module) -> std::collections::HashSet<&str> {
    fn idents<'a>(e: &'a Expr, out: &mut Vec<&'a str>) {
        match &e.kind {
            ExprKind::Ident(n) => out.push(n),
            ExprKind::Literal(_) => {}
            ExprKind::Unary { expr: x, .. }
            | ExprKind::Cast { expr: x, .. }
            | ExprKind::Shl { expr: x, .. }
            | ExprKind::Shr { expr: x, .. } => idents(x, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                idents(lhs, out);
                idents(rhs, out);
            }
            ExprKind::VarShift {
                expr: x, amount, ..
            } => {
                idents(x, out);
                idents(amount, out);
            }
            ExprKind::If { cond, then, els } => {
                idents(cond, out);
                idents(then, out);
                idents(els, out);
            }
        }
    }
    let mut refs: Vec<&str> = Vec::new();
    for (_, e) in &m.out_drives {
        idents(e, &mut refs);
    }
    for (r, e) in &m.reg_updates {
        if !m.is_model_only(r) {
            idents(e, &mut refs);
        }
    }
    for r in &m.mem_reads {
        idents(&r.addr, &mut refs);
    }
    for w in &m.mem_writes {
        idents(&w.we, &mut refs);
        idents(&w.addr, &mut refs);
        idents(&w.data, &mut refs);
    }
    for inst in &m.instances {
        refs.extend(inst.inputs.iter().map(|(_, w)| w.as_str()));
        refs.extend(inst.outputs.iter().map(|(_, w)| w.as_str()));
    }
    let mut live: std::collections::HashSet<&str> = refs.into_iter().collect();
    // Wires are topologically ordered, so one backward pass reaches every
    // wire a live wire reads.
    for l in m.lets.iter().rev() {
        if live.contains(l.name.as_str())
            && !m.is_model_only(&l.name)
            && m.instance_output(&l.name).is_none()
        {
            let mut more = Vec::new();
            idents(&l.value, &mut more);
            live.extend(more);
        }
    }
    live.retain(|n| !m.is_model_only(n));
    live
}

/// The clock net of domain `d`: `clk` with one domain, else `clk_<d>`.
fn clk_net(m: &Module, d: usize) -> String {
    if m.is_multi_clock() {
        format!("clk_{}", m.domains[d].name)
    } else {
        "clk".to_string()
    }
}

/// The active-low reset of domain `d`: `rst_n` with one domain, else that
/// domain's synchronized reset `rst_<d>_n` (see [`emit_reset_syncs`]).
fn rst_net(m: &Module, d: usize) -> String {
    if m.is_multi_clock() {
        format!("rst_{}_n", m.domains[d].name)
    } else {
        "rst_n".to_string()
    }
}

/// With several clocks, a reset synchronizer per domain (DESIGN §6.7): the
/// external `rst_n` asserts every domain's reset at once, and each domain
/// releases it two of its own clock edges later, so its registers leave reset
/// synchronously.
fn emit_reset_syncs(out: &mut String, m: &Module) {
    if !m.is_multi_clock() {
        return;
    }
    for (d, dom) in m.domains.iter().enumerate() {
        let meta = format!("rst_{}_meta", dom.name);
        let rst = rst_net(m, d);
        let clk = clk_net(m, d);
        let _ = writeln!(
            out,
            "    // Reset synchronizer ({} domain): asynchronous assert, synchronous deassert",
            dom.name
        );
        let _ = writeln!(out, "    logic [0:0] {meta};");
        let _ = writeln!(out, "    logic [0:0] {rst};");
        let _ = writeln!(out, "    always_ff @(posedge {clk} or negedge rst_n) begin");
        let _ = writeln!(out, "        if (!rst_n) begin");
        let _ = writeln!(out, "            {meta} <= 1'b0;");
        let _ = writeln!(out, "            {rst} <= 1'b0;");
        let _ = writeln!(out, "        end else begin");
        let _ = writeln!(out, "            {meta} <= 1'b1;");
        let _ = writeln!(out, "            {rst} <= {meta};");
        let _ = writeln!(out, "        end");
        let _ = writeln!(out, "    end");
    }
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
    let live = live_wires(m);
    if m.lets.iter().any(|l| live.contains(l.name.as_str())) {
        let _ = writeln!(out, "    // Combinational wires");
        for l in m.lets.iter().filter(|l| live.contains(l.name.as_str())) {
            let _ = writeln!(
                out,
                "    logic {} {};{}",
                net_type(l.ty),
                l.name,
                q_comment(l.ty, l.frac_bits)
            );
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
    // Model-only and dead wires don't exist in Verilog, and an instance output
    // is driven by its instance rather than by its model's logic.
    let live = live_wires(m);
    for l in &m.lets {
        if live.contains(l.name.as_str()) && m.instance_output(&l.name).is_none() {
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

/// A byte-enable write (MEM2, DESIGN §9.6): one `if` per lane in the write
/// process, the form Yosys maps onto a byte-enable block RAM. The enable and
/// data are first named (`__<mem>_we` / `__<mem>_wdata`) because a bit- or
/// part-select of an arbitrary expression is not legal SystemVerilog.
fn emit_masked_write(out: &mut String, m: &Module, w: &crate::ir::MemWrite, lanes: u32) {
    let we = format!("__{}_we", w.mem);
    let data = format!("__{}_wdata", w.mem);
    let _ = writeln!(
        out,
        "    // Memory write port: {} ({lanes} byte lanes)",
        w.mem
    );
    let _ = writeln!(out, "    logic {} {we};", net_type(w.we.ty));
    let _ = writeln!(out, "    logic {} {data};", net_type(w.data.ty));
    let _ = writeln!(out, "    assign {we} = {};", expr(&w.we));
    let _ = writeln!(out, "    assign {data} = {};", expr(&w.data));
    let _ = writeln!(
        out,
        "    always_ff @(posedge {}) begin",
        clk_net(m, w.domain)
    );
    for i in 0..lanes {
        let (hi, lo) = (8 * i + 7, 8 * i);
        let _ = writeln!(out, "        if ({we}[{i}]) begin");
        let _ = writeln!(
            out,
            "            {}[{}][{hi}:{lo}] <= {data}[{hi}:{lo}];",
            w.mem,
            expr(&w.addr)
        );
        let _ = writeln!(out, "        end");
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
        let lanes = w.we.ty.width();
        if lanes > 1 {
            emit_masked_write(out, m, w, lanes);
            continue;
        }
        let _ = writeln!(out, "    // Memory write port: {}", w.mem);
        let _ = writeln!(
            out,
            "    always_ff @(posedge {}) begin",
            clk_net(m, w.domain)
        );
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
        if inst.generated {
            let _ = writeln!(out, "    // Sinter module `{}`", inst.module);
        } else {
            let _ = writeln!(
                out,
                "    // Foreign Verilog module `{}`, modelled in simulation by `{}`",
                inst.module, inst.model
            );
        }
        let (clk_wire, rst_wire) = (clk_net(m, inst.domain), rst_net(m, inst.domain));
        let mut conns: Vec<(&str, &str)> = Vec::new();
        if let Some(clk) = &inst.clk {
            conns.push((clk, &clk_wire));
        }
        if let Some(rst_n) = &inst.rst_n {
            conns.push((rst_n, &rst_wire));
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
    for d in 0..m.domains.len().max(1) {
        let regs: Vec<_> = hw_regs(m).filter(|r| r.domain == d).collect();
        if regs.is_empty() {
            continue;
        }
        let name = m
            .domains
            .get(d)
            .map_or(m.clock_domain.as_str(), |dom| dom.name.as_str());
        let (clk, rst) = (clk_net(m, d), rst_net(m, d));
        let _ = writeln!(out, "    // Sequential state ({name} domain)");
        let _ = writeln!(out, "    always_ff @(posedge {clk} or negedge {rst}) begin");
        let _ = writeln!(out, "        if (!{rst}) begin");
        for r in &regs {
            let _ = writeln!(
                out,
                "            {} <= {}'h{:x};",
                r.name,
                r.ty.width(),
                r.reset
            );
        }
        let _ = writeln!(out, "        end else begin");
        for r in &regs {
            let _ = writeln!(out, "            {} <= {}_next;", r.name, r.name);
        }
        let _ = writeln!(out, "        end");
        let _ = writeln!(out, "    end");
    }
}

/// Render a typed expression as a fully-parenthesized SystemVerilog expression.
fn expr(e: &Expr) -> String {
    match &e.kind {
        // Literals are emitted as sized hex bit-patterns; the surrounding
        // context (net signedness / `$signed`) carries the interpretation.
        ExprKind::Literal(v) => format!("{}'h{v:x}", e.ty.width()),
        ExprKind::Ident(name) => name.clone(),
        ExprKind::Unary { op, expr: inner } => match op {
            UnaryOp::Not => format!("(!{})", operand(inner)),
            UnaryOp::BitNot => format!("(~{})", operand(inner)),
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
            // With the sign bit known to be 0, an arithmetic shift is a
            // logical one (and dodges the Verilator bug noted at
            // `hoist_zero_extensions`).
            let w = e.ty.width();
            if arith_shift(inner) {
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
            } else if arith_shift(inner) {
                format!("{w}'($signed({}) >>> {})", expr(inner), expr(amount))
            } else {
                format!("{w}'({} >> {})", expr(inner), expr(amount))
            }
        }
    }
}

/// A unary operator's operand. One that starts with a size cast is
/// parenthesized: Yosys reads `~8'(x)` as a cast of size `~8`.
fn operand(e: &Expr) -> String {
    let s = expr(e);
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("({s})")
    } else {
        s
    }
}

/// Whether the SystemVerilog that [`expr`] emits for `e` is signed. It can
/// differ from `e`'s own type: a size cast `N'(x)` keeps `x`'s signedness, and
/// a concatenation is always unsigned. Where that matters (comparisons and
/// zero-extension), [`unsigned`] corrects it.
fn sv_signed(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) => false,
        ExprKind::Ident(_) => e.ty.is_signed(),
        ExprKind::Unary { op, expr: inner } => match op {
            UnaryOp::Not => false,
            UnaryOp::BitNot => sv_signed(inner),
            UnaryOp::Neg => true,
        },
        ExprKind::Binary { op, lhs, rhs } => {
            if op.is_comparison() {
                false
            } else if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul) {
                let w = e.ty.width();
                ext_signed(lhs, w) && ext_signed(rhs, w)
            } else {
                sv_signed(lhs) && sv_signed(rhs)
            }
        }
        ExprKind::If { then, els, .. } => sv_signed(then) && sv_signed(els),
        ExprKind::Cast { expr: inner, mode } => match mode {
            CastMode::Sat => e.ty.is_signed() && sv_signed(inner),
            _ => ext_signed(inner, e.ty.width()),
        },
        ExprKind::Shl {
            expr: inner,
            amount,
        } => *amount == 0 && sv_signed(inner),
        ExprKind::Shr { expr: inner, .. } => arith_shift(inner) || sv_signed(inner),
        ExprKind::VarShift {
            expr: inner, left, ..
        } => (!*left && arith_shift(inner)) || sv_signed(inner),
    }
}

/// Whether a right shift of `e` is emitted as arithmetic (`>>>`): `e` is
/// signed and its sign bit isn't known to be 0.
fn arith_shift(e: &Expr) -> bool {
    e.ty.is_signed() && !zero_msb(e)
}

/// Whether [`ext`]`(e, w)` is signed SystemVerilog.
fn ext_signed(e: &Expr, w: u32) -> bool {
    if e.ty.width() >= w {
        sv_signed(e)
    } else {
        e.ty.is_signed()
    }
}

/// Whether [`expr`]`(e)` is a primary (a name, literal, size cast, or
/// concatenation), whose width its context can't change.
///
/// Anything else, widened by a size cast, is evaluated at the wider width:
/// SystemVerilog sizes an operator's operands to its context, so `16'(a - b)`
/// subtracts at 16 bits, and `~x` and `-x` likewise compute high bits that
/// `e`'s own width drops (and Verilator warns about a widened `?:`).
fn atomic(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Ident(_) => true,
        ExprKind::Unary { .. } | ExprKind::Binary { .. } | ExprKind::If { .. } => false,
        ExprKind::Cast { expr: inner, mode } => match mode {
            CastMode::Sat => false,
            _ => inner.ty.width() != e.ty.width() || atomic(inner),
        },
        ExprKind::Shl {
            expr: inner,
            amount,
        } => *amount != 0 || atomic(inner),
        ExprKind::Shr { .. } | ExprKind::VarShift { .. } => true,
    }
}

/// `e` as unsigned SystemVerilog of its own width, safe to zero-extend.
fn unsigned(e: &Expr) -> String {
    if sv_signed(e) || !atomic(e) {
        format!("$unsigned({})", expr(e))
    } else {
        expr(e)
    }
}

/// Extend operand `e` to `w` bits: sign-extend a signed operand, zero-extend an
/// unsigned one. A no-op when `e` is already `w` bits wide.
fn ext(e: &Expr, w: u32) -> String {
    if e.ty.width() == w {
        expr(e)
    } else if e.ty.is_signed() {
        sign_extend(e, w)
    } else {
        format!("{w}'({})", unsigned(e))
    }
}

/// Sign-extend signed `e` to `w` bits. When `e`'s sign bit is known to be 0,
/// zero-extend instead: the value is the same, and it avoids a Verilator
/// (5.045) optimizer bug that sign-extends a zero-extended variable from the
/// variable's own top bit (`19'($signed(18'(v)))` with `v` a 4-bit 8 gives
/// -8, not 8).
fn sign_extend(e: &Expr, w: u32) -> String {
    if zero_msb(e) {
        format!("$signed({w}'({}))", unsigned(e))
    } else {
        format!("{w}'($signed({}))", expr(e))
    }
}

/// Whether `e`'s top bit is 0 by construction: a zero-extension, or a bitwise
/// combination or choice of such.
fn zero_msb(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(v) => (v >> (e.ty.width() - 1)) & 1 == 0,
        ExprKind::Cast { expr: inner, mode } if *mode != CastMode::Sat => {
            match inner.ty.width().cmp(&e.ty.width()) {
                std::cmp::Ordering::Less => !inner.ty.is_signed(),
                std::cmp::Ordering::Equal => zero_msb(inner),
                std::cmp::Ordering::Greater => false,
            }
        }
        ExprKind::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } => zero_msb(lhs) || zero_msb(rhs),
        ExprKind::Binary {
            op: BinOp::Or | BinOp::Xor,
            lhs,
            rhs,
        } => zero_msb(lhs) && zero_msb(rhs),
        ExprKind::If { then, els, .. } => zero_msb(then) && zero_msb(els),
        ExprKind::Shl {
            expr: inner,
            amount: 0,
        } => zero_msb(inner),
        _ => false,
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
        sign_extend(inner, w)
    } else {
        format!("{w}'({})", unsigned(inner))
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

/// A comparison operand: `$signed(...)` when its type is signed, and
/// unsigned SystemVerilog otherwise, so the comparison's signedness is the
/// operands' type's.
fn signed(e: &Expr) -> String {
    if e.ty.is_signed() {
        format!("$signed({})", expr(e))
    } else if sv_signed(e) {
        format!("$unsigned({})", expr(e))
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

/// SystemVerilog reserved keywords (IEEE 1800-2017 Annex B). A generated
/// net may not use one of these as its name.
const KEYWORDS: &[&str] = &[
    "accept_on",
    "alias",
    "always",
    "always_comb",
    "always_ff",
    "always_latch",
    "and",
    "assert",
    "assign",
    "assume",
    "automatic",
    "before",
    "begin",
    "bind",
    "bins",
    "binsof",
    "bit",
    "break",
    "buf",
    "bufif0",
    "bufif1",
    "byte",
    "case",
    "casex",
    "casez",
    "cell",
    "chandle",
    "checker",
    "class",
    "clocking",
    "cmos",
    "config",
    "const",
    "constraint",
    "context",
    "continue",
    "cover",
    "covergroup",
    "coverpoint",
    "cross",
    "deassign",
    "default",
    "defparam",
    "design",
    "disable",
    "dist",
    "do",
    "edge",
    "else",
    "end",
    "endcase",
    "endchecker",
    "endclass",
    "endclocking",
    "endconfig",
    "endfunction",
    "endgenerate",
    "endgroup",
    "endinterface",
    "endmodule",
    "endpackage",
    "endprimitive",
    "endprogram",
    "endproperty",
    "endsequence",
    "endspecify",
    "endtable",
    "endtask",
    "enum",
    "event",
    "eventually",
    "expect",
    "export",
    "extends",
    "extern",
    "final",
    "first_match",
    "for",
    "force",
    "foreach",
    "forever",
    "fork",
    "forkjoin",
    "function",
    "generate",
    "genvar",
    "global",
    "highz0",
    "highz1",
    "if",
    "iff",
    "ifnone",
    "ignore_bins",
    "illegal_bins",
    "implements",
    "implies",
    "import",
    "incdir",
    "include",
    "initial",
    "inout",
    "input",
    "inside",
    "instance",
    "int",
    "integer",
    "interconnect",
    "interface",
    "intersect",
    "join",
    "join_any",
    "join_none",
    "large",
    "let",
    "liblist",
    "library",
    "local",
    "localparam",
    "logic",
    "longint",
    "macromodule",
    "matches",
    "medium",
    "modport",
    "module",
    "nand",
    "negedge",
    "nettype",
    "new",
    "nexttime",
    "nmos",
    "nor",
    "noshowcancelled",
    "not",
    "notif0",
    "notif1",
    "null",
    "or",
    "output",
    "package",
    "packed",
    "parameter",
    "pmos",
    "posedge",
    "primitive",
    "priority",
    "program",
    "property",
    "protected",
    "pull0",
    "pull1",
    "pulldown",
    "pullup",
    "pulsestyle_ondetect",
    "pulsestyle_onevent",
    "pure",
    "rand",
    "randc",
    "randcase",
    "randsequence",
    "rcmos",
    "real",
    "realtime",
    "ref",
    "reg",
    "reject_on",
    "release",
    "repeat",
    "restrict",
    "return",
    "rnmos",
    "rpmos",
    "rtran",
    "rtranif0",
    "rtranif1",
    "s_always",
    "s_eventually",
    "s_nexttime",
    "s_until",
    "s_until_with",
    "scalared",
    "sequence",
    "shortint",
    "shortreal",
    "showcancelled",
    "signed",
    "small",
    "soft",
    "solve",
    "specify",
    "specparam",
    "static",
    "string",
    "strong",
    "strong0",
    "strong1",
    "struct",
    "super",
    "supply0",
    "supply1",
    "sync_accept_on",
    "sync_reject_on",
    "table",
    "tagged",
    "task",
    "this",
    "throughout",
    "time",
    "timeprecision",
    "timeunit",
    "tran",
    "tranif0",
    "tranif1",
    "tri",
    "tri0",
    "tri1",
    "triand",
    "trior",
    "trireg",
    "type",
    "typedef",
    "union",
    "unique",
    "unique0",
    "unsigned",
    "until",
    "until_with",
    "untyped",
    "use",
    "uwire",
    "var",
    "vectored",
    "virtual",
    "void",
    "wait",
    "wait_order",
    "wand",
    "weak",
    "weak0",
    "weak1",
    "while",
    "wildcard",
    "wire",
    "with",
    "within",
    "wor",
    "xnor",
    "xor",
];

/// Whether `name` is a SystemVerilog keyword, and so unusable as a net name.
#[must_use]
pub fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
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
