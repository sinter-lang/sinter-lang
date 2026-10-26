//! Struct update: `S { a: x, ..base }` becomes a literal naming every field.
//!
//! The fields not listed are read from `base`. A base that isn't a plain name
//! (or `self`) is bound once with a `let`, so it is computed once:
//!
//! ```text
//! S { a: x, ..f(y) }   =>   { let __update_0 = f(y); S { a: x, b: __update_0.b } }
//! ```
//!
//! Runs right after name resolution, before every other pass, so nothing
//! downstream sees a `base`.

use std::collections::HashMap;

use sinter_syntax::ast::{Block, Expr, ExprKind, FieldInit, Item, Pattern, Program, Stmt};

use crate::error::CompileError;

struct Expander {
    /// Each struct's field names, in declaration order.
    fields: HashMap<String, Vec<String>>,
    /// The next `__update_<n>` binding.
    next: usize,
    /// The first error, reported once the walk is done.
    error: Option<CompileError>,
    /// The item being expanded, for diagnostics.
    ctx: String,
}

impl Expander {
    fn expr(&mut self, e: &mut Expr) {
        e.for_each_child_mut(&mut |c| self.expr(c));
        let pos = e.pos;
        let ExprKind::StructLit { name, fields, base } = &mut e.kind else {
            return;
        };
        let Some(base) = base.take() else {
            return;
        };
        let Some(all) = self.fields.get(name.as_str()) else {
            // An unknown struct: leave the literal for type checking to report.
            return;
        };
        let missing: Vec<String> = all
            .iter()
            .filter(|f| !fields.iter().any(|fi| &fi.name == *f))
            .cloned()
            .collect();
        if missing.is_empty() {
            self.error.get_or_insert_with(|| {
                CompileError::semantic(
                    &self.ctx,
                    base.pos,
                    format!("every field of `{name}` is listed, so `..` has nothing to fill in"),
                )
            });
            return;
        }
        let simple = matches!(base.kind, ExprKind::Ident(_));
        let source = if simple {
            (*base).clone()
        } else {
            let binding = format!("__update_{}", self.next);
            self.next += 1;
            Expr::new(ExprKind::Ident(binding), base.pos)
        };
        for f in missing {
            fields.push(FieldInit {
                name: f.clone(),
                value: Expr::new(
                    ExprKind::Field {
                        base: Box::new(source.clone()),
                        field: f,
                    },
                    base.pos,
                ),
            });
        }
        if !simple {
            let ExprKind::Ident(binding) = source.kind else {
                return;
            };
            let lit = std::mem::replace(&mut e.kind, ExprKind::None);
            e.kind = ExprKind::Block(Block {
                stmts: vec![Stmt::Let {
                    pattern: Pattern::Binding(binding),
                    ty: None,
                    value: *base,
                    pos,
                }],
                tail: Box::new(Expr::new(lit, pos)),
                pos,
            });
        }
    }

    fn block(&mut self, b: &mut Block) {
        for s in &mut b.stmts {
            match s {
                Stmt::Let { value, .. } => self.expr(value),
                Stmt::Yield { value, .. } => {
                    if let Some(v) = value {
                        self.expr(v);
                    }
                }
                Stmt::Assert { cond, .. } => self.expr(cond),
            }
        }
        self.expr(&mut b.tail);
    }
}

/// Expand every struct update in `program`.
///
/// # Errors
/// A [`CompileError`] for an update that lists every field already.
pub fn expand(program: &mut Program) -> Result<(), CompileError> {
    let fields = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Struct(s) => Some((
                s.name.clone(),
                s.fields.iter().map(|f| f.name.clone()).collect(),
            )),
            _ => None,
        })
        .collect();
    let mut x = Expander {
        fields,
        next: 0,
        error: None,
        ctx: String::new(),
    };
    for item in &mut program.items {
        match item {
            Item::Func(f) => {
                x.ctx.clone_from(&f.name);
                x.block(&mut f.body);
            }
            Item::Impl(im) => {
                for m in &mut im.methods {
                    x.ctx.clone_from(&m.name);
                    x.block(&mut m.body);
                }
            }
            Item::Const(c) => {
                x.ctx.clone_from(&c.name);
                x.expr(&mut c.value);
            }
            Item::Struct(s) => {
                x.ctx.clone_from(&s.name);
                for f in &mut s.fields {
                    if let Some(d) = &mut f.default {
                        x.expr(d);
                    }
                }
            }
            Item::Enum(_) | Item::Mod(_) | Item::Use(_) => {}
        }
    }
    match x.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
