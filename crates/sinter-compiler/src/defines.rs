//! Build-time constant overrides (DEF1, DESIGN §9.15).
//!
//! `-D NAME=VALUE` (or a `--defines` file of `NAME = VALUE` lines) replaces
//! the value of an integer `pub const` before [`crate::const_fold`] runs, so
//! every use of the constant, and every constant computed from it, sees the
//! new value. The output is specialized: the Verilog has no parameters, just
//! the folded numbers, as if the source had said the new value.
//!
//! ```text
//! pub const N_MUL: u8 = 1;        // sinter build top.snt -D N_MUL=2
//! ```
//!
//! A constant in a submodule is named by its path from the root module
//! (`setup::N_MUL`), the name [`crate::resolve`] gives it.

use sinter_syntax::ast::{CastMode, Expr, ExprKind, Item, Program, TypeRef, Visibility};

use crate::error::CompileError;

/// One override: a constant's path and its new value, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Define {
    /// The constant's path from the root module (`N_MUL`, `setup::N_MUL`).
    pub name: String,
    /// The value: decimal (a leading `-` for a signed constant), `0x` hex, or
    /// `0b` binary, with optional `_` separators.
    pub value: String,
    /// Where the override came from, for diagnostics.
    pub origin: String,
}

impl Define {
    /// Parse a `-D` argument, `NAME=VALUE`.
    ///
    /// # Errors
    /// A [`CompileError::Define`] when there is no `=` or either side is empty.
    pub fn parse(spec: &str) -> Result<Self, CompileError> {
        let origin = format!("-D {spec}");
        split(spec, &origin)
    }

    /// Parse a `--defines` file: one `NAME = VALUE` per line; blank lines and
    /// `#` comments are skipped. `source` names the file in diagnostics.
    ///
    /// # Errors
    /// A [`CompileError::Define`] naming the first malformed line.
    pub fn parse_file(text: &str, source: &str) -> Result<Vec<Self>, CompileError> {
        text.lines()
            .enumerate()
            .filter_map(|(i, raw)| {
                let line = raw.split('#').next().unwrap_or("").trim();
                (!line.is_empty()).then(|| split(line, &format!("{source}:{}", i + 1)))
            })
            .collect()
    }
}

fn split(spec: &str, origin: &str) -> Result<Define, CompileError> {
    let bad = || CompileError::Define {
        origin: origin.to_string(),
        msg: "expected `NAME=VALUE`, such as `N_MUL=2`".to_string(),
    };
    let (name, value) = spec.split_once('=').ok_or_else(bad)?;
    let (name, value) = (name.trim(), value.trim());
    if name.is_empty() || value.is_empty() {
        return Err(bad());
    }
    Ok(Define {
        name: name.to_string(),
        value: value.to_string(),
        origin: origin.to_string(),
    })
}

/// Apply `defines` to `program`'s constants (resolved names), in order; a
/// later override of the same constant wins.
///
/// # Errors
/// A [`CompileError::Define`] for an unknown name, a constant that isn't an
/// integer `pub const`, or a value that is malformed or doesn't fit.
pub(crate) fn apply(program: &mut Program, defines: &[Define]) -> Result<(), CompileError> {
    for d in defines {
        let err = |msg: String| CompileError::Define {
            origin: d.origin.clone(),
            msg,
        };
        let Some(c) = program.items.iter_mut().find_map(|it| match it {
            Item::Const(c) if c.name == d.name => Some(c),
            _ => None,
        }) else {
            let mut names: Vec<String> = program
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Const(c)
                        if c.vis == Visibility::Public && matches!(c.ty, TypeRef::Scalar(_)) =>
                    {
                        Some(format!("`{}`", c.name))
                    }
                    _ => None,
                })
                .collect();
            names.sort();
            let known = if names.is_empty() {
                "the design has no integer `pub const`".to_string()
            } else {
                format!("the integer `pub const`s are {}", names.join(", "))
            };
            return Err(err(format!("no constant `{}`; {known}", d.name)));
        };
        if c.vis != Visibility::Public {
            return Err(err(format!(
                "`{}` isn't `pub`; only a `pub const` can be overridden",
                d.name
            )));
        }
        let TypeRef::Scalar(ty) = c.ty else {
            return Err(err(format!(
                "`{}` isn't an integer constant; only `uN` / `iN` constants can be overridden",
                d.name
            )));
        };
        let v = parse_int(&d.value).ok_or_else(|| {
            err(format!(
                "`{}` isn't an integer (decimal, `0x` hex, or `0b` binary)",
                d.value
            ))
        })?;
        let w = ty.width();
        let (lo, hi) = if ty.is_signed() {
            (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
        } else {
            (0, (1i128 << w) - 1)
        };
        if v < lo || v > hi {
            let t = if ty.is_signed() { "i" } else { "u" };
            return Err(err(format!(
                "{} doesn't fit `{}`'s type `{t}{w}` ({lo} to {hi})",
                d.value, d.name
            )));
        }
        let bits = (v & ((1i128 << w) - 1)) as u128;
        let lit = Expr::new(ExprKind::Literal(bits), c.value.pos);
        c.value = if ty.is_signed() {
            // The bit pattern, reinterpreted, as `const_fold` writes it.
            Expr::new(
                ExprKind::Cast {
                    expr: Box::new(lit),
                    ty: TypeRef::Scalar(ty),
                    mode: CastMode::Wrap,
                },
                c.value.pos,
            )
        } else {
            lit
        };
    }
    Ok(())
}

/// An integer as `-D` accepts it.
fn parse_int(text: &str) -> Option<i128> {
    let (neg, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let body = body.replace('_', "");
    let (digits, radix) = if let Some(h) = body.strip_prefix("0x") {
        (h, 16)
    } else if let Some(b) = body.strip_prefix("0b") {
        (b, 2)
    } else {
        (body.as_str(), 10)
    };
    if digits.is_empty() || (neg && radix != 10) {
        return None;
    }
    let v = i128::from_str_radix(digits, radix).ok()?;
    Some(if neg { -v } else { v })
}
