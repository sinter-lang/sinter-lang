//! Compiler error types.

use std::fmt;

use sinter_syntax::ast::{Pos, Type};
use sinter_syntax::{LoadError, ParseError, SourceFile};

/// An error produced while checking or lowering a Sinter program.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// The source failed to parse.
    #[error(transparent)]
    Parse(#[from] ParseError),

    /// A multi-file project failed to load (DESIGN §10).
    #[error(transparent)]
    Load(#[from] LoadError),

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

    /// A semantic error located in a project source file, rendered with the
    /// file path, the offending line, and a caret under the position.
    #[error("{0}")]
    InFile(Box<Located>),
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

    /// Replace synthesized type names in the message with their source
    /// spellings (`(synthesized, source)` pairs), whole words only.
    #[must_use]
    pub(crate) fn rename_types(self, names: &[(String, String)]) -> Self {
        if names.is_empty() {
            return self;
        }
        let rename = |msg: String| {
            names
                .iter()
                .fold(msg, |m, (from, to)| replace_word(&m, from, to))
        };
        match self {
            CompileError::Semantic { module, pos, msg } => CompileError::Semantic {
                module,
                pos,
                msg: rename(msg),
            },
            CompileError::InFile(mut l) => {
                l.msg = rename(std::mem::take(&mut l.msg));
                CompileError::InFile(l)
            }
            other => other,
        }
    }

    /// Attach source context from a project's `files` (indexed by
    /// [`Pos::file`]): a [`CompileError::Semantic`] whose position lies in one
    /// of them becomes a [`CompileError::InFile`]. Any other error, or one
    /// with a synthesized (line-0) position, is returned unchanged.
    #[must_use]
    pub fn with_sources(self, files: &[SourceFile]) -> Self {
        let CompileError::Semantic { module, pos, msg } = self else {
            return self;
        };
        let file = usize::try_from(pos.file).ok().and_then(|i| files.get(i));
        let line_text = pos
            .line
            .checked_sub(1)
            .and_then(|i| file.and_then(|f| f.text.lines().nth(i)));
        match (file, line_text) {
            (Some(file), Some(line_text)) => CompileError::InFile(Box::new(Located {
                path: file.path.clone(),
                line_text: line_text.to_string(),
                context: module,
                pos,
                msg,
            })),
            _ => CompileError::Semantic { module, pos, msg },
        }
    }
}

/// A semantic error with its source location resolved (see
/// [`CompileError::with_sources`]).
#[derive(Debug)]
pub struct Located {
    /// The source file's path.
    pub path: String,
    /// The full text of the offending line.
    pub line_text: String,
    /// The item being checked (a function, method, or struct name).
    pub context: String,
    /// The position within the file.
    pub pos: Pos,
    /// Human-readable message.
    pub msg: String,
}

/// Renders rustc-style:
///
/// ```text
/// src/alu.snt:10:20: type mismatch: expected `u8`, found `u9`
///    |
/// 10 |             Counter { value: self.value + 1 }
///    |                    ^
///    = in `bump`
/// ```
impl fmt::Display for Located {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line_no = self.pos.line.to_string();
        let gutter = " ".repeat(line_no.len());
        // Keep tabs so the caret lines up under the same visual column.
        let pad: String = self
            .line_text
            .chars()
            .take(self.pos.col.saturating_sub(1))
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        writeln!(
            f,
            "{}:{}:{}: {}",
            self.path, self.pos.line, self.pos.col, self.msg
        )?;
        writeln!(f, "{gutter} |")?;
        writeln!(f, "{line_no} | {}", self.line_text)?;
        writeln!(f, "{gutter} | {pad}^")?;
        write!(f, "{gutter} = in `{}`", self.context)
    }
}

/// `text` with every whole-word occurrence of `from` replaced by `to`.
fn replace_word(text: &str, from: &str, to: &str) -> String {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + from.len()..].chars().next();
        out.push_str(&rest[..i]);
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
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
