//! Co-simulation of the emitted Verilog against the simulator (COSIM1, DESIGN
//! §9.15): the runner behind `sinter sim --cosim` and the conformance oracle
//! (DESIGN §7.4).
//!
//! [`run`] writes the module's SystemVerilog, its `#[module]` children, and
//! the Verilator harness into a directory, builds them with Verilator (and any
//! `#[verilog]` sources), replays a stimulus (and reset schedule), and returns
//! the Verilated output trace, in the format [`crate::sim::run_reference`]
//! writes. [`first_mismatch`] then finds where it departs from the
//! simulator's trace.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::ir::Module;
use crate::sim::{resets_to_bytes, stimulus_to_bytes};
use crate::{verilator, verilog};

/// Verilator's build flags, mirroring the downstream pico-gs integration
/// config (the repository's `verilator.f` carries the same list, with the
/// reasons): every warning reported but non-fatal, a parallel build,
/// assertions, timing, and X-propagation that surfaces missing resets.
pub const VERILATOR_FLAGS: &[&str] = &[
    "-Wall",
    "-Wno-fatal",
    "-j",
    "0",
    "--assert",
    "--timing",
    "--x-assign",
    "unique",
    "--x-initial",
    "unique",
];

/// A co-simulation that couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum CosimError {
    /// A file couldn't be written or read.
    #[error("{action} `{}`: {source}", path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// `verilator` couldn't be started.
    #[error("can't run `verilator` (is it on the PATH?): {0}")]
    NoVerilator(std::io::Error),
    /// Verilator failed to build the design, or the built model failed.
    #[error("{what} failed ({status}):\n{log}")]
    Tool {
        /// Which step.
        what: String,
        /// Its exit status.
        status: String,
        /// The end of its output.
        log: String,
    },
}

/// The first output value on which two traces differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// The 0-based cycle (step, with several clocks).
    pub cycle: usize,
    /// The output port.
    pub port: String,
    /// The simulator's value.
    pub expected: u64,
    /// The Verilog's value.
    pub actual: u64,
}

/// Whether `verilator` runs.
#[must_use]
pub fn verilator_available() -> bool {
    Command::new("verilator")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The Verilog sources of `module`'s `#[verilog]` instances (DESIGN §6.15),
/// sorted, each once.
#[must_use]
pub fn foreign_sources(module: &Module) -> Vec<PathBuf> {
    let mut srcs: Vec<PathBuf> = module
        .instances
        .iter()
        .filter_map(|i| i.src.clone())
        .collect();
    srcs.sort();
    srcs.dedup();
    srcs
}

/// Co-simulate `module` (with its `#[module]` `children`) in `dir`: emit the
/// SystemVerilog and harness, build them with Verilator, and run them over
/// `stimulus`, with `rst_n` asserted in each cycle `k` where `resets[k]`.
/// Returns the Verilated output trace.
///
/// # Errors
/// A [`CosimError`] if a file can't be written, Verilator is missing, or the
/// build or the run fails.
pub fn run(
    dir: &Path,
    module: &Module,
    children: &[Module],
    stimulus: &[Vec<u64>],
    resets: &[bool],
) -> Result<Vec<u8>, CosimError> {
    std::fs::create_dir_all(dir).map_err(|source| CosimError::Io {
        action: "creating",
        path: dir.to_path_buf(),
        source,
    })?;
    let sv = dir.join(format!("{}.sv", module.name));
    write(&sv, &verilog::emit_module(module))?;
    let harness = dir.join(format!("{}_tb.cpp", module.name));
    write(&harness, &verilator::emit_cpp_harness(module))?;
    let mut extra = foreign_sources(module);
    for child in children {
        let path = dir.join(format!("{}.sv", child.name));
        write(&path, &verilog::emit_module(child))?;
        extra.push(path);
        extra.extend(foreign_sources(child));
    }
    extra.sort();
    extra.dedup();
    let exe = verilate(dir, &sv, &extra, &harness, &module.name)?;

    let stim = dir.join("stimulus.bin");
    write_bytes(&stim, &stimulus_to_bytes(stimulus))?;
    let rst = if resets.is_empty() {
        None
    } else {
        let path = dir.join("resets.bin");
        write_bytes(&path, &resets_to_bytes(resets))?;
        Some(path)
    };
    run_harness(dir, &exe, &stim, stimulus.len(), rst.as_deref())
}

/// Build the Verilated model of `top` from `sv`, the `extra` sources, and
/// `harness` in `dir/obj`, returning the executable.
///
/// # Errors
/// A [`CosimError`] if Verilator is missing or the build fails.
pub fn verilate(
    dir: &Path,
    sv: &Path,
    extra: &[PathBuf],
    harness: &Path,
    top: &str,
) -> Result<PathBuf, CosimError> {
    let obj = dir.join("obj");
    let build_once = || {
        let _ = std::fs::remove_dir_all(&obj);
        Command::new("verilator")
            .args(["--cc", "--exe", "--build"])
            .args(VERILATOR_FLAGS)
            .arg("--top-module")
            .arg(top)
            .arg("--Mdir")
            .arg(&obj)
            .arg("-o")
            .arg("sim")
            .arg(sv)
            .args(extra)
            .arg(harness)
            .output()
            .map_err(CosimError::NoVerilator)
    };
    let mut build = build_once()?;
    if !build.status.success() {
        // Verilator's parallel build (`-j 0`) fails now and then with nothing
        // wrong (about 1 build in 100); one clean retry tells that apart from
        // a real failure.
        build = build_once()?;
    }
    check(&build, format!("verilator build of `{top}`"))?;
    Ok(obj.join("sim"))
}

/// Run the Verilated `exe` over the stimulus file for `cycles` rows (and the
/// optional reset schedule), returning its trace.
///
/// # Errors
/// A [`CosimError`] if the model fails or its trace can't be read.
pub fn run_harness(
    dir: &Path,
    exe: &Path,
    stim: &Path,
    cycles: usize,
    resets: Option<&Path>,
) -> Result<Vec<u8>, CosimError> {
    let trace = dir.join("dut_trace.bin");
    let run = Command::new(exe)
        .arg(stim)
        .arg(&trace)
        .arg(cycles.to_string())
        .args(resets)
        .output()
        .map_err(|source| CosimError::Io {
            action: "running",
            path: exe.to_path_buf(),
            source,
        })?;
    check(&run, "the Verilated model".to_string())?;
    std::fs::read(&trace).map_err(|source| CosimError::Io {
        action: "reading",
        path: trace,
        source,
    })
}

/// Where the Verilog's trace `actual` first departs from the simulator's
/// `expected` (both as [`crate::sim::run_reference`] writes them, one word
/// per output per cycle), or `None` if they agree. A trace that ends early
/// mismatches at its first missing word, read as 0.
#[must_use]
pub fn first_mismatch(module: &Module, expected: &[u8], actual: &[u8]) -> Option<Mismatch> {
    let outputs: Vec<&str> = module.outputs().map(|p| p.name.as_str()).collect();
    if outputs.is_empty() {
        return None;
    }
    let word = |t: &[u8], i: usize| {
        t.get(i * 8..i * 8 + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    };
    let words = expected.len().max(actual.len()) / 8;
    (0..words).find_map(|i| {
        let (e, a) = (word(expected, i), word(actual, i));
        (e != a).then(|| Mismatch {
            cycle: i / outputs.len(),
            port: outputs[i % outputs.len()].to_string(),
            expected: e.unwrap_or(0),
            actual: a.unwrap_or(0),
        })
    })
}

/// Fail with the end of `out`'s log unless it succeeded.
fn check(out: &Output, what: String) -> Result<(), CosimError> {
    if out.status.success() {
        return Ok(());
    }
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The error comes last, after any (non-fatal) warnings.
    let lines: Vec<&str> = log.lines().collect();
    Err(CosimError::Tool {
        what,
        status: out.status.to_string(),
        log: lines[lines.len().saturating_sub(40)..].join("\n"),
    })
}

fn write(path: &Path, text: &str) -> Result<(), CosimError> {
    write_bytes(path, text.as_bytes())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), CosimError> {
    std::fs::write(path, bytes).map_err(|source| CosimError::Io {
        action: "writing",
        path: path.to_path_buf(),
        source,
    })
}
