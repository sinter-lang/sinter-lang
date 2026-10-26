//! `sinter-syntax`: the grammar, AST, and parser for the Sinter HDL.
//!
//! Parsing a `.snt` source produces an [`ast::Program`]. Downstream crates
//! (`sinter-compiler`) consume the AST for semantic checks and lowering.
//!
//! See `DESIGN.md` at the repository root for the language design and
//! `grammar/sinter.pest` for the normative surface grammar.
//!
//! This crate is internal to the `sinter` command (package `sinter-lang`): its
//! API may change in any release. See `STABILITY.md` in the repository.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ast;
pub mod loader;
pub mod parser;
mod self_ty;

pub use ast::{
    Attr, AttrArg, AttrValue, BinOp, Block, CastMode, CdcKind, ConstItem, Dir, EnumItem,
    EnumVariant, Expr, FieldInit, FnColor, Func, ImplItem, Item, MatchArm, ModItem, Param,
    PathQualifier, Pattern, Pos, Program, Stmt, StructField, StructItem, Type, TypeRef, UnaryOp,
    UseItem, UsePath, Visibility,
};
pub use loader::{load_project, load_project_sources, LoadError, ModuleTree, SourceFile};
pub use parser::{parse_program, parse_program_in, ParseError};
