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
pub mod parser;

pub use ast::{
    Assign, AssignKind, BinOp, Dir, Expr, LetBinding, Module, Port, Pos, Program, Reg, Type,
    UnaryOp,
};
pub use parser::{parse_program, ParseError};
