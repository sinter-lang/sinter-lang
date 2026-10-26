//! `Self` inside an `impl` block: replaced by the implementing struct's name
//! as each `impl` is parsed, so the rest of the compiler only sees the name.
//!
//! `Self` is a type (`-> Self`, `let s: Self`) and a struct-literal name
//! (`Self { .. }`). It is not a reserved word: outside an `impl` it is an
//! ordinary name, which names nothing.

use crate::ast::{Block, Expr, ExprKind, ImplItem, Param, Stmt, TypeRef};

const SELF: &str = "Self";

/// Replace every `Self` in `im`'s methods with `im.type_name`.
pub(crate) fn expand(im: &mut ImplItem) {
    let name = im.type_name.clone();
    for m in &mut im.methods {
        for p in &mut m.params {
            if let Param::Typed { ty, .. } = p {
                ty_(ty, &name);
            }
        }
        ty_(&mut m.ret, &name);
        block(&mut m.body, &name);
    }
}

fn ty_(t: &mut TypeRef, name: &str) {
    match t {
        TypeRef::Named(n) if n == SELF => *n = name.to_string(),
        TypeRef::Option(inner) => ty_(inner, name),
        TypeRef::Array { elem, len } => {
            ty_(elem, name);
            expr(len, name);
        }
        TypeRef::Tuple(items) => items.iter_mut().for_each(|i| ty_(i, name)),
        TypeRef::Cdc { elem, depth, .. } => {
            if let Some(e) = elem {
                ty_(e, name);
            }
            if let Some(d) = depth {
                expr(d, name);
            }
        }
        TypeRef::Mem { elem, depth, .. } | TypeRef::Channel { elem, depth, .. } => {
            ty_(elem, name);
            expr(depth, name);
        }
        TypeRef::Named(_) | TypeRef::Scalar(_) | TypeRef::Fixed { .. } => {}
    }
}

fn block(b: &mut Block, name: &str) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { ty, value, .. } => {
                if let Some(t) = ty {
                    ty_(t, name);
                }
                expr(value, name);
            }
            Stmt::Yield { value, .. } => {
                if let Some(v) = value {
                    expr(v, name);
                }
            }
            Stmt::Assert { cond, .. } => expr(cond, name),
        }
    }
    expr(&mut b.tail, name);
}

fn expr(e: &mut Expr, name: &str) {
    match &mut e.kind {
        ExprKind::StructLit {
            name: n,
            fields,
            base,
        } => {
            if n == SELF {
                *n = name.to_string();
            }
            for f in fields {
                expr(&mut f.value, name);
            }
            if let Some(b) = base {
                expr(b, name);
            }
        }
        ExprKind::Cast {
            expr: inner, ty, ..
        } => {
            expr(inner, name);
            ty_(ty, name);
        }
        ExprKind::Block(b) => block(b, name),
        ExprKind::Unary { expr: inner, .. }
        | ExprKind::Some(inner)
        | ExprKind::Try(inner)
        | ExprKind::Field { base: inner, .. } => expr(inner, name),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr(lhs, name);
            expr(rhs, name);
        }
        ExprKind::If { cond, then, els } => {
            expr(cond, name);
            expr(then, name);
            expr(els, name);
        }
        ExprKind::ArrayRepeat { value, len } => {
            expr(value, name);
            expr(len, name);
        }
        ExprKind::Call { func, args } => {
            expr(func, name);
            args.iter_mut().for_each(|a| expr(a, name));
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            expr(receiver, name);
            args.iter_mut().for_each(|a| expr(a, name));
        }
        ExprKind::Index { base, index } => {
            expr(base, name);
            expr(index, name);
        }
        ExprKind::Slice { base, hi, lo } => {
            expr(base, name);
            expr(hi, name);
            expr(lo, name);
        }
        ExprKind::Tuple(items) | ExprKind::Array(items) => {
            items.iter_mut().for_each(|i| expr(i, name));
        }
        ExprKind::Match { scrutinee, arms } => {
            expr(scrutinee, name);
            arms.iter_mut().for_each(|a| expr(&mut a.body, name));
        }
        ExprKind::Literal(_)
        | ExprKind::Ident(_)
        | ExprKind::Path(_)
        | ExprKind::IncludeHex(_)
        | ExprKind::None => {}
    }
}
