//! The single, authoritative evaluation semantics for Sinter expressions.
//!
//! Both the constant-folder (reset values) and the cycle-accurate simulator
//! call into these functions, and the Verilog backend is written to match them
//! bit-for-bit. Centralizing the operator semantics here is what keeps the two
//! lowerings in agreement (DESIGN §7.4).
//!
//! Values are carried as `u64` bit patterns masked to their type's width. All
//! widths are ≤ 64 (enforced by the checker), so arithmetic stays exact.

use std::collections::HashMap;

use sinter_syntax::ast::{BinOp, CastMode, Type, UnaryOp};

use crate::error::{mask, signed_value};
use crate::ir::{Expr, ExprKind};

/// Apply a unary operator to a masked operand value.
#[must_use]
pub fn apply_unary(op: UnaryOp, v: u64, operand_ty: Type) -> u64 {
    let w = operand_ty.width();
    match op {
        // Logical not on a 1-bit value.
        UnaryOp::Not => u64::from(v == 0),
        UnaryOp::BitNot => mask(!v, w),
        UnaryOp::Neg => mask(v.wrapping_neg(), w),
    }
}

/// Apply a binary operator. Arithmetic/bitwise results are masked to
/// `result_ty`; comparisons yield a 1-bit `0`/`1`. Operand types are equal
/// (enforced by the checker) and determine signed vs unsigned comparison.
#[must_use]
pub fn apply_binary(op: BinOp, l: u64, lty: Type, r: u64, rty: Type, result_ty: Type) -> u64 {
    let w = result_ty.width();
    match op {
        // Operands are extended to the (possibly wider) result type before the
        // op, so width-inferred `+`/`-` (DESIGN §6.2) is exact for signed
        // operands too. A no-op on the legacy same-width path.
        BinOp::Add => mask(ext(l, lty, w).wrapping_add(ext(r, rty, w)), w),
        BinOp::Sub => mask(ext(l, lty, w).wrapping_sub(ext(r, rty, w)), w),
        // Operands are extended to the (wider) product type, so signed and
        // unsigned multiply share one masked low-`w`-bits implementation
        // (DESIGN §6.2: `Qa.b * Qc.d -> Q(a+c).(b+d)`).
        BinOp::Mul => mask(ext(l, lty, w).wrapping_mul(ext(r, rty, w)), w),
        BinOp::And => mask(l & r, w),
        BinOp::Or => mask(l | r, w),
        BinOp::Xor => mask(l ^ r, w),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let a = signed_value(l, lty);
            let b = signed_value(r, rty);
            let result = match op {
                BinOp::Eq => a == b,
                BinOp::Ne => a != b,
                BinOp::Lt => a < b,
                BinOp::Gt => a > b,
                BinOp::Le => a <= b,
                BinOp::Ge => a >= b,
                _ => unreachable!("outer match restricts to comparisons"),
            };
            u64::from(result)
        }
    }
}

/// Evaluate a typed expression against an environment mapping readable names to
/// their current masked values. Names are guaranteed present by the checker.
#[must_use]
pub fn eval_expr(expr: &Expr, env: &HashMap<String, u64>) -> u64 {
    match &expr.kind {
        ExprKind::Literal(v) => *v,
        ExprKind::Ident(name) => *env
            .get(name)
            .expect("checker guarantees every referenced name is in scope"),
        ExprKind::Unary { op, expr: inner } => apply_unary(*op, eval_expr(inner, env), inner.ty),
        ExprKind::Binary { op, lhs, rhs } => apply_binary(
            *op,
            eval_expr(lhs, env),
            lhs.ty,
            eval_expr(rhs, env),
            rhs.ty,
            expr.ty,
        ),
        ExprKind::If { cond, then, els } => {
            if eval_expr(cond, env) != 0 {
                eval_expr(then, env)
            } else {
                eval_expr(els, env)
            }
        }
        ExprKind::Cast { expr: inner, mode } => {
            apply_cast(eval_expr(inner, env), inner.ty, expr.ty, *mode)
        }
        ExprKind::Shl {
            expr: inner,
            amount,
        } => apply_shl(eval_expr(inner, env), expr.ty.width(), *amount),
        ExprKind::Shr {
            expr: inner,
            amount,
        } => apply_shr(eval_expr(inner, env), inner.ty, expr.ty.width(), *amount),
    }
}

/// Left shift `v` by `amount`, masked to the (wider) result width `to_w`. The
/// value scales by `2^amount`.
#[must_use]
pub fn apply_shl(v: u64, to_w: u32, amount: u32) -> u64 {
    if amount >= 64 {
        0
    } else {
        mask(v << amount, to_w)
    }
}

/// Right shift `v` (of type `from`) by `amount`, masked to the (narrower) result
/// width `to_w`. Arithmetic (toward -inf) for a signed operand, logical for
/// unsigned.
#[must_use]
pub fn apply_shr(v: u64, from: Type, to_w: u32, amount: u32) -> u64 {
    if from.is_signed() {
        // `signed_value` is i128, so the shift is always well-defined.
        mask((signed_value(v, from) >> amount.min(127)) as u64, to_w)
    } else if amount >= 64 {
        0
    } else {
        mask(v >> amount, to_w)
    }
}

/// Extend a masked operand `v` of type `from` to a `to_w`-bit value, sign-
/// extending signed operands and zero-extending unsigned ones. A no-op when the
/// operand is already at least `to_w` bits wide (the legacy same-width path).
fn ext(v: u64, from: Type, to_w: u32) -> u64 {
    let fw = from.width();
    if from.is_signed() && fw > 0 && fw < to_w && (v >> (fw - 1)) & 1 == 1 {
        let hi = if to_w >= 64 {
            u64::MAX
        } else {
            (1u64 << to_w) - 1
        };
        let low = if fw >= 64 { u64::MAX } else { (1u64 << fw) - 1 };
        v | (hi & !low)
    } else {
        v
    }
}

/// Apply a mode-named cast (DESIGN §6.3), converting a masked value of type
/// `from` to type `to`. The result is masked to `to`'s width.
#[must_use]
pub fn apply_cast(v: u64, from: Type, to: Type, mode: CastMode) -> u64 {
    let to_w = to.width();
    match mode {
        // Resize: sign/zero-extend (per the source type) when widening, truncate
        // the high bits when narrowing. `trunc`/`round` never reach the IR (the
        // elaborator lowers them to shifts); kept here for match exhaustiveness.
        CastMode::Wrap | CastMode::Trunc | CastMode::Round => mask(ext(v, from, to_w), to_w),
        CastMode::Sat => {
            let val = signed_value(v, from);
            let (lo, hi) = range_of(to);
            mask(val.clamp(lo, hi) as u64, to_w)
        }
    }
}

/// The inclusive value range of a scalar type, as `i128` (widths are ≤ 64).
fn range_of(ty: Type) -> (i128, i128) {
    let w = ty.width();
    if ty.is_signed() {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}
