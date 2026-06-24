//! Corpus-driven conformance tests for the Sinter surface grammar.
//!
//! Each `.snt` file under `tests/corpus/` is a spec-by-example: a small program
//! isolating one feature of the grammar. The corpus has two halves:
//!
//! * `corpus/pass/` — programs that must parse. Each `foo.snt` has a sibling
//!   golden `foo.ast` holding the pretty-printed [`Program`] it parses to, so
//!   the example documents *both* the surface syntax and exactly how the parser
//!   interprets it. A mismatch fails the test.
//! * `corpus/fail/` — programs that must be rejected. Each may carry a
//!   `//~ ERROR <substring>` annotation (in a comment, so it is ignored by the
//!   parser); the test asserts parsing fails *and* the error message contains
//!   that substring, so failures are attributable rather than incidental.
//!
//! Adding a case is just dropping in a `.snt` file — the harness discovers it.
//! After changing the grammar or AST, regenerate the `pass/` goldens with:
//!
//! ```text
//! SINTER_BLESS=1 cargo test -p sinter-syntax --test corpus
//! ```
//!
//! and review the resulting `.ast` diffs.

use std::fs;
use std::path::{Path, PathBuf};

use sinter_syntax::parse_program;

/// Absolute path to the `tests/corpus/<sub>` directory.
fn corpus_subdir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus")
        .join(sub)
}

/// Whether `SINTER_BLESS` is set, meaning goldens are rewritten, not checked.
fn blessing() -> bool {
    std::env::var_os("SINTER_BLESS").is_some()
}

/// Every `.snt` file directly under `dir`, sorted for deterministic ordering.
fn snt_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read corpus dir {}: {e}", dir.display()))
        .map(|entry| entry.expect("corpus dir entry").path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("snt"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .snt cases found in {}",
        dir.display()
    );
    files
}

#[test]
fn corpus_pass_cases_match_golden_ast() {
    let dir = corpus_subdir("pass");
    let mut failures = Vec::new();

    for path in snt_files(&dir) {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        let src = fs::read_to_string(&path).expect("read case source");

        let program = match parse_program(&src) {
            Ok(program) => program,
            Err(e) => {
                failures.push(format!("{name}: expected to parse, but failed: {e}"));
                continue;
            }
        };
        let actual = format!("{program:#?}\n");
        let golden_path = path.with_extension("ast");

        if blessing() {
            fs::write(&golden_path, &actual).expect("write golden ast");
            continue;
        }

        match fs::read_to_string(&golden_path) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => failures.push(format!(
                "{name}: AST does not match {}.\n--- expected ---\n{expected}\n--- actual ---\n{actual}",
                golden_path.display()
            )),
            Err(_) => failures.push(format!(
                "{name}: missing golden {}. Regenerate with SINTER_BLESS=1.",
                golden_path.display()
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "corpus pass failures:\n\n{}",
        failures.join("\n\n")
    );
}

#[test]
fn corpus_fail_cases_are_rejected() {
    let dir = corpus_subdir("fail");
    let mut failures = Vec::new();

    for path in snt_files(&dir) {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        let src = fs::read_to_string(&path).expect("read case source");

        match parse_program(&src) {
            Ok(_) => failures.push(format!("{name}: expected a parse error, but it parsed")),
            Err(e) => {
                let message = e.to_string();
                if let Some(expected) = expected_error(&src) {
                    if !message.contains(expected) {
                        failures.push(format!(
                            "{name}: error did not contain {expected:?}.\n  got: {message}"
                        ));
                    }
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "corpus fail failures:\n\n{}",
        failures.join("\n\n")
    );
}

/// Extract the substring from a `//~ ERROR <substring>` annotation, if present.
fn expected_error(src: &str) -> Option<&str> {
    src.lines()
        .find_map(|line| line.split_once("//~ ERROR ").map(|(_, rest)| rest.trim()))
}
