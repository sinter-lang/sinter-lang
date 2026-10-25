//! The single, authoritative evaluation semantics for Sinter expressions.
//!
//! Both the constant-folder (reset values) and the cycle-accurate simulator
//! call into these functions, and the Verilog backend is written to match them
//! bit-for-bit. Centralizing the operator semantics here is what keeps the two
//! lowerings in agreement (DESIGN §7.4). The arithmetic itself is
//! `sim_runtime.rs`'s, which the generated Rust simulator shares.
//!
//! Values are carried as `u64` bit patterns masked to their type's width. All
//! widths are ≤ 64 (enforced by the checker), so arithmetic stays exact.

use std::collections::HashMap;

use sinter_syntax::ast::{BinOp, CastMode, Type, UnaryOp};

use crate::ir::{Expr, ExprKind};
use crate::sim_runtime as rt;

/// A [`Type`] as the runtime's [`rt::Ty`].
#[must_use]
pub(crate) fn rt_ty(ty: Type) -> rt::Ty {
    match ty {
        Type::Uint(w) => rt::u(w),
        Type::Int(w) => rt::i(w),
    }
}

/// A unary operator as the runtime's [`rt::Un`].
#[must_use]
pub(crate) fn rt_un(op: UnaryOp) -> rt::Un {
    match op {
        UnaryOp::Not => rt::Un::Not,
        UnaryOp::BitNot => rt::Un::BitNot,
        UnaryOp::Neg => rt::Un::Neg,
    }
}

/// A binary operator as the runtime's [`rt::Bin`].
///
/// # Panics
/// On a shift or concatenation, which lower to their own IR nodes and never
/// reach a `Binary` (DESIGN §6.13 / §9.10).
#[must_use]
pub(crate) fn rt_bin(op: BinOp) -> rt::Bin {
    match op {
        BinOp::Add => rt::Bin::Add,
        BinOp::Sub => rt::Bin::Sub,
        BinOp::Mul => rt::Bin::Mul,
        BinOp::And => rt::Bin::And,
        BinOp::Or => rt::Bin::Or,
        BinOp::Xor => rt::Bin::Xor,
        BinOp::Eq => rt::Bin::Eq,
        BinOp::Ne => rt::Bin::Ne,
        BinOp::Lt => rt::Bin::Lt,
        BinOp::Gt => rt::Bin::Gt,
        BinOp::Le => rt::Bin::Le,
        BinOp::Ge => rt::Bin::Ge,
        BinOp::Shl | BinOp::Shr | BinOp::Concat => {
            unreachable!("shift/concat lower to ExprKind nodes, not a Binary op")
        }
    }
}

/// Apply a unary operator to a masked operand value.
#[must_use]
pub fn apply_unary(op: UnaryOp, v: u64, operand_ty: Type) -> u64 {
    rt::unary(rt_un(op), v, rt_ty(operand_ty))
}

/// Apply a binary operator. Arithmetic/bitwise results are masked to
/// `result_ty`; comparisons yield a 1-bit `0`/`1`. Operand types determine
/// signed vs unsigned comparison.
#[must_use]
pub fn apply_binary(op: BinOp, l: u64, lty: Type, r: u64, rty: Type, result_ty: Type) -> u64 {
    rt::binary(rt_bin(op), l, rt_ty(lty), r, rt_ty(rty), rt_ty(result_ty))
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
        // Variable (barrel) shift, same operand width.
        ExprKind::VarShift {
            expr: inner,
            amount,
            left,
        } => rt::var_shift(
            eval_expr(inner, env),
            eval_expr(amount, env),
            rt_ty(inner.ty),
            *left,
        ),
    }
}

/// Left shift `v` by `amount`, masked to the (wider) result width `to_w`. The
/// value scales by `2^amount`.
#[must_use]
pub fn apply_shl(v: u64, to_w: u32, amount: u32) -> u64 {
    rt::shl(v, to_w, amount)
}

/// Right shift `v` (of type `from`) by `amount`, masked to the (narrower) result
/// width `to_w`. Arithmetic (toward -inf) for a signed operand, logical for
/// unsigned.
#[must_use]
pub fn apply_shr(v: u64, from: Type, to_w: u32, amount: u32) -> u64 {
    rt::shr(v, rt_ty(from), to_w, amount)
}

/// Apply a mode-named cast (DESIGN §6.3), converting a masked value of type
/// `from` to type `to`. The result is masked to `to`'s width.
#[must_use]
pub fn apply_cast(v: u64, from: Type, to: Type, mode: CastMode) -> u64 {
    match mode {
        // `trunc` / `round` never reach the IR (the elaborator lowers them to
        // shifts); kept here for match exhaustiveness.
        CastMode::Wrap | CastMode::Trunc | CastMode::Round => rt::wrap(v, rt_ty(from), rt_ty(to)),
        CastMode::Sat => rt::sat(v, rt_ty(from), rt_ty(to)),
    }
}
