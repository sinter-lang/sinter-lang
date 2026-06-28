//! Compile-time expansion of `include_hex!("path")` (DESIGN §9.7).
//!
//! A source-to-source pass, run before checking/elaboration: each
//! [`Expr::IncludeHex`] is read from disk (relative to the source directory) and
//! replaced with an [`Expr::Array`] of literal words — so the rest of the compiler
//! (checking, the `rom` data path, elaboration) handles it exactly like an inline
//! array literal, and the emitted SystemVerilog stays self-contained (no runtime
//! `$readmemh`, no external data file — DESIGN §9.7).
//!
//! The file is **line-delimited hex**: one word per line, an optional `0x`/`0X`
//! prefix, with blank lines and `//` comments skipped (a strict subset of the
//! `$readmemh` format). Size/width are checked later, when the resulting array is
//! validated against the `rom`'s depth and element type.

use std::path::Path;

use sinter_syntax::ast::{Expr, FieldInit, Item, MatchArm, Program, Stmt};

use crate::error::CompileError;

/// Expand every `include_hex!` in `program` in place. `base` is the directory the
/// paths resolve against (the source file's directory); `None` when compiling a
/// bare string (no file context), in which case any `include_hex!` is an error.
///
/// # Errors
/// Returns a [`CompileError`] if a path cannot be resolved/read or the file is not
/// valid line-delimited hex.
pub fn expand_includes(program: &mut Program, base: Option<&Path>) -> Result<(), CompileError> {
    for item in &mut program.items {
        match item {
            Item::Const(c) => expand_expr(&mut c.value, base, &c.name)?,
            Item::Struct(s) => {
                for f in &mut s.fields {
                    if let Some(d) = &mut f.default {
                        expand_expr(d, base, &s.name)?;
                    }
                }
            }
            Item::Enum(e) => {
                for v in &mut e.variants {
                    if let Some(d) = &mut v.discriminant {
                        expand_expr(d, base, &e.name)?;
                    }
                }
            }
            Item::Func(f) => expand_block(&mut f.body, base, &f.name)?,
            Item::Impl(im) => {
                for m in &mut im.methods {
                    expand_block(&mut m.body, base, &im.type_name)?;
                }
            }
            Item::Mod(_) | Item::Use(_) => {}
        }
    }
    Ok(())
}

fn expand_block(
    block: &mut sinter_syntax::ast::Block,
    base: Option<&Path>,
    ctx: &str,
) -> Result<(), CompileError> {
    for stmt in &mut block.stmts {
        match stmt {
            Stmt::Let { value, .. } => expand_expr(value, base, ctx)?,
            Stmt::Yield { value, .. } => {
                if let Some(v) = value {
                    expand_expr(v, base, ctx)?;
                }
            }
        }
    }
    expand_expr(&mut block.tail, base, ctx)
}

fn expand_expr(e: &mut Expr, base: Option<&Path>, ctx: &str) -> Result<(), CompileError> {
    match e {
        Expr::IncludeHex(path) => {
            *e = load_hex_array(path, base, ctx)?;
            Ok(())
        }
        Expr::Unary { expr, .. } | Expr::Cast { expr, .. } => expand_expr(expr, base, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            expand_expr(lhs, base, ctx)?;
            expand_expr(rhs, base, ctx)
        }
        Expr::If { cond, then, els } => {
            expand_expr(cond, base, ctx)?;
            expand_expr(then, base, ctx)?;
            expand_expr(els, base, ctx)
        }
        Expr::Field { base: b, .. } => expand_expr(b, base, ctx),
        Expr::Call { func, args } => {
            expand_expr(func, base, ctx)?;
            args.iter_mut().try_for_each(|a| expand_expr(a, base, ctx))
        }
        Expr::MethodCall { receiver, args, .. } => {
            expand_expr(receiver, base, ctx)?;
            args.iter_mut().try_for_each(|a| expand_expr(a, base, ctx))
        }
        Expr::Index { base: b, index } => {
            expand_expr(b, base, ctx)?;
            expand_expr(index, base, ctx)
        }
        Expr::StructLit { fields, .. } => fields
            .iter_mut()
            .try_for_each(|FieldInit { value, .. }| expand_expr(value, base, ctx)),
        Expr::Tuple(items) | Expr::Array(items) => {
            items.iter_mut().try_for_each(|i| expand_expr(i, base, ctx))
        }
        Expr::Match { scrutinee, arms } => {
            expand_expr(scrutinee, base, ctx)?;
            arms.iter_mut()
                .try_for_each(|MatchArm { body, .. }| expand_expr(body, base, ctx))
        }
        Expr::Block(b) => expand_block(b, base, ctx),
        Expr::Some(inner) | Expr::Try(inner) => expand_expr(inner, base, ctx),
        Expr::None | Expr::Literal(_) | Expr::Ident(_) | Expr::Path(_) => Ok(()),
    }
}

/// Read `path` (relative to `base`) as line-delimited hex and build an array
/// literal of its words.
fn load_hex_array(path: &str, base: Option<&Path>, ctx: &str) -> Result<Expr, CompileError> {
    let dir = base.ok_or_else(|| {
        err(
            ctx,
            format!(
                "`include_hex!(\"{path}\")` needs a file-based project (compile a file, not an \
                 in-memory string)"
            ),
        )
    })?;
    let full = dir.join(path);
    let content = std::fs::read_to_string(&full)
        .map_err(|e| err(ctx, format!("cannot read `{}`: {e}", full.display())))?;

    let mut words = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        // Strip a `//` line comment, then surrounding whitespace.
        let line = raw.split("//").next().unwrap_or(raw).trim();
        if line.is_empty() {
            continue;
        }
        let hex = line
            .strip_prefix("0x")
            .or_else(|| line.strip_prefix("0X"))
            .unwrap_or(line);
        let v = u128::from_str_radix(hex, 16).map_err(|_| {
            err(
                ctx,
                format!("{}:{}: `{line}` is not a hex word", path, i + 1),
            )
        })?;
        words.push(Expr::Literal(v));
    }
    Ok(Expr::Array(words))
}

fn err(ctx: &str, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, sinter_syntax::ast::Pos::default(), msg)
}
