//! `sinter-compiler`: semantic checks and backends for the Sinter HDL.
//!
//! Pipeline: `sinter-syntax` parses `.snt` into an AST; [`check`] turns that
//! into a typed [`ir::Design`]; the backends lower the IR to a Rust simulator
//! ([`sim`]), portable SystemVerilog ([`verilog`]), and a Verilator testbench
//! harness ([`verilator`]). The shared [`eval`] semantics keep the simulator
//! and the Verilog in bit-for-bit agreement (DESIGN §7.4).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod board;
pub mod check;
pub mod elaborate;
pub mod error;
pub mod eval;
pub mod functional;
pub mod ir;
pub mod pinmap;
pub mod resolve;
pub mod sim;
pub mod verilator;
pub mod verilog;

pub use error::CompileError;
pub use ir::{Design, Module};

/// Parse and check Sinter source, returning the typed design.
///
/// Both surfaces are lowered to the same IR: the legacy `module` surface via
/// [`check`], and the functional surface (DESIGN §2.1) via
/// [`functional::check_functional`] (type checking) then [`elaborate`]
/// (lowering its `#[clock]` transition root to an [`ir::Module`]). A functional
/// source with no transition root yields no module.
///
/// # Errors
/// Returns a [`CompileError`] on a parse or semantic error.
pub fn compile_str(source: &str) -> Result<Design, CompileError> {
    let program = sinter_syntax::parse_program(source)?;
    compile_program(program)
}

/// Parse, resolve, and check a multi-file project rooted at `root` (DESIGN §10).
///
/// Loads the module tree ([`sinter_syntax::load_project`]), resolves cross-module
/// names + visibility into a single flat program ([`resolve`]), then runs the
/// same checks and lowering as [`compile_str`]. A single-file project (no `mod` /
/// `use`) yields the same result as calling [`compile_str`] on that file.
///
/// # Errors
/// Returns a [`CompileError`] on a load, resolution, or semantic error.
pub fn compile_project(root: &std::path::Path) -> Result<Design, CompileError> {
    let tree = sinter_syntax::load_project(root)?;
    let program = resolve::resolve_project(&tree)?;
    compile_program(program)
}

/// Check and lower an already-resolved [`Design`] from a flat program.
fn compile_program(program: sinter_syntax::ast::Program) -> Result<Design, CompileError> {
    functional::check_functional(&program)?;
    let mut design = check::check_program(&program)?;
    if let Some(module) = elaborate::elaborate(&program)? {
        design.modules.push(module);
    }
    Ok(design)
}
