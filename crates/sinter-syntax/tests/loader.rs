//! Tests for the multi-file project loader (DESIGN §10, milestone S1).
//!
//! Fixtures live under `tests/fixtures/`. The loader resolves external `mod`
//! declarations into a module tree following the Rust-2018 file layout.

use std::path::{Path, PathBuf};

use sinter_syntax::{load_project, load_project_sources, Item, LoadError, ModuleTree};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel)
}

fn child<'a>(tree: &'a ModuleTree, name: &str) -> &'a ModuleTree {
    tree.children
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| {
            let have: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
            panic!("no child module `{name}` (have {have:?})")
        })
}

#[test]
fn loads_rust_2018_module_tree() {
    let root = load_project(&fixture("proj/main.snt")).expect("load project");
    assert_eq!(root.name, "main");

    // Submodules appear in source order: external `alpha`, `beta`, inline.
    let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta", "inline"]);

    // The root keeps its non-`mod` items (the `Top` struct).
    assert_eq!(root.items.len(), 1, "root should keep the Top struct");

    // `alpha`'s child `gamma` is loaded from the `alpha/` subdirectory.
    let alpha = child(&root, "alpha");
    let gamma = child(alpha, "gamma");
    assert!(
        gamma.source.replace('\\', "/").ends_with("alpha/gamma.snt"),
        "gamma loaded from wrong file: {}",
        gamma.source
    );

    // The inline module shares the parent's source file and carries its item.
    let inline = child(&root, "inline");
    assert!(inline.source.replace('\\', "/").ends_with("proj/main.snt"));
    assert!(inline.children.is_empty());
    assert_eq!(
        inline.items.len(),
        1,
        "inline mod should hold its `here` fn"
    );
}

#[test]
fn missing_module_file_is_an_error() {
    let err = load_project(&fixture("broken/main.snt")).expect_err("should fail");
    match err {
        LoadError::MissingModule {
            name, candidate, ..
        } => {
            assert_eq!(name, "nope");
            assert!(candidate.ends_with("nope.snt"), "candidate: {candidate}");
        }
        other => panic!("expected MissingModule, got: {other}"),
    }
}

#[test]
fn duplicate_submodule_is_an_error() {
    let err = load_project(&fixture("dup/main.snt")).expect_err("should fail");
    match err {
        LoadError::DuplicateModule { name, .. } => assert_eq!(name, "a"),
        other => panic!("expected DuplicateModule, got: {other}"),
    }
}

/// Every file read gets an index, and each module's item positions carry the
/// index of the file they were parsed from (inline modules share their
/// parent's), so diagnostics can name the right file.
#[test]
fn positions_carry_their_source_file_index() {
    let (root, files) = load_project_sources(&fixture("proj/main.snt")).expect("load project");
    let paths: Vec<String> = files
        .iter()
        .map(|f| {
            Path::new(&f.path)
                .strip_prefix(fixture("proj"))
                .expect("fixture-relative path")
                .display()
                .to_string()
        })
        .collect();
    assert_eq!(
        paths,
        ["main.snt", "alpha.snt", "alpha/gamma.snt", "beta.snt"]
    );

    fn item_files(tree: &ModuleTree) -> Vec<u32> {
        tree.items
            .iter()
            .filter_map(|it| match it {
                Item::Struct(s) => Some(s.pos.file),
                Item::Func(f) => Some(f.pos.file),
                Item::Enum(e) => Some(e.pos.file),
                _ => None,
            })
            .collect()
    }
    for (tree, file) in [
        (&root, 0),
        (child(&root, "alpha"), 1),
        (child(child(&root, "alpha"), "gamma"), 2),
        (child(&root, "beta"), 3),
        (child(&root, "inline"), 0),
    ] {
        let found = item_files(tree);
        assert!(
            !found.is_empty(),
            "`{}` has no struct / fn items",
            tree.name
        );
        assert!(
            found.iter().all(|&f| f == file),
            "`{}` items carry files {found:?}, expected {file}",
            tree.name
        );
        assert_eq!(
            files[file as usize].path, tree.source,
            "`{}` source",
            tree.name
        );
    }
}
