//! FLT1b silicon fault latch (DESIGN §6.12 / §9.9).
//!
//! A gated IR-construction pass: when `--faults=on`, synthesize the silicon fault
//! channel for an elaborated [`ir::Module`] *entirely* from the sim-only
//! [`ir::Assert`] sites FLT0 already collected — reusing the existing register /
//! output-port / combinational machinery, so **no** backend (sim, Verilog,
//! Verilator harness, oracle) needs a single new line. That is the "one IR, two
//! lowerings" property doing the work: the sticky bitmap, the priority encoder,
//! and the sideband are all ordinary IR that both backends already emit, so the
//! sim and the Verilog agree on the fault code bit-for-bit by construction.
//!
//! **What is synthesized** (for `N` assert sites, codes `1..=N`, `0` = no fault):
//!
//! - a sticky **bitmap register** `__fault_bits : uN` (resets empty), with
//!   `__fault_bits <= __fault_bits | mask`, where `mask` bit `i` is `!cond_i` — the
//!   *fired* bit of site `i` (an assert "fires" when its predicate reads false).
//!   Sticky (monotonic): a fired bit never clears (DESIGN §6.4 boundary). The
//!   bitmap is multi-hot (several sites may fire in one cycle); the priority
//!   encoder resolves that to a single reported code.
//! - a **priority-encoded** `fault_code` output (`uK`, `K = clog2(N+1)` — enough to
//!   hold the max code `N`), read from the *registered* bitmap: the lowest set
//!   bit's code, else `0`. Lowest-code-wins
//!   is pipeline-invariant (the encode is over the monotonic accumulated set), so
//!   the aggregation may later be registered as a pure physical optimization
//!   (the don't-care fault latency, §6.12) with no semantic change.
//!
//! The code reaches only the `fault_code` **sideband output** — never the datapath
//! (the observation-only rule) — so `--faults=off` (this pass simply not run) is
//! provably functional-neutral: the module is byte-identical to its FLT0 form.
//!
//! The `asserts` vector is left intact, so the sim still records FLT0 failures
//! (the diagnostic) and [`fault_table_json`] can emit the offline `code → backtrace`
//! table (site `i` ↦ code `i + 1`).

use sinter_syntax::ast::{BinOp, Type, UnaryOp};

use crate::error::{mask, CompileError};
use crate::ir::{self, ExprKind};

/// Whether the silicon fault latch is emitted (DESIGN §6.12). Read once at
/// `ir::Design` construction; `Off` folds the whole fault channel away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FaultMode {
    /// No fault hardware — the module is byte-identical to its assert-free form.
    #[default]
    Off,
    /// Emit the sticky bitmap + priority-encoded `fault_code` sideband.
    On,
}

/// The register holding the sticky one-hot fault bitmap.
const BITS: &str = "__fault_bits";
/// The priority-encoded fault-code sideband output port.
const CODE: &str = "fault_code";
/// The maximum number of assert sites (the bitmap is one scalar register).
const MAX_SITES: usize = 64;

fn sem(msg: impl Into<String>) -> CompileError {
    CompileError::semantic("faults", sinter_syntax::ast::Pos::default(), msg)
}

/// Bits needed to hold the value `n` (`0` for `n == 0`): the `fault_code` width.
fn code_width(n: usize) -> u32 {
    if n == 0 {
        0
    } else {
        u64::BITS - (n as u64).leading_zeros()
    }
}

/// Whether an IR expression contains a multiply anywhere (DESIGN §6.12, FLT1b):
/// a `*` in a fault condition is rejected (see `apply`), so the fault channel
/// never lifts a multiply into silicon.
fn expr_has_mul(e: &ir::Expr) -> bool {
    match &e.kind {
        ExprKind::Binary { op, lhs, rhs } => {
            *op == BinOp::Mul || expr_has_mul(lhs) || expr_has_mul(rhs)
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Shl { expr, .. }
        | ExprKind::Shr { expr, .. } => expr_has_mul(expr),
        ExprKind::VarShift { expr, amount, .. } => expr_has_mul(expr) || expr_has_mul(amount),
        ExprKind::If { cond, then, els } => {
            expr_has_mul(cond) || expr_has_mul(then) || expr_has_mul(els)
        }
        ExprKind::Literal(_) | ExprKind::Ident(_) => false,
    }
}

fn expr(kind: ExprKind, ty: Type) -> ir::Expr {
    ir::Expr { kind, ty }
}

fn lit(v: u64, w: u32) -> ir::Expr {
    expr(ExprKind::Literal(mask(v, w)), Type::Uint(w))
}

fn ident(name: &str, ty: Type) -> ir::Expr {
    expr(ExprKind::Ident(name.to_string()), ty)
}

fn binary(op: BinOp, lhs: ir::Expr, rhs: ir::Expr, ty: Type) -> ir::Expr {
    expr(
        ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        ty,
    )
}

fn ite(cond: ir::Expr, then: ir::Expr, els: ir::Expr, ty: Type) -> ir::Expr {
    expr(
        ExprKind::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        },
        ty,
    )
}

/// Synthesize the silicon fault latch for `module` (DESIGN §6.12 / §9.9, FLT1b).
///
/// A no-op when the module has no assert sites. Assumes `--faults=on`; with `off`
/// this pass is simply not called and the module keeps its FLT0 (sim-only) shape.
///
/// # Errors
/// Returns an error if there are more than 64 assert sites (the bitmap is a
/// single scalar register), if an assert condition contains a multiply (not
/// lifted into the fault channel), or if a synthesized name collides with a user
/// signal.
pub fn apply(module: &mut ir::Module) -> Result<(), CompileError> {
    let n = module.asserts.len();
    if n == 0 {
        return Ok(());
    }
    if n > MAX_SITES {
        return Err(sem(format!(
            "a `--faults=on` build supports at most {MAX_SITES} assert sites, found {n}"
        )));
    }
    // Guard against clobbering a user signal (regs / ports / wires / memories).
    let name_taken = |name: &str| {
        module.regs.iter().any(|r| r.name == name)
            || module.ports.iter().any(|p| p.name == name)
            || module.lets.iter().any(|l| l.name == name)
            || module.mems.iter().any(|m| m.name == name)
    };
    if name_taken(BITS) || name_taken(CODE) {
        return Err(sem(format!(
            "the fault latch needs the names `{BITS}` / `{CODE}`, but one is already used"
        )));
    }
    // FLT1b does not lift a multiply into the fault channel: a multiply with a
    // non-leaf operand trips Verilator's `WIDTHEXPAND` (it bypasses the datapath
    // multiply-CSE, which never visits `asserts`), and it would instantiate an
    // extra multiplier in the sideband. Reject it with a scope note rather than
    // silently emit possibly-lint-failing hardware (a future tier can CSE it).
    for a in &module.asserts {
        if expr_has_mul(&a.cond) {
            return Err(sem(format!(
                "an `assert!` condition may not contain `*` under `--faults=on` (FLT1b): at {}",
                a.loc
            )));
        }
    }

    let nbits = n as u32;
    let cw = code_width(n);
    let bits_ty = Type::Uint(nbits);
    let code_ty = Type::Uint(cw);

    // mask = OR_i (if !cond_i then (1 << i) else 0) — the one-hot of this cycle's
    // fired sites. Built as `if`-selected uN literals (no shift node needed).
    let mut mask_expr: Option<ir::Expr> = None;
    for (i, a) in module.asserts.iter().enumerate() {
        let fired = expr(
            ExprKind::Unary {
                op: UnaryOp::Not,
                expr: Box::new(a.cond.clone()),
            },
            Type::Uint(1),
        );
        let placed = ite(fired, lit(1u64 << i, nbits), lit(0, nbits), bits_ty);
        mask_expr = Some(match mask_expr {
            None => placed,
            Some(acc) => binary(BinOp::Or, acc, placed, bits_ty),
        });
    }
    let mask_expr = mask_expr.expect("n >= 1");

    // __fault_bits <= __fault_bits | mask  (sticky OR).
    let next = binary(BinOp::Or, ident(BITS, bits_ty), mask_expr, bits_ty);
    module.regs.push(ir::Reg {
        name: BITS.to_string(),
        ty: bits_ty,
        reset: 0,
    });
    module.reg_updates.push((BITS.to_string(), next));

    // fault_code = priority-encode(__fault_bits): lowest set bit's code, else 0.
    // Fold from the highest bit inward so bit 0 (code 1) ends up the outermost
    // test — lowest code wins.
    let mut code = lit(0, cw);
    for i in (0..n).rev() {
        let bit_set = binary(
            BinOp::Ne,
            binary(
                BinOp::And,
                ident(BITS, bits_ty),
                lit(1u64 << i, nbits),
                bits_ty,
            ),
            lit(0, nbits),
            Type::Uint(1),
        );
        code = ite(bit_set, lit((i + 1) as u64, cw), code, code_ty);
    }
    module.ports.push(ir::Port {
        dir: sinter_syntax::ast::Dir::Out,
        name: CODE.to_string(),
        ty: code_ty,
    });
    module.out_drives.push((CODE.to_string(), code));

    Ok(())
}

/// The offline fault table for `module`: a JSON array mapping each fault `code`
/// (site index + 1) to its message and static backtrace (DESIGN §6.12, FLT1b).
/// The silicon carries the number; this table carries the meaning.
#[must_use]
pub fn fault_table_json(module: &ir::Module) -> String {
    let mut out = String::from("[\n");
    for (i, a) in module.asserts.iter().enumerate() {
        out.push_str(&format!(
            "  {{ \"code\": {}, \"message\": {}, \"backtrace\": {} }}{}\n",
            i + 1,
            json_string(&a.msg),
            json_string(&a.loc),
            if i + 1 == module.asserts.len() {
                ""
            } else {
                ","
            }
        ));
    }
    out.push_str("]\n");
    out
}

/// Escape a string as a JSON string literal (quotes + backslashes + controls).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
