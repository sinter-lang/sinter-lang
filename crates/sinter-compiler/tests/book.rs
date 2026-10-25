//! The book's code samples compile (`docs/book/`, ROADMAP v0.9).
//!
//! Every ```` ```sinter ```` block under `docs/book/src/` is a whole program
//! that must compile once its hidden lines are restored. It is compiled as
//! `sinter` compiles a file, so it may contain inline `mod`s. A hidden line starts
//! with `~`, which mdBook strips from the rendered page (`hidelines` in
//! `book.toml`), so a page can show a fragment while the test compiles the
//! program around it. Comma-separated flags after `sinter` change that:
//!
//! - `ignore`: not compiled (a syntax sketch, or a fragment with no context).
//! - `fail`: must be rejected. The next code block on the page must be a
//!   ```` ```text ```` block whose first line, minus a leading `error: `, is
//!   part of the compiler's message, so the error shown is the error given.
//! - `project=NAME,file=PATH`: one file of a multi-file project. All blocks of
//!   project `NAME` on a page (of any language, so a `.hex` file can join) are
//!   written to a scratch directory, and the first `.snt` file is compiled as
//!   the project root. The flags of that first block apply to the project.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sinter_compiler::compile_project;

/// A fenced code block.
struct Block {
    /// `page:line` of the opening fence, for failure messages.
    at: String,
    lang: String,
    ignore: bool,
    fail: bool,
    project: Option<String>,
    file: Option<String>,
    /// The code with hidden lines restored.
    code: String,
}

fn book_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/book/src")
}

/// Every `.md` page under `dir`, recursively.
fn pages(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read a book directory") {
        let path = entry.expect("read a book directory entry").path();
        if path.is_dir() {
            pages(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// The fenced code blocks of one page. A fence may be indented (inside a list
/// item); its indentation is removed from the code.
fn blocks(name: &str, text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((n, line)) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let indent = line.len() - trimmed.len();
        let mut words = info.split(',').map(str::trim);
        let lang = words.next().unwrap_or_default().to_string();
        let mut block = Block {
            at: format!("{name}:{}", n + 1),
            lang,
            ignore: false,
            fail: false,
            project: None,
            file: None,
            code: String::new(),
        };
        for word in words {
            match word.split_once('=') {
                Some(("project", v)) => block.project = Some(v.to_string()),
                Some(("file", v)) => block.file = Some(v.to_string()),
                _ if word == "ignore" => block.ignore = true,
                _ if word == "fail" => block.fail = true,
                _ if block.lang != "sinter" => {}
                _ => panic!("{}: unknown code block flag `{word}`", block.at),
            }
        }
        for (_, line) in lines.by_ref() {
            if line.trim_start() == "```" {
                break;
            }
            let line = line.get(indent..).unwrap_or(line.trim_start());
            // mdBook hides a line whose first non-blank character is `~` and
            // drops that `~` when the line is shown.
            let line = match line.trim_start().strip_prefix('~') {
                Some(rest) => format!("{}{rest}", &line[..line.len() - line.trim_start().len()]),
                None => line.to_string(),
            };
            block.code.push_str(&line);
            block.code.push('\n');
        }
        out.push(block);
    }
    out
}

/// Check one compile result against what its block expects; `next` is the
/// block after it on the page, which a `fail` block's expected error is in.
fn check(
    block: &Block,
    next: Option<&Block>,
    result: Result<(), String>,
    failures: &mut Vec<String>,
) {
    match (block.fail, result) {
        (false, Ok(())) => {}
        (false, Err(e)) => failures.push(format!("{}: does not compile:\n{e}", block.at)),
        (true, Ok(())) => failures.push(format!("{}: compiles, but is marked `fail`", block.at)),
        (true, Err(e)) => {
            let expected = next
                .filter(|b| b.lang == "text")
                .and_then(|b| b.code.lines().next())
                .map(|l| l.trim().trim_start_matches("error: "));
            match expected {
                Some(want) if e.contains(want) => {}
                Some(want) => failures.push(format!(
                    "{}: fails with a different error\n  expected: {want}\n  got: {e}",
                    block.at
                )),
                None => failures.push(format!(
                    "{}: a `fail` block must be followed by a ```text block holding its error",
                    block.at
                )),
            }
        }
    }
}

/// Write a project's files (`(path, code)`) to a scratch directory and
/// compile its root, the first `.snt` file.
fn compile_files(dir: &Path, files: &[(&str, &str)]) -> Result<(), String> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("clearing {}: {e}", dir.display()))?;
    }
    let mut root = None;
    for &(rel, code) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        fs::write(&path, code).map_err(|e| format!("writing {}: {e}", path.display()))?;
        if root.is_none() && rel.ends_with(".snt") {
            root = Some(path);
        }
    }
    let root = root.ok_or("a project needs a `.snt` file")?;
    compile_project(&root)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[test]
fn book_samples_compile() {
    let src = book_src();
    let mut all = Vec::new();
    pages(&src, &mut all);
    all.sort();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("book");

    let mut failures = Vec::new();
    let mut compiled = 0;
    for page in &all {
        let name = page
            .strip_prefix(&src)
            .unwrap_or(page)
            .display()
            .to_string();
        let text = fs::read_to_string(page).expect("read a book page");
        let blocks = blocks(&name, &text);
        let page_dir = scratch.join(name.replace(['/', '.'], "_"));
        let mut projects: BTreeMap<&str, Vec<(usize, &Block)>> = BTreeMap::new();
        for (i, b) in blocks.iter().enumerate() {
            if let Some(p) = &b.project {
                projects.entry(p).or_default().push((i, b));
            } else if b.lang == "sinter" && !b.ignore {
                compiled += 1;
                let dir = page_dir.join(format!("line{}", i));
                let result = compile_files(&dir, &[("main.snt", &b.code)]);
                check(b, blocks.get(i + 1), result, &mut failures);
            }
        }
        for (project, files) in projects {
            let head = files[0].1;
            if head.ignore {
                continue;
            }
            compiled += 1;
            let mut sources = Vec::new();
            for &(_, b) in &files {
                match &b.file {
                    Some(rel) => sources.push((rel.as_str(), b.code.as_str())),
                    None => failures.push(format!("{}: a project block needs `file=PATH`", b.at)),
                }
            }
            let result = compile_files(&page_dir.join(project), &sources);
            // A failing project's expected error follows its last file.
            let next = files.last().and_then(|&(i, _)| blocks.get(i + 1));
            check(head, next, result, &mut failures);
        }
    }

    assert!(
        failures.is_empty(),
        "{} book sample(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    // Guard against the test silently finding nothing (a moved book, say).
    assert!(compiled >= 20, "only {compiled} book samples were compiled");
}
