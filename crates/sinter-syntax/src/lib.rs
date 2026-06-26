//! `sinter-syntax`: the grammar, AST, and parser for the Sinter HDL.
//!
//! Parsing a `.snt` source produces an [`ast::Program`]. Downstream crates
//! (`sinter-compiler`) consume the AST for semantic checks and lowering.
//!
//! See `DESIGN.md` at the repository root for the language design and
//! `grammar/sinter.pest` for the normative surface grammar.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ast;
pub mod loader;
pub mod parser;

pub use ast::{
    Assign, AssignKind, Attr, AttrArg, AttrValue, BinOp, Block, CastMode, ConstItem, Dir, EnumItem,
    EnumVariant, Expr, FieldInit, Func, ImplItem, Item, LetBinding, MatchArm, ModItem, Module,
    Param, PathQualifier, Pattern, Port, Pos, Program, Reg, Stmt, StructField, StructItem, Type,
    TypeRef, UnaryOp, UseItem, UsePath, Visibility,
};
pub use loader::{load_project, LoadError, ModuleTree};
pub use parser::{parse_program, ParseError};
