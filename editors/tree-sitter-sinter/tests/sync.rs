//! The editor grammars stay in step with the compiler: every reserved word in
//! the pest grammar is highlighted by both, and both editor packages carry
//! the workspace's version.

use std::path::Path;

fn read(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The quoted words of the pest grammar's `keyword` rule.
fn reserved_words() -> Vec<String> {
    let pest = read("crates/sinter-syntax/src/grammar/sinter.pest");
    let start = pest.find("keyword = @{").expect("pest `keyword` rule");
    let body = &pest[start..start + pest[start..].find('}').expect("end of rule")];
    // The rule ends with a `!(ASCII_ALPHANUMERIC | "_")` word boundary.
    body.split('"')
        .skip(1)
        .step_by(2)
        .filter(|w| w.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string)
        .collect()
}

#[test]
fn both_editor_grammars_know_every_reserved_word() {
    let words = reserved_words();
    assert!(words.len() > 15, "parsed only {words:?}");
    let tm = read("editors/vscode/syntaxes/sinter.tmLanguage.json");
    let ts = read("editors/tree-sitter-sinter/grammar.js");
    let hl = read("editors/tree-sitter-sinter/queries/highlights.scm");
    for w in &words {
        // A regex alternative (`(a|b)`) or a whole word (`\\bw\\b`).
        let in_tm = [
            format!("|{w}|"),
            format!("({w}|"),
            format!("|{w})"),
            format!("\\\\b{w}\\\\b"),
        ]
        .iter()
        .any(|pat| tm.contains(pat.as_str()));
        assert!(in_tm, "the VS Code grammar doesn't highlight `{w}`");
        // A token of the grammar, or a word the highlight query names (a
        // built-in type name, or a reserved word with no syntax yet).
        let in_ts = ts.contains(&format!("'{w}'")) || hl.contains(w.as_str());
        assert!(in_ts, "the tree-sitter grammar doesn't highlight `{w}`");
    }
}

#[test]
fn editor_packages_carry_the_workspace_version() {
    let version = env!("CARGO_PKG_VERSION");
    for rel in [
        "editors/vscode/package.json",
        "editors/tree-sitter-sinter/tree-sitter.json",
    ] {
        assert!(
            read(rel).contains(&format!("\"version\": \"{version}\"")),
            "{rel} should have version {version}"
        );
    }
}
