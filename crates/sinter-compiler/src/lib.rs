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
pub mod channel_desugar;
pub mod check;
pub mod elaborate;
pub mod error;
pub mod eval;
pub mod fault_latch;
pub mod functional;
pub mod include_expand;
pub mod ir;
pub mod ir_cse;
pub mod multiplex_desugar;
pub mod pinmap;
pub mod pipeline_desugar;
pub mod resolve;
pub mod sim;
pub mod stimulus;
pub mod vcd;
pub mod verilator;
pub mod verilog;

pub use error::CompileError;
pub use fault_latch::FaultMode;
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
    compile_str_faults(source, FaultMode::Off)
}

/// Like [`compile_str`], but with an explicit [`FaultMode`] (DESIGN §6.12,
/// FLT1b): `On` emits the silicon fault latch (a sticky bitmap + a `fault_code`
/// sideband); `Off` is functional-neutral (byte-identical to the assert-free
/// module).
///
/// # Errors
/// Returns a [`CompileError`] on a parse or semantic error.
pub fn compile_str_faults(source: &str, faults: FaultMode) -> Result<Design, CompileError> {
    let program = sinter_syntax::parse_program(source)?;
    // No source directory, so `include_hex!` has nowhere to resolve against.
    compile_program(program, None, faults)
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
    compile_project_faults(root, FaultMode::Off)
}

/// Like [`compile_project`], but with an explicit [`FaultMode`] (DESIGN §6.12,
/// FLT1b).
///
/// # Errors
/// Returns a [`CompileError`] on a load, resolution, or semantic error.
pub fn compile_project_faults(
    root: &std::path::Path,
    faults: FaultMode,
) -> Result<Design, CompileError> {
    let (tree, files) = sinter_syntax::load_project_sources(root)?;
    let located = |e: CompileError| e.with_sources(&files);
    let program = resolve::resolve_project(&tree).map_err(located)?;
    // `include_hex!` paths resolve against the root source file's directory.
    compile_program(program, root.parent(), faults).map_err(located)
}

/// Check and lower an already-resolved [`Design`] from a flat program. `base` is
/// the source directory `include_hex!` paths resolve against (`None` for a bare
/// string).
///
/// `include_hex!` is expanded first (DESIGN §9.7), then staged function colors
/// are desugared to a synthesized instance struct (with an `advance` method) so
/// the rest of the pipeline — type checking and elaboration — handles them as
/// ordinary functional items: `pipeline fn`s (DESIGN §3.1) via
/// [`pipeline_desugar`], `multiplex fn`s (§3.2) via [`multiplex_desugar`]. A
/// `multiplex` design additionally runs the IR-level multiply-CSE pass
/// ([`ir_cse`]) so its shared multiply becomes one physical multiplier. A program
/// with no staged color is passed through untouched (byte-identical legacy path).
fn compile_program(
    program: sinter_syntax::ast::Program,
    base: Option<&std::path::Path>,
    faults: FaultMode,
) -> Result<Design, CompileError> {
    use sinter_syntax::ast::{FnColor, Item};
    let mut program = program;
    include_expand::expand_includes(&mut program, base)?;
    // Lower channel fields (`fifo`/`ring`/`flow`) to synthesized struct + impl
    // (DESIGN §9.8) before the staged-color desugars, so they see only ordinary
    // state.
    let program = if channel_desugar::has_channel(&program) {
        channel_desugar::desugar_channels(&program)?
    } else {
        program
    };
    let has_pipeline = program
        .items
        .iter()
        .any(|it| matches!(it, Item::Func(f) if f.color == FnColor::Pipeline));
    let has_multiplex = program
        .items
        .iter()
        .any(|it| matches!(it, Item::Func(f) if f.color == FnColor::Multiplex));
    let program = if has_pipeline {
        pipeline_desugar::desugar_pipelines(&program)?
    } else {
        program
    };
    let program = if has_multiplex {
        multiplex_desugar::desugar_multiplexes(&program)?
    } else {
        program
    };
    functional::check_functional(&program)?;
    let mut design = check::check_program(&program)?;
    if let Some(mut module) = elaborate::elaborate(&program)? {
        if has_multiplex {
            // Collapse the desugar's shared (then SROA-duplicated) multiply into
            // one physical multiplier (DESIGN §3.2).
            ir_cse::share_muls(&mut module);
        }
        // FLT1b: when faults are on, synthesize the silicon latch (sticky bitmap +
        // `fault_code` sideband) from the assert sites. Read once here, so both
        // backends fold from one IR; `off` leaves the module functional-neutral.
        if faults == FaultMode::On {
            fault_latch::apply(&mut module)?;
        }
        design.modules.push(module);
    }
    Ok(design)
}
