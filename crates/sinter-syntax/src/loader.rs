//! Multi-file project loader (DESIGN §10, milestone S1).
//!
//! Given a root `.snt` file, parse it and recursively resolve its external
//! `mod foo;` declarations into a [`ModuleTree`], following the Rust-2018 file
//! layout: a module `foo` declared where submodule files live in directory `D`
//! is read from `D/foo.snt`, and `foo`'s own submodules live in `D/foo/`. Inline
//! `mod foo { … }` needs no file. No name resolution or visibility checking is
//! done here (milestone S2) — this produces the structural tree only.

use std::path::Path;

use crate::ast::{Item, ModItem, Module, Pos, Program, Visibility};
use crate::parser::{parse_program_in, ParseError};

/// An error from loading a multi-file project.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// A source file could not be read.
    #[error("cannot read `{path}`: {source}")]
    Io {
        /// The offending file path.
        path: String,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// A source file failed to parse.
    #[error("{path}: {source}")]
    Parse {
        /// The offending file path.
        path: String,
        /// The underlying parse error.
        source: ParseError,
    },
    /// An external `mod foo;` had no corresponding file.
    #[error("module `{name}` (declared in {decl}) has no file `{candidate}`")]
    MissingModule {
        /// The module name.
        name: String,
        /// The file that declared it.
        decl: String,
        /// The path that was looked for.
        candidate: String,
    },
    /// Two submodules with the same name were declared in one parent.
    #[error("module `{name}` declared more than once in {decl}")]
    DuplicateModule {
        /// The duplicated module name.
        name: String,
        /// The file declaring the duplicate.
        decl: String,
    },
}

/// A loaded module: its items, submodules, and (root only) any legacy modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleTree {
    /// The module name (the root takes its file stem).
    pub name: String,
    /// Visibility of this module (the root is `Public`).
    pub vis: Visibility,
    /// Non-`mod` functional items in this module (including `use` imports).
    pub items: Vec<Item>,
    /// Legacy `module` declarations in this file. The functional module tree does
    /// not nest these; they appear only where written (typically the root).
    pub modules: Vec<Module>,
    /// Resolved submodules (inline `mod` bodies and loaded `mod foo;` files).
    pub children: Vec<ModuleTree>,
    /// The source file this module came from (inline modules share the parent's).
    pub source: String,
    /// Source position of the `mod` declaration (the root uses the default).
    pub pos: Pos,
}

/// One source file of a loaded project, for rendering diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// The file's path, as displayed in diagnostics.
    pub path: String,
    /// The file's full text.
    pub text: String,
}

/// Load a multi-file project rooted at `root`, resolving external `mod`s.
///
/// # Errors
/// Returns a [`LoadError`] on a read / parse failure, a missing `mod foo;` file,
/// or a duplicate submodule name.
pub fn load_project(root: &Path) -> Result<ModuleTree, LoadError> {
    load_project_sources(root).map(|(tree, _)| tree)
}

/// Like [`load_project`], but also returns every source file read. The file at
/// index `i` is the one whose AST positions carry [`Pos::file`] `== i`; the
/// root is index 0.
///
/// # Errors
/// As [`load_project`].
pub fn load_project_sources(root: &Path) -> Result<(ModuleTree, Vec<SourceFile>), LoadError> {
    let mut files = Vec::new();
    let tree = load_root(root, &mut files)?;
    Ok((tree, files))
}

fn load_root(root: &Path, files: &mut Vec<SourceFile>) -> Result<ModuleTree, LoadError> {
    let text = std::fs::read_to_string(root).map_err(|e| LoadError::Io {
        path: root.display().to_string(),
        source: e,
    })?;
    let program = parse_source(&root.display().to_string(), text, files)?;
    let name = root
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("root")
        .to_string();
    let child_dir = root.parent().unwrap_or_else(|| Path::new(""));
    build_tree(
        name,
        Visibility::Public,
        program,
        &root.display().to_string(),
        child_dir,
        Pos::default(),
        files,
    )
}

/// Parse `text` as the next file of the project, recording it in `files` so
/// its positions' [`Pos::file`] index resolves back to `path`.
fn parse_source(
    path: &str,
    text: String,
    files: &mut Vec<SourceFile>,
) -> Result<Program, LoadError> {
    let index = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let program = parse_program_in(&text, index).map_err(|e| LoadError::Parse {
        path: path.to_string(),
        source: e,
    })?;
    files.push(SourceFile {
        path: path.to_string(),
        text,
    });
    Ok(program)
}

/// Turn a parsed `program` into a [`ModuleTree`], resolving its `mod` items.
/// `child_dir` is the directory holding this module's external submodule files.
fn build_tree(
    name: String,
    vis: Visibility,
    program: Program,
    source: &str,
    child_dir: &Path,
    pos: Pos,
    files: &mut Vec<SourceFile>,
) -> Result<ModuleTree, LoadError> {
    let Program { modules, items } = program;
    let mut tree_items = Vec::new();
    let mut children: Vec<ModuleTree> = Vec::new();
    for item in items {
        match item {
            Item::Mod(m) => {
                if children.iter().any(|c| c.name == m.name) {
                    return Err(LoadError::DuplicateModule {
                        name: m.name,
                        decl: source.to_string(),
                    });
                }
                children.push(load_mod(m, source, child_dir, files)?);
            }
            other => tree_items.push(other),
        }
    }
    Ok(ModuleTree {
        name,
        vis,
        items: tree_items,
        modules,
        children,
        source: source.to_string(),
        pos,
    })
}

/// Resolve one `mod` declaration into a [`ModuleTree`]. `parent_child_dir` is the
/// directory holding the parent's submodule files.
fn load_mod(
    m: ModItem,
    decl_source: &str,
    parent_child_dir: &Path,
    files: &mut Vec<SourceFile>,
) -> Result<ModuleTree, LoadError> {
    let ModItem {
        vis,
        name,
        body,
        pos,
    } = m;
    // This module's own external submodules live in `<parent_child_dir>/<name>/`.
    let child_dir = parent_child_dir.join(&name);
    match body {
        Some(items) => {
            // Inline module: no file of its own.
            let program = Program {
                modules: Vec::new(),
                items,
            };
            build_tree(name, vis, program, decl_source, &child_dir, pos, files)
        }
        None => {
            // External module: read `<parent_child_dir>/<name>.snt`.
            let file = parent_child_dir.join(format!("{name}.snt"));
            let program = match std::fs::read_to_string(&file) {
                Ok(text) => parse_source(&file.display().to_string(), text, files)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(LoadError::MissingModule {
                        name,
                        decl: decl_source.to_string(),
                        candidate: file.display().to_string(),
                    });
                }
                Err(e) => {
                    return Err(LoadError::Io {
                        path: file.display().to_string(),
                        source: e,
                    });
                }
            };
            build_tree(
                name,
                vis,
                program,
                &file.display().to_string(),
                &child_dir,
                pos,
                files,
            )
        }
    }
}
