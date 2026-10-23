//! Directed stimulus: CSV input vectors with optional expected outputs.
//!
//! The format (see README "Directed stimulus") is one header row naming
//! columns, then one data row per clock cycle:
//!
//! ```text
//! # comments and blank lines are ignored
//! en, expect.value_q
//! 1,  0
//! 1,  1
//! 0,  -        # `-` or an empty cell: don't check this output this cycle
//! ```
//!
//! A column is either an **input port** name, or `expect.` + an **output
//! port** name. Input ports with no column are driven `0` every cycle. Values
//! are decimal (a leading `-` only for signed ports), `0x` hex, or `0b`
//! binary, with optional `_` separators; each must fit the port's type.
//!
//! The parsed inputs have the same shape [`crate::sim::gen_stimulus`] returns,
//! so a directed run feeds [`crate::sim::run_reference`] and the Verilator
//! harness unchanged — the §7.4 oracle covers directed tests too.

use std::collections::HashMap;
use std::fmt;

use sinter_syntax::ast::Type;

use crate::error::{mask, signed_value, umax};
use crate::ir::Module;

/// Column-name prefix that marks an expected-output column.
const EXPECT_PREFIX: &str = "expect.";

/// An error in a stimulus file, with the 1-based line it occurred on.
#[derive(Debug, thiserror::Error)]
pub enum StimulusError {
    /// The file has no header row (it is empty or all comments).
    #[error("stimulus file is empty: expected a header row naming the columns")]
    Empty,
    /// A malformed header, row, or value.
    #[error("line {line}: {msg}")]
    Invalid {
        /// 1-based source line.
        line: usize,
        /// Human-readable message.
        msg: String,
    },
}

/// A parsed directed stimulus for one [`Module`].
#[derive(Debug, Clone)]
pub struct Stimulus {
    /// One row per cycle, one value per input port in declaration order.
    pub inputs: Vec<Vec<u64>>,
    /// One row per cycle, one entry per output port in declaration order;
    /// `None` means the output is not checked that cycle.
    pub expects: Vec<Vec<Option<u64>>>,
    /// The 1-based source line of each data row (for mismatch reports).
    pub lines: Vec<usize>,
    /// Input ports the file has no column for (driven `0` every cycle).
    pub defaulted: Vec<String>,
}

/// One expected output that did not match the simulated value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// The 0-based cycle.
    pub cycle: usize,
    /// The 1-based stimulus-file line of that cycle's row.
    pub line: usize,
    /// The output port name.
    pub port: String,
    /// The port's type (for formatting values).
    pub ty: Type,
    /// The expected bit pattern.
    pub expected: u64,
    /// The simulated bit pattern.
    pub actual: u64,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cycle {} (line {}): `{}` expected {}, got {}",
            self.cycle,
            self.line,
            self.port,
            format_value(self.expected, self.ty),
            format_value(self.actual, self.ty)
        )
    }
}

/// Format a port value for display: signed ports as (possibly negative)
/// decimal, unsigned ports as plain decimal.
#[must_use]
pub fn format_value(bits: u64, ty: Type) -> String {
    signed_value(mask(bits, ty.width()), ty).to_string()
}

/// What one CSV column drives or checks.
#[derive(Clone, Copy)]
enum Column {
    /// Index into `module.inputs()`.
    Input(usize),
    /// Index into `module.outputs()`.
    Expect(usize),
}

impl Stimulus {
    /// Parse a CSV stimulus against `module`'s ports.
    ///
    /// # Errors
    /// Returns a [`StimulusError`] for an empty file, an unknown or duplicate
    /// column, a row with the wrong number of cells, or a value that is
    /// malformed or does not fit its port's type.
    pub fn parse_csv(module: &Module, text: &str) -> Result<Self, StimulusError> {
        let inputs: Vec<_> = module.inputs().collect();
        let outputs: Vec<_> = module.outputs().collect();

        let mut rows = text
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, strip_comment(l).trim()))
            .filter(|(_, l)| !l.is_empty());

        let (header_line, header) = rows.next().ok_or(StimulusError::Empty)?;
        let invalid = |line: usize, msg: String| StimulusError::Invalid { line, msg };

        let mut columns = Vec::new();
        let mut seen: HashMap<&str, ()> = HashMap::new();
        for name in header.split(',').map(str::trim) {
            if seen.insert(name, ()).is_some() {
                return Err(invalid(header_line, format!("duplicate column `{name}`")));
            }
            let col = if let Some(out) = name.strip_prefix(EXPECT_PREFIX) {
                outputs
                    .iter()
                    .position(|p| p.name == out)
                    .map(Column::Expect)
                    .ok_or_else(|| {
                        invalid(
                            header_line,
                            format!(
                                "`{name}` names no output port; outputs are: {}",
                                join_names(outputs.iter().map(|p| p.name.as_str()))
                            ),
                        )
                    })?
            } else {
                inputs
                    .iter()
                    .position(|p| p.name == name)
                    .map(Column::Input)
                    .ok_or_else(|| {
                        invalid(
                            header_line,
                            format!(
                                "`{name}` names no input port (prefix outputs with \
                                 `{EXPECT_PREFIX}`); inputs are: {}",
                                join_names(inputs.iter().map(|p| p.name.as_str()))
                            ),
                        )
                    })?
            };
            columns.push(col);
        }

        let defaulted = inputs
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                !columns
                    .iter()
                    .any(|c| matches!(c, Column::Input(j) if j == i))
            })
            .map(|(_, p)| p.name.clone())
            .collect();

        let mut stim = Stimulus {
            inputs: Vec::new(),
            expects: Vec::new(),
            lines: Vec::new(),
            defaulted,
        };
        for (line, row) in rows {
            let cells: Vec<&str> = row.split(',').map(str::trim).collect();
            if cells.len() != columns.len() {
                return Err(invalid(
                    line,
                    format!(
                        "expected {} cells (one per header column), found {}",
                        columns.len(),
                        cells.len()
                    ),
                ));
            }
            let mut in_row = vec![0; inputs.len()];
            let mut exp_row = vec![None; outputs.len()];
            for (col, cell) in columns.iter().zip(&cells) {
                match *col {
                    Column::Input(i) => {
                        let p = inputs[i];
                        if cell.is_empty() || *cell == "-" {
                            return Err(invalid(
                                line,
                                format!("input `{}` needs a value every cycle", p.name),
                            ));
                        }
                        in_row[i] = parse_value(cell, p.ty)
                            .map_err(|m| invalid(line, format!("input `{}`: {m}", p.name)))?;
                    }
                    Column::Expect(o) => {
                        let p = outputs[o];
                        if !cell.is_empty() && *cell != "-" {
                            exp_row[o] =
                                Some(parse_value(cell, p.ty).map_err(|m| {
                                    invalid(line, format!("output `{}`: {m}", p.name))
                                })?);
                        }
                    }
                }
            }
            stim.inputs.push(in_row);
            stim.expects.push(exp_row);
            stim.lines.push(line);
        }
        Ok(stim)
    }

    /// The number of cycles (data rows).
    #[must_use]
    pub fn cycles(&self) -> usize {
        self.inputs.len()
    }

    /// The number of output values the file checks, over all cycles.
    #[must_use]
    pub fn expectation_count(&self) -> usize {
        self.expects
            .iter()
            .flatten()
            .filter(|e| e.is_some())
            .count()
    }

    /// Compare cycle `cycle`'s simulated `outputs` (as returned by
    /// [`crate::sim::Simulator::step`]) against its expectations.
    #[must_use]
    pub fn check_cycle(
        &self,
        module: &Module,
        cycle: usize,
        outputs: &HashMap<String, u64>,
    ) -> Vec<Mismatch> {
        let Some(expects) = self.expects.get(cycle) else {
            return Vec::new();
        };
        module
            .outputs()
            .zip(expects)
            .filter_map(|(p, exp)| {
                let expected = (*exp)?;
                let actual = mask(outputs.get(&p.name).copied().unwrap_or(0), p.ty.width());
                (actual != expected).then(|| Mismatch {
                    cycle,
                    line: self.lines[cycle],
                    port: p.name.clone(),
                    ty: p.ty,
                    expected,
                    actual,
                })
            })
            .collect()
    }
}

/// Drop a trailing `# comment` from a line.
fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(code, _)| code)
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<_> = names.map(|n| format!("`{n}`")).collect();
    if v.is_empty() {
        "(none)".to_string()
    } else {
        v.join(", ")
    }
}

/// Parse one cell as a value of type `ty`, returning its bit pattern.
///
/// Decimal values are range-checked against the type (`-8..=7` for `i4`);
/// `0x` / `0b` values are raw bit patterns and must fit in the width.
fn parse_value(cell: &str, ty: Type) -> Result<u64, String> {
    let text: String = cell.chars().filter(|&c| c != '_').collect();
    let width = ty.width();
    let radix = |digits: &str, base: u32| {
        u64::from_str_radix(digits, base).map_err(|_| format!("`{cell}` is not a valid number"))
    };

    let lower = text.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix("0x") {
        return fit_bits(radix(digits, 16)?, ty, cell);
    }
    if let Some(digits) = lower.strip_prefix("0b") {
        return fit_bits(radix(digits, 2)?, ty, cell);
    }

    let value: i128 = text
        .parse()
        .map_err(|_| format!("`{cell}` is not a valid number"))?;
    let (min, max) = if ty.is_signed() {
        let half = 1i128 << (width - 1);
        (-half, half - 1)
    } else {
        (0, i128::from(umax(width)))
    };
    if value < min || value > max {
        return Err(format!(
            "{cell} is out of range for `{ty}` ({min}..={max}); \
             use 0x / 0b for a raw bit pattern"
        ));
    }
    // Two's-complement encode, then keep the low `width` bits.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(mask(value as u64, width))
}

/// Check a raw `0x` / `0b` bit pattern fits in `ty`'s width.
fn fit_bits(bits: u64, ty: Type, cell: &str) -> Result<u64, String> {
    if bits > umax(ty.width()) {
        return Err(format!(
            "{cell} does not fit in the {} bits of `{ty}`",
            ty.width()
        ));
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_str;

    const COUNTER: &str = "
        struct C { pub value: u8 = 0, pub acc: i4 = 0, }
        impl C {
            #[clock(domain = sys)]
            fn tick(self, en: u1, x: i4) -> C {
                C {
                    value: if en == 1 { (self.value + 1) as u8 wrap } else { self.value },
                    acc: x,
                }
            }
        }
    ";

    fn module() -> Module {
        let design = compile_str(COUNTER).expect("test source compiles");
        design.modules.into_iter().next().expect("one module")
    }

    fn err(text: &str) -> String {
        Stimulus::parse_csv(&module(), text)
            .expect_err("stimulus should be rejected")
            .to_string()
    }

    #[test]
    fn parses_inputs_expects_comments_and_radixes() {
        let text = "\
# header follows
en, x, expect.value_q
1, -8, 0   # first cycle

0b1, 0x7, -
0, -0_1,
";
        let s = Stimulus::parse_csv(&module(), text).expect("parses");
        assert_eq!(s.cycles(), 3);
        // x: i4, so -8 -> 0b1000 and -1 -> 0b1111.
        assert_eq!(s.inputs, vec![vec![1, 8], vec![1, 7], vec![0, 15]]);
        assert_eq!(s.lines, vec![3, 5, 6]);
        assert_eq!(s.expectation_count(), 1);
        assert!(s.defaulted.is_empty());
    }

    #[test]
    fn missing_input_columns_default_to_zero() {
        let s = Stimulus::parse_csv(&module(), "en\n1\n").expect("parses");
        assert_eq!(s.inputs, vec![vec![1, 0]]);
        assert_eq!(s.defaulted, vec!["x".to_string()]);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(err("# only a comment\n").contains("empty"));
        assert!(err("en, bogus\n").contains("`bogus` names no input port"));
        assert!(err("en, expect.nope\n").contains("`expect.nope` names no output port"));
        assert!(err("en, en\n").contains("duplicate column `en`"));
        assert!(err("en, x\n1\n").contains("line 2: expected 2 cells"));
        assert!(err("en, x\n1, \n").contains("input `x` needs a value"));
        assert!(err("en\n2\n").contains("out of range for `u1`"));
        assert!(err("x\n8\n").contains("out of range for `i4` (-8..=7)"));
        assert!(err("en\n-1\n").contains("out of range"));
        assert!(err("x\n0x10\n").contains("does not fit in the 4 bits"));
        assert!(err("en\nabc\n").contains("not a valid number"));
    }

    #[test]
    fn check_cycle_reports_mismatches() {
        let m = module();
        let s = Stimulus::parse_csv(&m, "x, expect.acc_q\n0, -1\n").expect("parses");
        let mut outputs = HashMap::new();
        outputs.insert("acc_q".to_string(), 15);
        assert!(s.check_cycle(&m, 0, &outputs).is_empty());
        outputs.insert("acc_q".to_string(), 2);
        let mm = s.check_cycle(&m, 0, &outputs);
        assert_eq!(mm.len(), 1);
        assert_eq!(
            mm[0].to_string(),
            "cycle 0 (line 2): `acc_q` expected -1, got 2"
        );
    }
}
