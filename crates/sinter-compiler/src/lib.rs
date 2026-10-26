//! `sinter-compiler`: semantic checks and backends for the Sinter HDL.
//!
//! Pipeline: `sinter-syntax` parses `.snt` into an AST; [`functional`] type-checks
//! it and [`elaborate`] lowers it to a typed [`ir::Design`]; the backends lower the IR to a Rust simulator
//! ([`sim`]), portable SystemVerilog ([`verilog`]), and a Verilator testbench
//! harness ([`verilator`]). The shared [`eval`] semantics keep the simulator
//! and the Verilog in bit-for-bit agreement (DESIGN §7.4).
//!
//! This crate is internal to the `sinter` command (package `sinter-lang`): its
//! API may change in any release. See `STABILITY.md` in the repository.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod board;
pub mod cdc_desugar;
pub mod channel_desugar;
pub mod elaborate;
pub mod error;
pub mod eval;
pub mod fault_latch;
pub mod foreign;
pub mod functional;
pub mod include_expand;
pub mod ir;
pub mod ir_cse;
pub mod multiplex_desugar;
pub mod pinmap;
pub mod pipeline_desugar;
pub mod resolve;
pub mod rust_sim;
pub mod sim;
mod sim_runtime;
pub mod stimulus;
pub mod surface;
pub mod vcd;
pub mod verilator;
pub mod verilog;

pub use error::CompileError;
pub use fault_latch::FaultMode;
pub use ir::{Design, Module};

/// Parse and check Sinter source, returning the typed design.
///
/// The source is type-checked by [`functional::check_functional`], then
/// [`elaborate`] lowers its `#[clock]` transition root to an [`ir::Module`]
/// (DESIGN §2.1). A source with no transition root yields no module.
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
/// with no staged color is passed through untouched.
fn compile_program(
    program: sinter_syntax::ast::Program,
    base: Option<&std::path::Path>,
    faults: FaultMode,
) -> Result<Design, CompileError> {
    surface::check_attributes(&program)?;
    surface::check_pub_internal_state(&program)?;
    let mut program = program;
    include_expand::expand_includes(&mut program, base)?;
    // Lower channel fields (`fifo`/`ring`/`flow`) to synthesized struct + impl
    // (DESIGN §9.8) before the staged-color desugars, so they see only ordinary
    // state.
    // Errors after this point name the synthesized channel structs; show them
    // as the user wrote them (`Push_u8` → `Push<u8>`).
    // Clock-domain crossings first: `mailbox` / `async_fifo` lower to channel
    // views that the channel desugar then lowers in turn.
    let mut names = Vec::new();
    let program = if cdc_desugar::has_cdc(&program) {
        names.extend(cdc_desugar::source_names(&program));
        cdc_desugar::desugar_cdc(&program)?
    } else {
        program
    };
    let (program, channel_names) = if channel_desugar::has_channel(&program) {
        names.extend(channel_desugar::source_names(&program));
        (channel_desugar::desugar_channels(&program)?, names)
    } else {
        (program, names)
    };
    compile_lowered(program, base, faults).map_err(|e| e.rename_types(&channel_names))
}

/// The rest of [`compile_program`], after the channel desugar.
fn compile_lowered(
    program: sinter_syntax::ast::Program,
    base: Option<&std::path::Path>,
    faults: FaultMode,
) -> Result<Design, CompileError> {
    use sinter_syntax::ast::{FnColor, Item};
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
    let mut design = Design {
        modules: Vec::new(),
    };
    if let Some(mut module) = elaborate::elaborate(&program)? {
        // `#[verilog(src = "...")]` paths resolve against the source directory,
        // like `include_hex!` (DESIGN §6.15).
        for inst in &mut module.instances {
            if let (Some(src), Some(base)) = (&inst.src, base) {
                let path = base.join(src);
                if !path.is_file() {
                    return Err(CompileError::semantic(
                        &inst.model,
                        sinter_syntax::ast::Pos::default(),
                        format!(
                            "`#[verilog]` source `{}` not found (resolved to `{}`)",
                            src.display(),
                            path.display()
                        ),
                    ));
                }
                inst.src = Some(path);
            }
        }
        if has_multiplex {
            // Collapse the desugar's shared (then SROA-duplicated) multiply into
            // one physical multiplier (DESIGN §3.2).
            ir_cse::share_muls(&mut module);
        }
        // FLT1b: when faults are on, synthesize the silicon latch (sticky bitmap +
        // `fault_code` sideband) from the assert sites. Read once here, so both
        // backends fold from one IR; `off` leaves the module functional-neutral.
        if faults == FaultMode::On {
            if module.is_multi_clock() {
                // The latch is one register over every assert site, which would
                // itself cross clock domains.
                return Err(CompileError::semantic(
                    &module.name,
                    sinter_syntax::ast::Pos::default(),
                    "`--faults` doesn't support several clock domains yet",
                ));
            }
            fault_latch::apply(&mut module)?;
        }
        design.modules.push(module);
    }
    Ok(design)
}
