//! The `sinter` command-line interface.
//!
//! Subcommands:
//! - `check` — parse and semantically check a `.snt` source.
//! - `build` — emit SystemVerilog, a Verilator C++ harness, and a compiled Rust
//!   simulator crate per module.
//! - `sim`   — run the cycle-accurate reference model over random or directed
//!   (CSV) stimulus, checking any expected outputs.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use log::{error, info, warn};

use sinter_compiler::ir::Module;
use sinter_compiler::pinmap::PinMap;
use sinter_compiler::sim::{gen_stimulus, run_reference_with_resets, Simulator};
use sinter_compiler::stimulus::{format_value, Stimulus};
use sinter_compiler::vcd::VcdWriter;
use sinter_compiler::{
    board, compile_project_with, cosim, fault_latch, rust_sim, verilator, verilog, Define,
    FaultMode,
};

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
        #[command(flatten)]
        consts: ConstArgs,
    },
    /// Emit backend artifacts (SystemVerilog, a Verilator harness, a Rust
    /// simulator crate).
    Build {
        /// Path to a `.snt` source file.
        file: PathBuf,
        #[command(flatten)]
        consts: ConstArgs,
        /// Which artifacts to emit.
        #[arg(long, value_enum, default_value_t = Emit::All)]
        emit: Emit,
        /// Output directory.
        #[arg(long, default_value = "build")]
        out: PathBuf,
        /// Optional pin-map file (`.pins`): also emit a board-top Verilog
        /// wrapper and the matching nextpnr PCF (DESIGN §7.6).
        #[arg(long)]
        pins: Option<PathBuf>,
        /// Where to write the pin constraints, as an iCE40 PCF (`.pcf`) or an
        /// ECP5 LPF (`.lpf`), chosen by the extension (needs `--pins`;
        /// default `<out>/<wrapper>.pcf`).
        #[arg(long, requires = "pins")]
        constraints: Option<PathBuf>,
        /// Emit the silicon fault latch (DESIGN §6.12, FLT1b): a sticky bitmap +
        /// a priority-encoded `fault_code` sideband, plus a `<module>.faults.json`
        /// `code -> backtrace` table. Off by default (functional-neutral).
        #[arg(long)]
        faults: bool,
    },
    /// Run the cycle-accurate reference simulator over random or directed
    /// stimulus.
    Sim {
        /// Path to a `.snt` source file.
        file: PathBuf,
        #[command(flatten)]
        consts: ConstArgs,
        /// Number of cycles of random stimulus to simulate.
        #[arg(long, default_value_t = 16)]
        cycles: usize,
        /// Deterministic random-stimulus seed.
        #[arg(long, default_value_t = 0x5151_5151)]
        seed: u64,
        /// Directed stimulus: a CSV file with one row per cycle, a column per
        /// input port, and optional `expect.<output>` columns that are checked
        /// (a mismatch fails the run). Replaces the random stimulus.
        #[arg(long, conflicts_with_all = ["cycles", "seed"])]
        stimulus: Option<PathBuf>,
        /// Optional path to write the binary output trace.
        #[arg(long)]
        trace: Option<PathBuf>,
        /// Optional path to write a VCD waveform (inputs, outputs, registers,
        /// and wires; viewable in GTKWave / Surfer).
        #[arg(long)]
        vcd: Option<PathBuf>,
        /// Also build the emitted SystemVerilog with Verilator, run it over
        /// the same stimulus, and fail on the first output that differs from
        /// the simulator (DESIGN §9.15, COSIM1). Needs `verilator` on the PATH.
        #[arg(long)]
        cosim: bool,
        /// Where `--cosim` writes the Verilog, the harness, and the build.
        #[arg(
            long,
            value_name = "DIR",
            default_value = "build/cosim",
            requires = "cosim"
        )]
        cosim_dir: PathBuf,
    },
}

/// What `sinter sim` writes besides its table.
struct SimOutputs<'a> {
    trace: Option<&'a Path>,
    vcd: Option<&'a Path>,
    /// The `--cosim` directory, when co-simulating.
    cosim: Option<&'a Path>,
}

/// Build-time constant overrides (DESIGN §9.15, DEF1).
#[derive(Args)]
struct ConstArgs {
    /// Override an integer `pub const`, named by its path from the root
    /// module (`N_MUL`, `setup::N_MUL`). Repeatable; applied after `--defines`.
    #[arg(short = 'D', value_name = "NAME=VALUE")]
    define: Vec<String>,
    /// A file of overrides, one `NAME = VALUE` per line (`#` comments).
    #[arg(long, value_name = "FILE")]
    defines: Option<PathBuf>,
}

impl ConstArgs {
    /// The overrides: the file's first, then each `-D`, so a `-D` wins.
    fn parse(&self) -> Result<Vec<Define>> {
        let mut out = Vec::new();
        if let Some(path) = &self.defines {
            let text = fs::read_to_string(path)
                .with_context(|| format!("reading defines `{}`", path.display()))?;
            out.extend(Define::parse_file(&text, &path.display().to_string())?);
        }
        for spec in &self.define {
            out.push(Define::parse(spec)?);
        }
        Ok(out)
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum Emit {
    /// SystemVerilog only.
    Verilog,
    /// Verilator C++ harness only.
    Harness,
    /// A standalone Rust simulator crate, `<out>/<module>-sim/` (DESIGN §7.2).
    Rust,
    /// All of the above.
    All,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Check { file, consts } => cmd_check(&file, &consts.parse()?),
        Command::Build {
            file,
            consts,
            emit,
            out,
            pins,
            constraints,
            faults,
        } => cmd_build(
            &file,
            &consts.parse()?,
            emit,
            &out,
            pins.as_deref(),
            constraints.as_deref(),
            faults,
        ),
        Command::Sim {
            file,
            consts,
            cycles,
            seed,
            stimulus,
            trace,
            vcd,
            cosim,
            cosim_dir,
        } => cmd_sim(
            &file,
            &consts.parse()?,
            cycles,
            seed,
            stimulus.as_deref(),
            &SimOutputs {
                trace: trace.as_deref(),
                vcd: vcd.as_deref(),
                cosim: cosim.then_some(cosim_dir.as_path()),
            },
        ),
    }
}

/// Compile the `.snt` at `file` as the root of a (possibly multi-file) project:
/// `mod` declarations pull in sibling files, names resolve across modules, and
/// the result lowers to the same flat IR as the equivalent single file (DESIGN
/// §10). A file with no `mod`s compiles exactly as before.
fn read_and_compile(file: &Path, faults: FaultMode, defines: &[Define]) -> Result<Vec<Module>> {
    let design = compile_project_with(file, faults, defines)
        .with_context(|| format!("compiling `{}`", file.display()))?;
    Ok(design.modules)
}

/// The command's results on standard output. Once a reader closes the pipe
/// (`sinter sim ... | head`), the rest is dropped quietly: the run still
/// finishes, so failures are still logged and still set the exit status.
#[derive(Default)]
struct Stdout {
    closed: bool,
}

impl Stdout {
    fn line(&mut self, args: std::fmt::Arguments) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        match writeln!(std::io::stdout().lock(), "{args}") {
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                self.closed = true;
                Ok(())
            }
            r => r.context("writing to standard output"),
        }
    }
}

fn cmd_check(file: &Path, defines: &[Define]) -> Result<()> {
    let modules = read_and_compile(file, FaultMode::Off, defines)?;
    let mut stdout = Stdout::default();
    for m in &modules {
        stdout.line(format_args!(
            "ok: module `{}` ({} ports, {} regs, {} wires)",
            m.name,
            m.ports.len(),
            m.regs.len(),
            m.lets.len()
        ))?;
    }
    if modules.is_empty() {
        // The source type-checked (it reached here without error) but has no
        // `#[clock]` root, so there is no module to lower (DESIGN §2.1).
        info!(
            "`{}` type-checks, but has no `#[clock]` root, so there is no \
             module to build or simulate",
            file.display()
        );
    }
    Ok(())
}

fn cmd_build(
    file: &Path,
    defines: &[Define],
    emit: Emit,
    out: &Path,
    pins: Option<&Path>,
    constraints: Option<&Path>,
    faults: bool,
) -> Result<()> {
    let mode = if faults {
        FaultMode::On
    } else {
        FaultMode::Off
    };
    let modules = read_and_compile(file, mode, defines)?;
    fs::create_dir_all(out).with_context(|| format!("creating output dir `{}`", out.display()))?;

    // The first module is the top; the rest are its `#[module]` children.
    // Each gets its Verilog and a Rust simulator (a child's, for testing that
    // block alone); the Verilator harness is for the top.
    for (k, m) in modules.iter().enumerate() {
        let top = k == 0;
        if m.outputs().next().is_none() {
            warn!(
                "module `{}` has no output ports, so synthesis will remove all of its \
                 logic; mark the state fields to export as `pub`",
                m.name
            );
        }
        if matches!(emit, Emit::Verilog | Emit::All) {
            let path = out.join(format!("{}.sv", m.name));
            fs::write(&path, verilog::emit_module(m))
                .with_context(|| format!("writing `{}`", path.display()))?;
            info!("wrote {}", path.display());
        }
        if top && matches!(emit, Emit::Harness | Emit::All) {
            let path = out.join(format!("{}_tb.cpp", m.name));
            fs::write(&path, verilator::emit_cpp_harness(m))
                .with_context(|| format!("writing `{}`", path.display()))?;
            info!("wrote {}", path.display());
        }
        if matches!(emit, Emit::Rust | Emit::All) {
            let name = rust_sim::crate_name(m);
            let dir = out.join(&name);
            for (rel, contents) in rust_sim::emit_crate(m, &name) {
                let path = dir.join(rel);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("creating `{}`", parent.display()))?;
                }
                fs::write(&path, contents)
                    .with_context(|| format!("writing `{}`", path.display()))?;
            }
            info!("wrote {}", dir.display());
        }
        // FLT1b: with `--faults`, emit the offline `code -> backtrace` fault table
        // alongside the SV so a latched `fault_code` can be decoded (DESIGN §6.12).
        if faults && !m.asserts.is_empty() {
            let path = out.join(format!("{}.faults.json", m.name));
            fs::write(&path, fault_latch::fault_table_json(m))
                .with_context(|| format!("writing `{}`", path.display()))?;
            info!("wrote {}", path.display());
        }
    }

    copy_verilog_sources(&modules, out)?;
    if let Some(pins) = pins {
        emit_board(&modules, pins, constraints, out)?;
    }
    Ok(())
}

/// Copy each `#[verilog(src = "...")]` source beside the emitted SystemVerilog,
/// so the output directory is a complete file list for synthesis and simulation
/// (DESIGN §6.15). Warn about instances with no `src`.
fn copy_verilog_sources(modules: &[Module], out: &Path) -> Result<()> {
    let mut copied: HashMap<String, PathBuf> = HashMap::new();
    // A `#[module]` instance's module is one of `modules`, already written.
    for inst in modules
        .iter()
        .flat_map(|m| &m.instances)
        .filter(|i| !i.generated)
    {
        let Some(src) = &inst.src else {
            warn!(
                "`#[verilog]` instance `{}` of `{}` has no `src`; add the module's Verilog \
                 to your synthesis / simulation file list yourself",
                inst.name, inst.module
            );
            continue;
        };
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("`#[verilog]` source `{}` has no file name", src.display()))?
            .to_string();
        match copied.get(&name) {
            Some(prev) if prev == src => continue,
            Some(prev) => bail!(
                "two `#[verilog]` sources share the file name `{name}`: `{}` and `{}`",
                prev.display(),
                src.display()
            ),
            None => {}
        }
        let dest = out.join(&name);
        fs::copy(src, &dest)
            .with_context(|| format!("copying `{}` to `{}`", src.display(), dest.display()))?;
        info!("wrote {}", dest.display());
        copied.insert(name, src.clone());
    }
    Ok(())
}

/// Emit the board-top wrapper + its pin constraints (a PCF, or an LPF when
/// `constraints` names a `.lpf`) around the design's top module.
fn emit_board(
    modules: &[Module],
    pins: &Path,
    constraints: Option<&Path>,
    out: &Path,
) -> Result<()> {
    let Some(module) = modules.first() else {
        anyhow::bail!(
            "`--pins` needs a design with a `#[clock]` root, but `{}` has none",
            pins.display()
        );
    };
    let text = fs::read_to_string(pins)
        .with_context(|| format!("reading pin map `{}`", pins.display()))?;
    let source = pins.display().to_string();
    let pinmap = PinMap::parse(&text, &source).context("parsing pin map")?;
    let stem = pins.file_stem().and_then(|s| s.to_str()).unwrap_or("board");
    let board = board::emit_board(module, &pinmap, stem).context("generating board wrapper")?;

    let sv = out.join(format!("{}.sv", board.module_name));
    fs::write(&sv, &board.verilog).with_context(|| format!("writing `{}`", sv.display()))?;
    info!("wrote {}", sv.display());
    let path = constraints.map_or_else(
        || out.join(format!("{}.pcf", board.module_name)),
        Path::to_path_buf,
    );
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let text = match ext.to_ascii_lowercase().as_str() {
        "pcf" => {
            if board.lpf_only {
                warn!(
                    "`{}` sets `io_type=` or `freq=`, which a PCF can't carry; they are \
                     ignored (write an `.lpf` to keep them)",
                    pins.display()
                );
            }
            &board.pcf
        }
        "lpf" => &board.lpf,
        _ => anyhow::bail!(
            "`--constraints {}`: the extension picks the format, `.pcf` (iCE40) or `.lpf` (ECP5)",
            path.display()
        ),
    };
    fs::write(&path, text).with_context(|| format!("writing `{}`", path.display()))?;
    info!("wrote {}", path.display());
    Ok(())
}

/// `--cosim`: run the Verilog over `rows` (and `resets`) under Verilator in
/// `dir/<module>`, and report whether its outputs match the simulator's.
fn co_simulate(modules: &[Module], rows: &[Vec<u64>], resets: &[bool], dir: &Path) -> Result<bool> {
    let Some((module, children)) = modules.split_first() else {
        return Ok(true);
    };
    if !cosim::verilator_available() {
        bail!("`--cosim` needs `verilator` on the PATH");
    }
    let dir = dir.join(&module.name);
    info!(
        "co-simulating the Verilog with Verilator in {}",
        dir.display()
    );
    let dut = cosim::run(&dir, module, children, rows, resets).context("co-simulation")?;
    let reference = run_reference_with_resets(module, rows, resets);
    match cosim::first_mismatch(module, &reference, &dut) {
        None => {
            info!(
                "co-simulation: the Verilog matches the simulator over {} cycle(s)",
                rows.len()
            );
            Ok(true)
        }
        Some(m) => {
            let ty = module
                .outputs()
                .find(|p| p.name == m.port)
                .map_or(sinter_syntax::ast::Type::Uint(64), |p| p.ty);
            error!(
                "co-simulation mismatch (cycle {}): `{}` is {} in the simulator but {} in the \
                 Verilog",
                m.cycle,
                m.port,
                format_value(m.expected, ty),
                format_value(m.actual, ty)
            );
            Ok(false)
        }
    }
}

fn cmd_sim(
    file: &Path,
    defines: &[Define],
    cycles: usize,
    seed: u64,
    stimulus: Option<&Path>,
    writes: &SimOutputs,
) -> Result<()> {
    let (trace, vcd) = (writes.trace, writes.vcd);
    let modules = read_and_compile(file, FaultMode::Off, defines)?;
    let module = modules
        .first()
        .context("source contains no modules to simulate")?;

    // Directed (CSV) stimulus, or the deterministic random default.
    let directed = match stimulus {
        Some(path) => {
            let text = fs::read_to_string(path)
                .with_context(|| format!("reading stimulus `{}`", path.display()))?;
            let stim = Stimulus::parse_csv(module, &text)
                .with_context(|| format!("parsing stimulus `{}`", path.display()))?;
            if !stim.defaulted.is_empty() {
                info!(
                    "inputs with no stimulus column are driven 0: {}",
                    stim.defaulted.join(", ")
                );
            }
            Some(stim)
        }
        None => None,
    };
    let rows = match &directed {
        Some(stim) => stim.inputs.clone(),
        None => gen_stimulus(module, cycles, seed),
    };

    let resets: &[bool] = directed.as_ref().map_or(&[], |s| s.resets.as_slice());
    if let Some(path) = trace {
        let bytes = run_reference_with_resets(module, &rows, resets);
        fs::write(path, &bytes).with_context(|| format!("writing trace `{}`", path.display()))?;
        info!("wrote {} ({} bytes)", path.display(), bytes.len());
    }

    // Print a human-readable per-cycle table to stdout.
    let inputs: Vec<_> = module.inputs().collect();
    let outputs: Vec<_> = module.outputs().collect();
    let names = |ports: &[&sinter_compiler::ir::Port]| {
        ports
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    // With several clock domains a row is an edge time: show the time and
    // which domains ticked (DESIGN §6.7).
    let multi = module.is_multi_clock();
    let mut stdout = Stdout::default();
    if multi {
        stdout.line(format_args!(
            " step |  time | ticks | {} || {}",
            names(&inputs),
            names(&outputs)
        ))?;
    } else {
        stdout.line(format_args!(
            "cycle | {} || {}",
            names(&inputs),
            names(&outputs)
        ))?;
    }

    let mut wave = match vcd {
        Some(path) => {
            let f = fs::File::create(path)
                .with_context(|| format!("creating VCD `{}`", path.display()))?;
            Some(VcdWriter::new(BufWriter::new(f), module).context("writing VCD header")?)
        }
        None => None,
    };

    let mut sim = Simulator::new(module);
    let mut mismatches = Vec::new();
    for (k, row) in rows.iter().enumerate() {
        let in_map: HashMap<String, u64> = inputs
            .iter()
            .map(|p| p.name.clone())
            .zip(row.iter().copied())
            .collect();
        let in_reset = resets.get(k).copied().unwrap_or(false);
        let out = if in_reset {
            sim.step_in_reset(&in_map)
        } else {
            sim.step(&in_map)
        };
        // The inputs in effect: a domain's hold between its edges.
        let in_str = inputs
            .iter()
            .zip(row)
            .map(|(p, &v)| {
                format_value(
                    if multi {
                        sim.signal(&p.name).unwrap_or(v)
                    } else {
                        v
                    },
                    p.ty,
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let out_str = outputs
            .iter()
            .map(|p| format_value(out.get(&p.name).copied().unwrap_or(0), p.ty))
            .collect::<Vec<_>>()
            .join(" ");
        if multi {
            let ticks: Vec<&str> = module
                .domains
                .iter()
                .zip(sim.ticking())
                .filter(|(_, &t)| t)
                .map(|(d, _)| d.name.as_str())
                .collect();
            stdout.line(format_args!(
                "{k:5} | {:5} | {:5} | {in_str} || {out_str}",
                sim.time(),
                ticks.join(",")
            ))?;
        } else {
            let mark = if in_reset { "  (in reset)" } else { "" };
            stdout.line(format_args!("{k:5} | {in_str} || {out_str}{mark}"))?;
        }
        if let Some(stim) = &directed {
            mismatches.extend(stim.check_cycle(module, k, &out));
        }
        if let Some(w) = &mut wave {
            w.cycle(&sim, &out).context("writing VCD")?;
        }
    }
    if let (Some(w), Some(path)) = (wave, vcd) {
        w.finish().context("writing VCD")?;
        info!("wrote {}", path.display());
    }

    // Sim-only assertion failures (DESIGN §6.12, FLT0) and directed-stimulus
    // expectation mismatches are test-style failures: surface each (the
    // per-cycle table above went to stdout as program output; diagnostics go to
    // the log) and fail the command.
    let failures = sim.assert_failures();
    for f in failures {
        error!("assert failed (cycle {}): {} [{}]", f.cycle, f.msg, f.loc);
    }
    for m in &mismatches {
        error!("expectation failed: {m}");
    }
    let mut problems = Vec::new();
    if !failures.is_empty() {
        problems.push(format!("{} sim-only assertion failure(s)", failures.len()));
    }
    if !mismatches.is_empty() {
        problems.push(format!("{} expectation mismatch(es)", mismatches.len()));
    }
    if let Some(dir) = writes.cosim {
        if !co_simulate(&modules, &rows, resets, dir)? {
            problems.push("a co-simulation mismatch".to_string());
        }
    }
    if !problems.is_empty() {
        bail!("{}; see the log above", problems.join(" and "));
    }
    if let Some(stim) = &directed {
        info!(
            "all {} expectation(s) over {} cycle(s) passed",
            stim.expectation_count(),
            stim.cycles()
        );
    }
    Ok(())
}
