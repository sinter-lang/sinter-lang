//! The `sinter` command-line interface.
//!
//! Subcommands:
//! - `check` — parse and semantically check a `.snt` source.
//! - `build` — emit SystemVerilog and a Verilator C++ harness per module.
//! - `sim`   — run the cycle-accurate reference model and write a binary trace.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use log::info;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference, Simulator};
use sinter_compiler::{compile_str, verilator, verilog};

/// The Sinter HDL compiler.
#[derive(Parser)]
#[command(name = "sinter", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and check a source file, reporting any errors.
    Check {
        /// Path to a `.snt` source file.
        file: PathBuf,
    },
    /// Emit backend artifacts (SystemVerilog and/or a Verilator harness).
    Build {
        /// Path to a `.snt` source file.
        file: PathBuf,
        /// Which artifacts to emit.
        #[arg(long, value_enum, default_value_t = Emit::All)]
        emit: Emit,
        /// Output directory.
        #[arg(long, default_value = "build")]
        out: PathBuf,
    },
    /// Run the cycle-accurate reference simulator over a generated stimulus.
    Sim {
        /// Path to a `.snt` source file.
        file: PathBuf,
        /// Number of cycles to simulate.
        #[arg(long, default_value_t = 16)]
        cycles: usize,
        /// Deterministic stimulus seed.
        #[arg(long, default_value_t = 0x5151_5151)]
        seed: u64,
        /// Optional path to write the binary output trace.
        #[arg(long)]
        trace: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, ValueEnum)]
enum Emit {
    /// SystemVerilog only.
    Verilog,
    /// Verilator C++ harness only.
    Harness,
    /// Both.
    All,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Check { file } => cmd_check(&file),
        Command::Build { file, emit, out } => cmd_build(&file, emit, &out),
        Command::Sim {
            file,
            cycles,
            seed,
            trace,
        } => cmd_sim(&file, cycles, seed, trace.as_deref()),
    }
}

fn read_and_compile(file: &Path) -> Result<Vec<Module>> {
    let source =
        fs::read_to_string(file).with_context(|| format!("reading source `{}`", file.display()))?;
    let design = compile_str(&source).with_context(|| format!("compiling `{}`", file.display()))?;
    Ok(design.modules)
}

fn cmd_check(file: &Path) -> Result<()> {
    let modules = read_and_compile(file)?;
    for m in &modules {
        println!(
            "ok: module `{}` ({} ports, {} regs, {} wires)",
            m.name,
            m.ports.len(),
            m.regs.len(),
            m.lets.len()
        );
    }
    Ok(())
}

fn cmd_build(file: &Path, emit: Emit, out: &Path) -> Result<()> {
    let modules = read_and_compile(file)?;
    fs::create_dir_all(out).with_context(|| format!("creating output dir `{}`", out.display()))?;

    for m in &modules {
        if matches!(emit, Emit::Verilog | Emit::All) {
            let path = out.join(format!("{}.sv", m.name));
            fs::write(&path, verilog::emit_module(m))
                .with_context(|| format!("writing `{}`", path.display()))?;
            info!("wrote {}", path.display());
        }
        if matches!(emit, Emit::Harness | Emit::All) {
            let path = out.join(format!("{}_tb.cpp", m.name));
            fs::write(&path, verilator::emit_cpp_harness(m))
                .with_context(|| format!("writing `{}`", path.display()))?;
            info!("wrote {}", path.display());
        }
    }
    Ok(())
}

fn cmd_sim(file: &Path, cycles: usize, seed: u64, trace: Option<&Path>) -> Result<()> {
    let modules = read_and_compile(file)?;
    let module = modules
        .first()
        .context("source contains no modules to simulate")?;

    let stimulus = gen_stimulus(module, cycles, seed);

    if let Some(path) = trace {
        let bytes = run_reference(module, &stimulus);
        fs::write(path, &bytes).with_context(|| format!("writing trace `{}`", path.display()))?;
        info!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    // Print a human-readable per-cycle table to stdout.
    let inputs: Vec<String> = module.inputs().map(|p| p.name.clone()).collect();
    let outputs: Vec<String> = module.outputs().map(|p| p.name.clone()).collect();
    println!("cycle | {} || {}", inputs.join(" "), outputs.join(" "));

    let mut sim = Simulator::new(module);
    for (k, row) in stimulus.iter().enumerate() {
        let in_map: HashMap<String, u64> =
            inputs.iter().cloned().zip(row.iter().copied()).collect();
        let out = sim.step(&in_map);
        let in_str = row
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let out_str = outputs
            .iter()
            .map(|n| out.get(n).copied().unwrap_or(0).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        println!("{k:5} | {in_str} || {out_str}");
    }
    Ok(())
}
