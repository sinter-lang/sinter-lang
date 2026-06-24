//! Compiler error types.

use sinter_syntax::ast::{Pos, Type};
use sinter_syntax::ParseError;

/// An error produced while checking or lowering a Sinter program.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// The source failed to parse.
    #[error(transparent)]
    Parse(#[from] ParseError),

    /// A semantic error, with a source position.
    #[error("{module}:{}:{}: {msg}", pos.line, pos.col)]
    Semantic {
        /// The module in which the error occurred.
        module: String,
        /// Source position.
        pos: Pos,
        /// Human-readable message.
        msg: String,
    },
}

impl CompileError {
    /// Build a semantic error.
    pub(crate) fn semantic(module: &str, pos: Pos, msg: impl Into<String>) -> Self {
        CompileError::Semantic {
            module: module.to_string(),
            pos,
            msg: msg.into(),
        }
    }
}

/// Mask `value` to the low `width` bits.
#[must_use]
pub(crate) fn mask(value: u64, width: u32) -> u64 {
    if width >= 64 {
        value
    } else {
        value & ((1u64 << width) - 1)
    }
}

/// The maximum representable value of an unsigned type of `width` bits.
#[must_use]
pub(crate) fn umax(width: u32) -> u64 {
    mask(u64::MAX, width)
}

/// Interpret `bits` (masked to `ty.width()`) as a signed integer if `ty` is
/// signed, otherwise as unsigned. Returns an `i128` so all widths up to 64 fit.
#[must_use]
pub(crate) fn signed_value(bits: u64, ty: Type) -> i128 {
    let w = ty.width();
    if ty.is_signed() && w > 0 && (bits >> (w - 1)) & 1 == 1 {
        i128::from(bits) - (1i128 << w)
    } else {
        i128::from(bits)
    }
}
