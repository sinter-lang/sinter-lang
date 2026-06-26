//! Parser for the physical pin-map file (`.pins`) — DESIGN §7.6.
//!
//! The pin map is a small, line-oriented config (a superset of the nextpnr PCF
//! idiom), *not* part of the `.snt` grammar, so it is parsed here by hand rather
//! than by the `pest` grammar. Each non-blank, non-comment line is:
//!
//! ```text
//! set_io <pin#> <dir> <path> [low]
//! ```
//!
//! where `<dir>` is `clock` | `in` | `out`, `<path>` addresses the clock-root
//! `tick` signature (an input parameter like `btn.inc`, or a state field with
//! bit/array indices like `seg[0][6]`), and the optional `low` flag marks an
//! active-low board signal. The compiler ([`crate::board`]) resolves each path
//! against the elaborated [`crate::ir::Module`] and synthesizes a top-level
//! wrapper plus a concrete PCF.

use sinter_syntax::ast::Pos;

use crate::error::CompileError;

/// The direction of a pin binding (DESIGN §7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDir {
    /// A clock pin, bound to a clock *domain* rather than a signal.
    Clock,
    /// An input: a physical pin drives an input of the transition function.
    In,
    /// An output: a state/observation bit drives a physical pin.
    Out,
}

/// One `set_io` binding parsed from a pin-map file.
#[derive(Debug, Clone)]
pub struct PinBinding {
    /// The physical pin identifier (e.g. `"15"`), passed through verbatim to the
    /// generated PCF.
    pub pin: String,
    /// The binding direction.
    pub dir: PinDir,
    /// The unresolved path (`"sys"`, `"btn.inc"`, `"seg[0][6]"`), resolved later
    /// against the elaborated module.
    pub path: String,
    /// Whether the board signal is active-low (polarity is inverted once at the
    /// pin boundary so the source logic stays active-high).
    pub active_low: bool,
    /// 1-based line number in the source file, for diagnostics.
    pub line: usize,
}

/// A parsed pin-map file (DESIGN §7.6).
#[derive(Debug, Clone)]
pub struct PinMap {
    /// The bindings, in source order (the wrapper ports and PCF preserve it).
    pub bindings: Vec<PinBinding>,
    /// A label for the file, used in diagnostics and generated-file headers.
    pub source: String,
}

impl PinMap {
    /// Parse a pin-map file's `text`. `source` labels the file in diagnostics.
    ///
    /// # Errors
    /// Returns a [`CompileError`] on any malformed line.
    pub fn parse(text: &str, source: &str) -> Result<Self, CompileError> {
        let mut bindings = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            // Strip a trailing `# comment` and surrounding whitespace; skip
            // blank / comment-only lines.
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            bindings.push(Self::parse_line(content, source, line)?);
        }
        Ok(PinMap {
            bindings,
            source: source.to_string(),
        })
    }

    fn parse_line(content: &str, source: &str, line: usize) -> Result<PinBinding, CompileError> {
        let err = |msg: String| CompileError::semantic(source, Pos { line, col: 1 }, msg);

        let tokens: Vec<&str> = content.split_whitespace().collect();
        if tokens[0] != "set_io" {
            return Err(err(format!(
                "expected a `set_io` directive, found `{}`",
                tokens[0]
            )));
        }
        if tokens.len() < 4 {
            return Err(err("`set_io` needs `<pin#> <dir> <path> [low]`".to_string()));
        }
        let pin = tokens[1].to_string();
        let dir = match tokens[2] {
            "clock" => PinDir::Clock,
            "in" => PinDir::In,
            "out" => PinDir::Out,
            other => {
                return Err(err(format!(
                    "unknown direction `{other}` (expected `clock`, `in`, or `out`)"
                )))
            }
        };
        let path = tokens[3].to_string();

        let mut active_low = false;
        for extra in &tokens[4..] {
            if *extra == "low" {
                active_low = true;
            } else {
                return Err(err(format!("unexpected trailing token `{extra}`")));
            }
        }
        if active_low && dir == PinDir::Clock {
            return Err(err("a `clock` pin cannot be marked `low`".to_string()));
        }

        Ok(PinBinding {
            pin,
            dir,
            path,
            active_low,
            line,
        })
    }
}
