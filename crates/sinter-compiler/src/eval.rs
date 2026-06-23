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

use sinter_syntax::ast::{BinOp, Type, UnaryOp};

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
        BinOp::Add => mask(l.wrapping_add(r), w),
        BinOp::Sub => mask(l.wrapping_sub(r), w),
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
    }
}
