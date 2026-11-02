//! Parser for the physical pin-map file (`.pins`) — DESIGN §7.6.
//!
//! The pin map is a small, line-oriented config (a superset of the nextpnr PCF
//! idiom), *not* part of the `.snt` grammar, so it is parsed here by hand rather
//! than by the `pest` grammar. Each non-blank, non-comment line is:
//!
//! ```text
//! set_io <pin#> <dir> <path> [low] [io_type=<std>] [freq=<n>MHz]
//! ```
//!
//! where `<dir>` is `clock` | `in` | `out`, `<path>` addresses the clock-root
//! `tick` signature (an input parameter like `btn.inc`, or a state field with
//! bit/array indices like `seg[0][6]`), and the optional `low` flag marks an
//! active-low board signal. `io_type=` names the pin's I/O standard and
//! `freq=` (on a `clock` line) the board clock's frequency; both go only into
//! an ECP5 LPF (LPF1, DESIGN §9.15). The compiler ([`crate::board`]) resolves
//! each path against the elaborated [`crate::ir::Module`] and synthesizes a
//! top-level wrapper plus a concrete PCF or LPF.

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
    /// The I/O standard (`io_type=LVCMOS33`), for an LPF's `IOBUF`.
    pub io_type: Option<String>,
    /// A `clock` pin's frequency in MHz, as written (`freq=25MHz` → `"25"`),
    /// for an LPF's `FREQUENCY PORT`.
    pub freq_mhz: Option<String>,
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
        let err = |msg: String| {
            CompileError::semantic(
                source,
                Pos {
                    line,
                    col: 1,
                    file: 0,
                },
                msg,
            )
        };

        let tokens: Vec<&str> = content.split_whitespace().collect();
        if tokens[0] != "set_io" {
            return Err(err(format!(
                "expected a `set_io` directive, found `{}`",
                tokens[0]
            )));
        }
        if tokens.len() < 4 {
            return Err(err(
                "`set_io` needs `<pin#> <dir> <path> [low] [io_type=<std>] [freq=<n>MHz]`"
                    .to_string(),
            ));
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
        let mut io_type = None;
        let mut freq_mhz = None;
        for extra in &tokens[4..] {
            if *extra == "low" {
                active_low = true;
            } else if let Some(std) = extra.strip_prefix("io_type=") {
                if std.is_empty() || !std.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(err(format!(
                        "`io_type=` takes an I/O standard such as `LVCMOS33`, not `{std}`"
                    )));
                }
                io_type = Some(std.to_string());
            } else if let Some(f) = extra.strip_prefix("freq=") {
                freq_mhz = Some(parse_mhz(f).ok_or_else(|| {
                    err(format!(
                        "`freq=` takes a frequency in MHz such as `25MHz`, not `{f}`"
                    ))
                })?);
            } else {
                return Err(err(format!("unexpected trailing token `{extra}`")));
            }
        }
        if active_low && dir == PinDir::Clock {
            return Err(err("a `clock` pin cannot be marked `low`".to_string()));
        }
        if freq_mhz.is_some() && dir != PinDir::Clock {
            return Err(err("only a `clock` pin takes `freq=`".to_string()));
        }

        Ok(PinBinding {
            pin,
            dir,
            path,
            active_low,
            io_type,
            freq_mhz,
            line,
        })
    }
}

/// The number in `<n>MHz` (case-insensitive unit): digits with at most one
/// `.`, positive.
fn parse_mhz(text: &str) -> Option<String> {
    let n = text
        .strip_suffix("MHz")
        .or_else(|| text.strip_suffix("mhz"))
        .or_else(|| text.strip_suffix("MHZ"))?;
    let digits = n.chars().filter(char::is_ascii_digit).count();
    let dots = n.chars().filter(|&c| c == '.').count();
    let ok = digits > 0
        && digits + dots == n.len()
        && dots <= 1
        && !n.starts_with('.')
        && !n.ends_with('.')
        && n.chars().any(|c| c.is_ascii_digit() && c != '0');
    ok.then(|| n.to_string())
}
