//! Compile-time expansion of `include_hex!("path")` (DESIGN §9.7) and of the
//! array repeat `[value; N]` (DESIGN §6.6).
//!
//! A source-to-source pass, run before checking/elaboration: each
//! [`ExprKind::IncludeHex`] is read from disk (relative to the source directory) and
//! replaced with an [`ExprKind::Array`] of literal words — so the rest of the compiler
//! (checking, the `rom` data path, elaboration) handles it exactly like an inline
//! array literal, and the emitted SystemVerilog stays self-contained (no runtime
//! `$readmemh`, no external data file — DESIGN §9.7).
//!
//! The file is **line-delimited hex**: one word per line, an optional `0x`/`0X`
//! prefix, with blank lines and `//` comments skipped (a strict subset of the
//! `$readmemh` format). Size/width are checked later, when the resulting array is
//! validated against the `rom`'s depth and element type.
//!
//! `[value; N]` becomes an array literal of `N` copies of `value`. `N` is a
//! literal or the name of a `const` whose value is a literal (possibly
//! width-suffixed), so later passes only ever see ordinary arrays.

use std::collections::HashMap;
use std::path::Path;

use sinter_syntax::ast::{Expr, ExprKind, FieldInit, Item, MatchArm, Program, Stmt};

use crate::error::CompileError;

/// Expand every `include_hex!` in `program` in place. `base` is the directory the
/// paths resolve against (the source file's directory); `None` when compiling a
/// bare string (no file context), in which case any `include_hex!` is an error.
///
/// # Errors
/// Returns a [`CompileError`] if a path cannot be resolved/read or the file is not
/// valid line-delimited hex.
pub fn expand_includes(program: &mut Program, base: Option<&Path>) -> Result<(), CompileError> {
    let consts: HashMap<String, u128> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Const(c) => literal_of(&c.value).map(|v| (c.name.clone(), v)),
            _ => None,
        })
        .collect();
    let x = Expander {
        base,
        consts: &consts,
    };
    for item in &mut program.items {
        match item {
            Item::Const(c) => x.expr(&mut c.value, &c.name)?,
            Item::Struct(s) => {
                for f in &mut s.fields {
                    if let Some(d) = &mut f.default {
                        x.expr(d, &s.name)?;
                    }
                }
            }
            Item::Enum(e) => {
                for v in &mut e.variants {
                    if let Some(d) = &mut v.discriminant {
                        x.expr(d, &e.name)?;
                    }
                }
            }
            Item::Func(f) => x.block(&mut f.body, &f.name)?,
            Item::Impl(im) => {
                for m in &mut im.methods {
                    x.block(&mut m.body, &im.type_name)?;
                }
            }
            Item::Mod(_) | Item::Use(_) => {}
        }
    }
    Ok(())
}

/// The expansion walk: the source directory for `include_hex!` and the
/// literal-valued `const`s a repeat length may name.
struct Expander<'a> {
    base: Option<&'a Path>,
    consts: &'a HashMap<String, u128>,
}

impl Expander<'_> {
    fn block(&self, block: &mut sinter_syntax::ast::Block, ctx: &str) -> Result<(), CompileError> {
        let x = self;
        for stmt in &mut block.stmts {
            match stmt {
                Stmt::Let { value, .. } => x.expr(value, ctx)?,
                Stmt::Yield { value, .. } => {
                    if let Some(v) = value {
                        x.expr(v, ctx)?;
                    }
                }
                // An `assert!` condition is an ordinary expression; expand any
                // `include_hex!` in it (unlikely but keeps the walk uniform).
                Stmt::Assert { cond, .. } => x.expr(cond, ctx)?,
            }
        }
        x.expr(&mut block.tail, ctx)
    }

    fn expr(&self, e: &mut Expr, ctx: &str) -> Result<(), CompileError> {
        let x = self;
        let pos = e.pos;
        match &mut e.kind {
            ExprKind::IncludeHex(path) => {
                *e = Expr::new(load_hex_array(path, self.base, ctx)?, pos);
                Ok(())
            }
            ExprKind::ArrayRepeat { value, len } => {
                x.expr(value, ctx)?;
                let n = match &len.kind {
                    ExprKind::Ident(name) => self.consts.get(name).copied().ok_or_else(|| {
                        err(
                            ctx,
                            format!("`[value; {name}]` needs a `const` with a literal value"),
                        )
                    })?,
                    _ => literal_of(len).ok_or_else(|| {
                        err(
                            ctx,
                            "the length in `[value; N]` must be a literal or a `const`",
                        )
                    })?,
                };
                let n = usize::try_from(n)
                    .ok()
                    .filter(|&n| n <= MAX_REPEAT)
                    .ok_or_else(|| err(ctx, format!("`[value; {n}]` is too long")))?;
                *e = Expr::new(ExprKind::Array(vec![(**value).clone(); n]), pos);
                Ok(())
            }
            ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => x.expr(expr, ctx),
            ExprKind::Binary { lhs, rhs, .. } => {
                x.expr(lhs, ctx)?;
                x.expr(rhs, ctx)
            }
            ExprKind::If { cond, then, els } => {
                x.expr(cond, ctx)?;
                x.expr(then, ctx)?;
                x.expr(els, ctx)
            }
            ExprKind::Field { base: b, .. } => x.expr(b, ctx),
            ExprKind::Call { func, args } => {
                x.expr(func, ctx)?;
                args.iter_mut().try_for_each(|a| x.expr(a, ctx))
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                x.expr(receiver, ctx)?;
                args.iter_mut().try_for_each(|a| x.expr(a, ctx))
            }
            ExprKind::Index { base: b, index } => {
                x.expr(b, ctx)?;
                x.expr(index, ctx)
            }
            ExprKind::Slice { base: b, hi, lo } => {
                x.expr(b, ctx)?;
                x.expr(hi, ctx)?;
                x.expr(lo, ctx)
            }
            ExprKind::StructLit { fields, .. } => fields
                .iter_mut()
                .try_for_each(|FieldInit { value, .. }| x.expr(value, ctx)),
            ExprKind::Tuple(items) | ExprKind::Array(items) => {
                items.iter_mut().try_for_each(|i| x.expr(i, ctx))
            }
            ExprKind::Match { scrutinee, arms } => {
                x.expr(scrutinee, ctx)?;
                arms.iter_mut()
                    .try_for_each(|MatchArm { body, .. }| x.expr(body, ctx))
            }
            ExprKind::Block(b) => x.block(b, ctx),
            ExprKind::Some(inner) | ExprKind::Try(inner) => x.expr(inner, ctx),
            ExprKind::None | ExprKind::Literal(_) | ExprKind::Ident(_) | ExprKind::Path(_) => {
                Ok(())
            }
        }
    }
}

/// Read `path` (relative to `base`) as line-delimited hex and build an array
/// literal of its words.
fn load_hex_array(path: &str, base: Option<&Path>, ctx: &str) -> Result<ExprKind, CompileError> {
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
        words.push(Expr::from(ExprKind::Literal(v)));
    }
    Ok(ExprKind::Array(words))
}

/// The longest array a repeat may expand to.
const MAX_REPEAT: usize = 1 << 16;

/// The value of a literal, or of a width-suffixed literal (which the parser
/// desugars to a `wrap` cast of one).
fn literal_of(e: &Expr) -> Option<u128> {
    match &e.kind {
        ExprKind::Literal(v) => Some(*v),
        ExprKind::Cast { expr, .. } => match &expr.kind {
            ExprKind::Literal(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

fn err(ctx: &str, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, sinter_syntax::ast::Pos::default(), msg)
}
