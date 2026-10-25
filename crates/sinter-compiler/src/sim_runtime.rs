//! Sinter's value semantics: the arithmetic every simulator shares.
//!
//! This file is self-contained on purpose. The compiler's interpreter
//! (`eval.rs`) is built on it, and `sinter build --emit rust` copies it verbatim
//! into every generated simulator crate as `src/runtime.rs`, so the interpreted
//! and the compiled models compute with the same code (DESIGN §7.2 / §7.4).
//!
//! Values are `u64` bit patterns masked to their type's width; every width is
//! at most 64 bits.

/// A scalar type: signedness and width in bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ty {
    /// Two's complement if true.
    pub signed: bool,
    /// Width in bits, 1 to 64.
    pub width: u32,
}

/// The unsigned type `uW`.
#[must_use]
pub const fn u(width: u32) -> Ty {
    Ty {
        signed: false,
        width,
    }
}

/// The signed type `iW`.
#[must_use]
pub const fn i(width: u32) -> Ty {
    Ty {
        signed: true,
        width,
    }
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Un {
    /// Logical not of a 1-bit value (`!`).
    Not,
    /// Bitwise complement (`~`).
    BitNot,
    /// Two's complement negation (`-`).
    Neg,
}

/// A binary operator. Shifts and concatenation have their own functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bin {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `&`
    And,
    /// `|`
    Or,
    /// `^`
    Xor,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
}

/// `value` masked to its low `width` bits.
#[inline]
#[must_use]
pub fn mask(value: u64, width: u32) -> u64 {
    if width >= 64 {
        value
    } else {
        value & ((1u64 << width) - 1)
    }
}

/// `bits` (masked to `ty`'s width) as an integer: two's complement if `ty` is
/// signed.
#[inline]
#[must_use]
pub fn signed_value(bits: u64, ty: Ty) -> i128 {
    let w = ty.width;
    if ty.signed && w > 0 && (bits >> (w - 1)) & 1 == 1 {
        i128::from(bits) - (1i128 << w)
    } else {
        i128::from(bits)
    }
}

/// Extend `v` of type `from` to `to_w` bits: sign-extend a signed value,
/// zero-extend an unsigned one. A no-op when `from` is already that wide.
#[inline]
fn ext(v: u64, from: Ty, to_w: u32) -> u64 {
    let fw = from.width;
    if from.signed && fw > 0 && fw < to_w && (v >> (fw - 1)) & 1 == 1 {
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

/// Apply a unary operator to `v` of type `ty`.
#[inline]
#[must_use]
pub fn unary(op: Un, v: u64, ty: Ty) -> u64 {
    match op {
        Un::Not => u64::from(v == 0),
        Un::BitNot => mask(!v, ty.width),
        Un::Neg => mask(v.wrapping_neg(), ty.width),
    }
}

/// Apply a binary operator. Arithmetic extends both operands to the result
/// type `ty` first, so a width-inferred `+` / `-` / `*` is exact; a comparison
/// yields 1 or 0, signed if its operands are.
#[inline]
#[must_use]
pub fn binary(op: Bin, l: u64, lty: Ty, r: u64, rty: Ty, ty: Ty) -> u64 {
    let w = ty.width;
    match op {
        Bin::Add => mask(ext(l, lty, w).wrapping_add(ext(r, rty, w)), w),
        Bin::Sub => mask(ext(l, lty, w).wrapping_sub(ext(r, rty, w)), w),
        Bin::Mul => mask(ext(l, lty, w).wrapping_mul(ext(r, rty, w)), w),
        Bin::And => mask(l & r, w),
        Bin::Or => mask(l | r, w),
        Bin::Xor => mask(l ^ r, w),
        Bin::Eq | Bin::Ne | Bin::Lt | Bin::Gt | Bin::Le | Bin::Ge => {
            let a = signed_value(l, lty);
            let b = signed_value(r, rty);
            u64::from(match op {
                Bin::Eq => a == b,
                Bin::Ne => a != b,
                Bin::Lt => a < b,
                Bin::Gt => a > b,
                Bin::Le => a <= b,
                _ => a >= b,
            })
        }
    }
}

/// `as T wrap`: resize `v` from `from` to `to`, extending by `from`'s
/// signedness when widening and dropping high bits when narrowing.
#[inline]
#[must_use]
pub fn wrap(v: u64, from: Ty, to: Ty) -> u64 {
    mask(ext(v, from, to.width), to.width)
}

/// `as T sat`: clamp `v` of type `from` to `to`'s range.
#[inline]
#[must_use]
pub fn sat(v: u64, from: Ty, to: Ty) -> u64 {
    let w = to.width;
    let (lo, hi) = if to.signed {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    };
    mask(signed_value(v, from).clamp(lo, hi) as u64, w)
}

/// Shift `v` left by `amount`, masked to `to_w` bits.
#[inline]
#[must_use]
pub fn shl(v: u64, to_w: u32, amount: u32) -> u64 {
    if amount >= 64 {
        0
    } else {
        mask(v << amount, to_w)
    }
}

/// Shift `v` of type `from` right by `amount`, masked to `to_w` bits:
/// arithmetic for a signed value, logical for an unsigned one.
#[inline]
#[must_use]
pub fn shr(v: u64, from: Ty, to_w: u32, amount: u32) -> u64 {
    if from.signed {
        mask((signed_value(v, from) >> amount.min(127)) as u64, to_w)
    } else if amount >= 64 {
        0
    } else {
        mask(v >> amount, to_w)
    }
}

/// A runtime (barrel) shift of `v` of type `ty` by `amount`, keeping the
/// width: left if `left`, else right (arithmetic for a signed value). An
/// amount of the width or more yields 0, or the sign fill.
#[inline]
#[must_use]
pub fn var_shift(v: u64, amount: u64, ty: Ty, left: bool) -> u64 {
    let sh = u32::try_from(amount).unwrap_or(u32::MAX);
    if left {
        shl(v, ty.width, sh)
    } else {
        shr(v, ty, ty.width, sh)
    }
}

/// Write `data` into `cell` under enable `we`: the whole word when `lanes` is
/// 1, else only the byte lanes whose enable bit is set.
#[inline]
pub fn write_lanes(cell: &mut u64, we: u64, data: u64, lanes: u32) {
    if lanes == 1 {
        *cell = data;
    } else {
        let keep = (0..lanes)
            .filter(|i| (we >> i) & 1 == 1)
            .fold(0u64, |m, i| m | (0xFF << (8 * i)));
        *cell = (*cell & !keep) | (data & keep);
    }
}
