//! The tree-sitter grammar parses every program the compiler's parser accepts
//! (DESIGN §7.1): the parser corpus's `pass/` cases and every example.

use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser, Query};

fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sinter::LANGUAGE.into())
        .expect("load the Sinter grammar");
    parser
}

/// Every `.snt` file under `dir`, recursively.
fn snt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            snt_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "snt") {
            out.push(path);
        }
    }
}

/// The first ERROR or MISSING node under `node`, as `line:col kind`.
fn first_error(node: Node) -> Option<String> {
    if node.is_error() || node.is_missing() {
        let p = node.start_position();
        return Some(format!("{}:{} {}", p.row + 1, p.column + 1, node.kind()));
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    children.into_iter().find_map(first_error)
}

#[test]
fn parses_the_corpus_and_every_example() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    snt_files(
        &root.join("crates/sinter-syntax/tests/corpus/pass"),
        &mut files,
    );
    snt_files(&root.join("examples"), &mut files);
    assert!(files.len() > 50, "found only {} files", files.len());

    let mut parser = parser();
    let mut failures = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("read source");
        let tree = parser.parse(&src, None).expect("parse");
        if let Some(at) = first_error(tree.root_node()) {
            failures.push(format!("{}:{at}", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "tree-sitter failed to parse:\n{}",
        failures.join("\n")
    );
}

#[test]
fn rejects_the_removed_module_form() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let src = std::fs::read_to_string(
        root.join("crates/sinter-syntax/tests/corpus/fail/legacy_module.snt"),
    )
    .expect("read source");
    let tree = parser().parse(&src, None).expect("parse");
    assert!(tree.root_node().has_error());
}

#[test]
fn the_highlight_query_compiles() {
    Query::new(
        &tree_sitter_sinter::LANGUAGE.into(),
        tree_sitter_sinter::HIGHLIGHTS_QUERY,
    )
    .expect("highlights.scm is valid for this grammar");
}
