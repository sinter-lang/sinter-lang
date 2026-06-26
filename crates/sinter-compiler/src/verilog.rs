//! SystemVerilog backend.
//!
//! Emits portable, Yosys-synthesizable, Verilator-lint-clean SystemVerilog that
//! matches the `eval` semantics bit-for-bit. Conforms to the `claude-skill-verilog`
//! standard: `` `default_nettype none ``, explicit widths, `always_ff` carrying
//! only simple non-blocking assignments (next-state computed in `always_comb`),
//! named ports, and one statement per line.

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

/// The signed/range prefix for a net of `kind` width, e.g. `signed [15:0]`.
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
/// Literals carry explicit width and signedness so Verilog's context rules
/// cannot widen them away from the `eval` semantics.
fn expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Literal(v) => {
            let w = e.ty.width();
            if e.ty.is_signed() {
                format!("{w}'sd{v}")
            } else {
                format!("{w}'d{v}")
            }
        }
        ExprKind::Ident(name) => name.clone(),
        ExprKind::Unary { op, expr: inner } => {
            let sym = match op {
                UnaryOp::Not => "!",
                UnaryOp::BitNot => "~",
                UnaryOp::Neg => "-",
            };
            format!("({sym}{})", expr(inner))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            // Width-inferred `+`/`-` widen the result; size-cast the operands up
            // to the result width so the Verilog arithmetic context matches the
            // `eval` semantics (a no-op when operands are already that wide, as
            // on the legacy same-width path).
            if matches!(op, BinOp::Add | BinOp::Sub) {
                let w = e.ty.width();
                format!("({} {} {})", widen(lhs, w), bin_sym(*op), widen(rhs, w))
            } else {
                format!("({} {} {})", expr(lhs), bin_sym(*op), expr(rhs))
            }
        }
        ExprKind::If { cond, then, els } => {
            format!("({} ? {} : {})", expr(cond), expr(then), expr(els))
        }
        ExprKind::Cast { expr: inner, mode } => match mode {
            // Reinterpret the low `w` bits via a SystemVerilog size cast.
            CastMode::Wrap | CastMode::Trunc | CastMode::Round => {
                format!("{}'({})", e.ty.width(), expr(inner))
            }
            CastMode::Sat => sat_expr(inner, e.ty),
        },
    }
}

/// Size-cast an operand up to `w` bits (zero-extend) when narrower; emit it
/// unchanged otherwise. Signed widening is deferred (F3+); the functional
/// subset lowered today is unsigned.
fn widen(operand: &Expr, w: u32) -> String {
    if operand.ty.width() < w && !operand.ty.is_signed() {
        format!("{w}'({})", expr(operand))
    } else {
        expr(operand)
    }
}

/// Emit a saturating cast of `inner` into scalar type `to` (DESIGN §6.3). Used
/// only once `sat` casts are lowered (F4); the `eval` side is `apply_cast`.
fn sat_expr(inner: &Expr, to: Type) -> String {
    let w = to.width();
    let i = expr(inner);
    if to.is_signed() {
        let hi = (1i128 << (w - 1)) - 1;
        let lo_abs = 1u128 << (w - 1);
        format!(
            "(($signed({i}) > {w}'sd{hi}) ? {w}'sd{hi} : \
             (($signed({i}) < -{w}'sd{lo_abs}) ? -{w}'sd{lo_abs} : {w}'({i})))"
        )
    } else {
        let hi = (1u128 << w) - 1;
        format!("(({i} > {hi}) ? {w}'d{hi} : {w}'({i}))")
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
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
