//! Constant folding: an integer `const` may be computed from literals and other
//! constants, and is replaced by its value before anything else reads it.
//!
//! ```text
//! const BIT_CYCLES: u8 = 217;
//! const LAST: u8 = (BIT_CYCLES - 1) as u8 wrap;   // folds to 216
//! ```
//!
//! The expression is typed by the language's own rules (widening arithmetic,
//! no implicit conversions, mode-named casts), so a constant means what the
//! same expression would mean at run time, and must have exactly its declared
//! type. Constants may refer to each other in any order, but not in a cycle.
//! Because every folded constant is a literal afterwards, it works anywhere a
//! literal-valued constant does: array lengths, memory depths, bit slices.
//! Fixed-point constants are left as written (a literal).

use std::collections::HashMap;

use sinter_syntax::ast::{
    BinOp, CastMode, ConstItem, Expr, ExprKind, Item, Pos, Program, Type, TypeRef, UnaryOp,
};

use crate::error::CompileError;

/// The widest value Sinter holds (DESIGN §6.2).
const MAX_WIDTH: u32 = 64;

/// A folded value: its integer value and its type.
#[derive(Clone, Copy)]
struct Val {
    v: i128,
    ty: Type,
}

fn ty_name(t: Type) -> String {
    match t {
        Type::Uint(w) => format!("u{w}"),
        Type::Int(w) => format!("i{w}"),
    }
}

fn fits(v: i128, t: Type) -> bool {
    match t {
        Type::Uint(w) => v >= 0 && v < (1i128 << w),
        Type::Int(w) => v >= -(1i128 << (w - 1)) && v < (1i128 << (w - 1)),
    }
}

/// `v`'s bit pattern as an unsigned number of `t`'s width.
fn to_bits(v: i128, t: Type) -> u64 {
    // `t` is at most 64 bits wide, so the mask fits.
    (v & ((1i128 << t.width()) - 1)) as u64
}

/// The value of bit pattern `b` read as type `t`.
fn from_bits(b: u64, t: Type) -> i128 {
    let w = t.width();
    let b = i128::from(b) & ((1i128 << w) - 1);
    if t.is_signed() && b >> (w - 1) == 1 {
        b - (1i128 << w)
    } else {
        b
    }
}

/// `v` reduced into `t`'s range, modulo 2^width.
fn wrap(v: i128, t: Type) -> i128 {
    from_bits(to_bits(v, t), t)
}

struct Folder<'a> {
    consts: HashMap<&'a str, &'a ConstItem>,
    /// Folded constants, by name.
    done: HashMap<String, Val>,
    /// Constants being folded, to catch a cycle.
    active: Vec<String>,
    /// The constant being folded, for diagnostics.
    ctx: String,
}

impl Folder<'_> {
    fn err(&self, pos: Pos, msg: impl Into<String>) -> CompileError {
        CompileError::semantic(&self.ctx, pos, msg)
    }

    fn width_ok(&self, w: u32, pos: Pos) -> Result<(), CompileError> {
        if w > MAX_WIDTH {
            return Err(self.err(
                pos,
                format!("this constant expression is {w} bits wide, past the 64-bit limit"),
            ));
        }
        Ok(())
    }

    /// Fold constant `name`, folding the constants it uses first.
    fn constant(&mut self, name: &str, pos: Pos) -> Result<Val, CompileError> {
        if let Some(v) = self.done.get(name) {
            return Ok(*v);
        }
        let Some(c) = self.consts.get(name).copied() else {
            return Err(self.err(
                pos,
                format!("`{name}` is not a constant; a constant is computed from literals and other constants"),
            ));
        };
        if self.active.iter().any(|a| a == name) {
            return Err(self.err(
                c.pos,
                format!(
                    "constant `{name}` depends on itself: {} -> {name}",
                    self.active.join(" -> ")
                ),
            ));
        }
        let TypeRef::Scalar(ty) = c.ty else {
            return Err(self.err(
                pos,
                format!("`{name}` isn't an integer constant, so it can't be used in a constant expression"),
            ));
        };
        let outer = std::mem::replace(&mut self.ctx, c.name.clone());
        self.active.push(name.to_string());
        let folded = self.expr(&c.value, Some(ty));
        self.active.pop();
        self.ctx = outer;
        let v = folded?;
        if v.ty != ty {
            return Err(CompileError::semantic(
                &c.name,
                c.value.pos,
                format!(
                    "type mismatch: expected `{}`, found `{}`",
                    ty_name(ty),
                    ty_name(v.ty)
                ),
            ));
        }
        self.done.insert(name.to_string(), v);
        Ok(v)
    }

    fn literal(&self, v: u128, want: Option<Type>, pos: Pos) -> Result<Val, CompileError> {
        let Some(ty) = want else {
            return Err(self.err(pos, format!("cannot infer a type for literal `{v}`")));
        };
        let v = i128::try_from(v).unwrap_or(i128::MAX);
        if !fits(v, ty) {
            return Err(self.err(
                pos,
                format!("literal `{v}` does not fit in `{}`", ty_name(ty)),
            ));
        }
        Ok(Val { v, ty })
    }

    /// Fold both operands of a binary operator. A bare literal takes the other
    /// operand's type, as at run time.
    fn operands(&mut self, l: &Expr, r: &Expr) -> Result<(Val, Val), CompileError> {
        let lit = |e: &Expr| matches!(e.kind, ExprKind::Literal(_));
        if lit(l) && !lit(r) {
            let rv = self.expr(r, None)?;
            Ok((self.expr(l, Some(rv.ty))?, rv))
        } else {
            let lv = self.expr(l, None)?;
            Ok((lv, self.expr(r, Some(lv.ty))?))
        }
    }

    fn expr(&mut self, e: &Expr, want: Option<Type>) -> Result<Val, CompileError> {
        let pos = e.pos;
        match &e.kind {
            ExprKind::Literal(v) => self.literal(*v, want, pos),
            ExprKind::Ident(name) => self.constant(name, pos),
            ExprKind::Unary { op, expr } => match op {
                // `-128` in an `i8` is the literal's negation, not 128 negated.
                UnaryOp::Neg => {
                    let x = match (&expr.kind, want) {
                        (ExprKind::Literal(v), Some(ty)) if ty.is_signed() => {
                            let neg = -i128::try_from(*v).unwrap_or(i128::MAX);
                            if !fits(neg, ty) {
                                return Err(self.err(
                                    pos,
                                    format!("literal `-{v}` does not fit in `{}`", ty_name(ty)),
                                ));
                            }
                            return Ok(Val { v: neg, ty });
                        }
                        _ => self.expr(expr, want)?,
                    };
                    if !x.ty.is_signed() {
                        return Err(
                            self.err(pos, format!("cannot negate unsigned `{}`", ty_name(x.ty)))
                        );
                    }
                    Ok(Val {
                        v: wrap(-x.v, x.ty),
                        ty: x.ty,
                    })
                }
                UnaryOp::BitNot => {
                    let x = self.expr(expr, want)?;
                    Ok(Val {
                        v: wrap(!x.v, x.ty),
                        ty: x.ty,
                    })
                }
                UnaryOp::Not => {
                    let x = self.expr(expr, Some(Type::Uint(1)))?;
                    if x.ty != Type::Uint(1) {
                        return Err(self.err(
                            pos,
                            format!("type mismatch: expected `u1`, found `{}`", ty_name(x.ty)),
                        ));
                    }
                    Ok(Val {
                        v: 1 - x.v,
                        ty: x.ty,
                    })
                }
            },
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, pos),
            ExprKind::Cast { expr, ty, mode } => {
                let TypeRef::Scalar(to) = ty else {
                    return Err(self.err(
                        pos,
                        "a constant expression can only cast to an integer type",
                    ));
                };
                let from = match &expr.kind {
                    // A bare literal under a cast takes the target type if it
                    // fits, else the narrowest unsigned type that holds it.
                    ExprKind::Literal(v) => {
                        let v = i128::try_from(*v).unwrap_or(i128::MAX);
                        if fits(v, *to) {
                            Val { v, ty: *to }
                        } else {
                            let w = (128 - v.leading_zeros()).max(1);
                            self.width_ok(w, pos)?;
                            Val {
                                v,
                                ty: Type::Uint(w),
                            }
                        }
                    }
                    _ => self.expr(expr, None)?,
                };
                let mode = match mode {
                    // On integers there is no fraction to round or truncate.
                    CastMode::Round | CastMode::Trunc => CastMode::Wrap,
                    m => *m,
                };
                let bits = crate::eval::apply_cast(to_bits(from.v, from.ty), from.ty, *to, mode);
                Ok(Val {
                    v: from_bits(bits, *to),
                    ty: *to,
                })
            }
            ExprKind::Slice { base, hi, lo } => {
                let b = self.expr(base, None)?;
                if b.ty.is_signed() {
                    return Err(self.err(
                        pos,
                        format!(
                            "bit-slice requires an unsigned integer (`uN`) operand, not `{}`",
                            ty_name(b.ty)
                        ),
                    ));
                }
                let bound = |f: &mut Self, e: &Expr| -> Result<u32, CompileError> {
                    let v = f.expr(e, Some(Type::Uint(32)))?.v;
                    u32::try_from(v).map_err(|_| f.err(pos, "bit-slice bound out of range"))
                };
                let (h, l) = (bound(self, hi)?, bound(self, lo)?);
                if h < l || h >= b.ty.width() {
                    return Err(self.err(
                        pos,
                        format!(
                            "bit-slice [{h}:{l}] is out of range for a `{}` value",
                            ty_name(b.ty)
                        ),
                    ));
                }
                let ty = Type::Uint(h - l + 1);
                Ok(Val {
                    v: wrap(b.v >> l, ty),
                    ty,
                })
            }
            _ => Err(self.err(
                pos,
                "a constant is computed from integer literals, other constants, operators, \
                 casts, and bit slices; this expression isn't one of those",
            )),
        }
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr, pos: Pos) -> Result<Val, CompileError> {
        // A literal shift amount changes the width, as at run time; any other
        // amount keeps it.
        if let (BinOp::Shl | BinOp::Shr, ExprKind::Literal(k)) = (op, &r.kind) {
            let x = self.expr(l, None)?;
            let k = u32::try_from(*k).map_err(|_| self.err(pos, "shift amount out of range"))?;
            let w = x.ty.width();
            let (v, w) = match op {
                BinOp::Shl => (x.v << k, w + k),
                _ if k >= w => {
                    return Err(self.err(
                        pos,
                        format!("shifting a {w}-bit value right by {k} leaves nothing"),
                    ))
                }
                _ => (x.v >> k, w - k),
            };
            self.width_ok(w, pos)?;
            let ty = if x.ty.is_signed() {
                Type::Int(w)
            } else {
                Type::Uint(w)
            };
            return Ok(Val { v, ty });
        }
        if let BinOp::Shl | BinOp::Shr = op {
            let x = self.expr(l, None)?;
            let amount = self.expr(r, None)?;
            if amount.ty.is_signed() {
                return Err(self.err(pos, "a shift amount must be unsigned"));
            }
            let w = i128::from(x.ty.width());
            let v = match op {
                BinOp::Shl if amount.v >= w => 0,
                BinOp::Shl => wrap(x.v << amount.v, x.ty),
                // Past the width, a right shift leaves only the sign fill.
                _ if amount.v >= w => {
                    if x.v < 0 {
                        -1
                    } else {
                        0
                    }
                }
                _ => x.v >> amount.v,
            };
            return Ok(Val { v, ty: x.ty });
        }
        if op == BinOp::Concat {
            let (a, b) = (self.expr(l, None)?, self.expr(r, None)?);
            if a.ty.is_signed() || b.ty.is_signed() {
                return Err(self.err(
                    pos,
                    "concatenation operands must be unsigned integers (`uN`)",
                ));
            }
            let w = a.ty.width() + b.ty.width();
            self.width_ok(w, pos)?;
            return Ok(Val {
                v: (a.v << b.ty.width()) | b.v,
                ty: Type::Uint(w),
            });
        }
        let (a, b) = self.operands(l, r)?;
        let signed = a.ty.is_signed();
        if signed != b.ty.is_signed() {
            return Err(self.err(
                pos,
                format!(
                    "mixed signedness in arithmetic: `{}` and `{}`",
                    ty_name(a.ty),
                    ty_name(b.ty)
                ),
            ));
        }
        let mk = |w: u32| if signed { Type::Int(w) } else { Type::Uint(w) };
        let (wa, wb) = (a.ty.width(), b.ty.width());
        let same = |f: &Self| -> Result<(), CompileError> {
            if a.ty == b.ty {
                Ok(())
            } else {
                Err(f.err(
                    pos,
                    format!(
                        "type mismatch: expected `{}`, found `{}`",
                        ty_name(a.ty),
                        ty_name(b.ty)
                    ),
                ))
            }
        };
        let (v, ty) = match op {
            BinOp::Add | BinOp::Sub => {
                let ty = mk(wa.max(wb) + 1);
                self.width_ok(ty.width(), pos)?;
                let v = if op == BinOp::Add {
                    a.v + b.v
                } else {
                    a.v - b.v
                };
                (wrap(v, ty), ty)
            }
            BinOp::Mul => {
                let ty = mk(wa + wb);
                self.width_ok(ty.width(), pos)?;
                (a.v * b.v, ty)
            }
            BinOp::And | BinOp::Or | BinOp::Xor => {
                same(self)?;
                let (x, y) = (to_bits(a.v, a.ty), to_bits(b.v, b.ty));
                let bits = match op {
                    BinOp::And => x & y,
                    BinOp::Or => x | y,
                    _ => x ^ y,
                };
                (from_bits(bits, a.ty), a.ty)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                same(self)?;
                let t = match op {
                    BinOp::Eq => a.v == b.v,
                    BinOp::Ne => a.v != b.v,
                    BinOp::Lt => a.v < b.v,
                    BinOp::Gt => a.v > b.v,
                    BinOp::Le => a.v <= b.v,
                    _ => a.v >= b.v,
                };
                (i128::from(t), Type::Uint(1))
            }
            // Shifts and concatenation returned above.
            BinOp::Shl | BinOp::Shr | BinOp::Concat => {
                return Err(self.err(pos, "internal error: unexpected operator in constant"))
            }
        };
        Ok(Val { v, ty })
    }
}

/// Replace every integer constant's expression with its value.
///
/// # Errors
/// A [`CompileError`] for a constant whose expression can't be computed, has
/// the wrong type, or depends on itself.
pub fn fold(program: &mut Program) -> Result<(), CompileError> {
    let snapshot = program.clone();
    let consts: HashMap<&str, &ConstItem> = snapshot
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Const(c) => Some((c.name.as_str(), c)),
            _ => None,
        })
        .collect();
    let mut folder = Folder {
        consts,
        done: HashMap::new(),
        active: Vec::new(),
        ctx: String::new(),
    };
    for item in &mut program.items {
        let Item::Const(c) = item else {
            continue;
        };
        if !matches!(c.ty, TypeRef::Scalar(_)) {
            continue;
        }
        let Val { v, ty } = folder.constant(&c.name, c.pos)?;
        let lit = Expr::new(ExprKind::Literal(u128::from(to_bits(v, ty))), c.value.pos);
        c.value = if ty.is_signed() {
            // The bit pattern, reinterpreted: what a hex literal with an
            // `_iN` suffix means.
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
