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
    emit_comb(&mut out, m);
    emit_seq(&mut out, m);

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
    let ports: Vec<String> = m
        .ports
        .iter()
        .map(|p| {
            let (dir, kw) = match p.dir {
                sinter_syntax::ast::Dir::In => ("input", "wire"),
                sinter_syntax::ast::Dir::Out => ("output", "logic"),
            };
            format!("    {dir:6} {kw:5} {} {}", net_type(p.ty), p.name)
        })
        .collect();
    let _ = writeln!(out, "{}", ports.join(",\n"));
    let _ = writeln!(out, ");");
}

fn emit_decls(out: &mut String, m: &Module) {
    if !m.regs.is_empty() {
        let _ = writeln!(out, "    // Registers");
        for r in &m.regs {
            let _ = writeln!(out, "    logic {} {};", net_type(r.ty), r.name);
            let _ = writeln!(out, "    logic {} {}_next;", net_type(r.ty), r.name);
        }
    }
    if !m.lets.is_empty() {
        let _ = writeln!(out, "    // Combinational wires");
        for l in &m.lets {
            let _ = writeln!(out, "    logic {} {};", net_type(l.ty), l.name);
        }
    }
}

fn emit_comb(out: &mut String, m: &Module) {
    let _ = writeln!(out, "    // Combinational logic");
    let _ = writeln!(out, "    always_comb begin");
    for l in &m.lets {
        let _ = writeln!(out, "        {} = {};", l.name, expr(&l.value));
    }
    for (name, e) in &m.reg_updates {
        let _ = writeln!(out, "        {name}_next = {};", expr(e));
    }
    for (name, e) in &m.out_drives {
        let _ = writeln!(out, "        {name} = {};", expr(e));
    }
    let _ = writeln!(out, "    end");
}

fn emit_seq(out: &mut String, m: &Module) {
    if m.regs.is_empty() {
        return;
    }
    let _ = writeln!(out, "    // Sequential state ({} domain)", m.clock_domain);
    let _ = writeln!(out, "    always_ff @(posedge clk or negedge rst_n) begin");
    let _ = writeln!(out, "        if (!rst_n) begin");
    for r in &m.regs {
        let _ = writeln!(
            out,
            "            {} <= {}'h{:x};",
            r.name,
            r.ty.width(),
            r.reset
        );
    }
    let _ = writeln!(out, "        end else begin");
    for r in &m.regs {
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
    }
}
