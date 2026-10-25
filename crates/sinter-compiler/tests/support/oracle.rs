//! The oracle's tool runners, shared by `conformance.rs` and `fuzz.rs`:
//! Yosys synthesis, Verilator lint and co-simulation against the reference
//! model, and the generated Rust simulators (DESIGN §7.2 / §7.4).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{run_reference, stimulus_to_bytes};
use sinter_compiler::{rust_sim, verilator, verilog};

/// How strict the Verilator lint pass is.
#[derive(Clone, Copy)]
pub enum Lint {
    /// Every `-Wall` warning fails: the examples' lint-clean guarantee.
    Strict,
    /// As `Strict`, except warnings about the design's own logic, which a
    /// random program is free to have: unused signals and constant
    /// comparisons. Any other warning is still the compiler's fault.
    Design,
}

/// Shared Verilator flag file mirroring the downstream pico-gs config (repo-root
/// `verilator.f`); drives the build / co-simulation step.
pub const VERILATOR_FLAGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../verilator.f");

pub fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn require_tools() -> bool {
    std::env::var("SINTER_REQUIRE_TOOLS").is_ok_and(|v| v == "1")
}

pub fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sinter-conf-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

/// The Verilog sources of a module's `#[verilog]` instances (DESIGN §6.15),
/// which every tool below reads alongside the emitted SV.
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

/// Synthesize with Yosys to confirm the emitted SV is synthesizable.
pub fn run_yosys(sv: &Path, extra: &[PathBuf], top: &str) {
    let reads: String = std::iter::once(sv)
        .chain(extra.iter().map(PathBuf::as_path))
        .map(|f| format!("read_verilog -sv {}; ", f.display()))
        .collect();
    let script = format!("{reads}hierarchy -check -top {top}; proc; opt; synth -top {top}");
    let out = Command::new("yosys")
        .args(["-q", "-p", &script])
        .output()
        .expect("spawn yosys");
    assert!(
        out.status.success(),
        "yosys failed for {top}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Strict Verilator lint (`-Wall`, warnings fatal — note: NO `-Wno-fatal`): the
/// `claude-skill-verilog` gate. Run alongside the pico-gs-flagged co-sim so that
/// generated Verilog must be warning-free even though the co-sim tolerates
/// warnings (matching pico-gs's `-Wno-fatal`).
pub fn lint_verilator(sv: &Path, extra: &[PathBuf], top: &str, lint: Lint) {
    let out = Command::new("verilator")
        .args(["--lint-only", "-Wall"])
        .args(match lint {
            Lint::Strict => &[][..],
            Lint::Design => &["-Wno-UNUSED", "-Wno-UNSIGNED", "-Wno-CMPCONST"][..],
        })
        .arg("--top-module")
        .arg(top)
        .arg(sv)
        .args(extra)
        .output()
        .expect("spawn verilator");
    assert!(
        out.status.success(),
        "verilator lint failed for {top}:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build and run the Verilator model, returning its output trace bytes. The
/// build uses the shared pico-gs flag file (`-f verilator.f`) so the generated
/// SV runs under the same Verilator settings as the downstream project.
pub fn run_verilator(
    dir: &Path,
    sv: &Path,
    extra: &[PathBuf],
    harness: &Path,
    top: &str,
    stim: &Path,
    cycles: usize,
) -> Vec<u8> {
    let obj = dir.join("obj");
    let build = Command::new("verilator")
        .args(["--cc", "--exe", "--build", "-f", VERILATOR_FLAGS])
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
        .expect("spawn verilator");
    assert!(
        build.status.success(),
        "verilator build failed for {top}:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let trace = dir.join("dut_trace.bin");
    let run = Command::new(obj.join("sim"))
        .arg(stim)
        .arg(&trace)
        .arg(cycles.to_string())
        .output()
        .expect("run verilated sim");
    assert!(
        run.status.success(),
        "verilated sim failed for {top}:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&trace).expect("read dut trace")
}

/// Which external tools this run uses; enforces `SINTER_REQUIRE_TOOLS=1`.
pub struct Tools {
    pub yosys: bool,
    pub verilator: bool,
}

pub fn tools() -> Tools {
    let t = Tools {
        yosys: tool_available("yosys"),
        verilator: tool_available("verilator"),
    };
    if require_tools() {
        assert!(t.yosys, "SINTER_REQUIRE_TOOLS=1 but yosys is missing");
        assert!(
            t.verilator,
            "SINTER_REQUIRE_TOOLS=1 but verilator is missing"
        );
    }
    if !t.yosys && !t.verilator {
        eprintln!(
            "skipping Yosys/Verilator conformance: neither tool found \
             (install oss-cad-suite; set SINTER_REQUIRE_TOOLS=1 to enforce)"
        );
    }
    t
}

/// Run one oracle case: emit the SV + harness, compute the reference trace over
/// `stimulus`, then (tools permitting) synthesize, lint, and co-simulate the SV,
/// asserting its trace matches the reference bit-for-bit.
pub fn co_simulate(tools: &Tools, name: &str, module: &Module, stimulus: &[Vec<u64>], lint: Lint) {
    let dir = workdir(name);

    // Emit artifacts.
    let sv = dir.join(format!("{}.sv", module.name));
    std::fs::write(&sv, verilog::emit_module(module)).expect("write sv");
    let harness = dir.join(format!("{}_tb.cpp", module.name));
    std::fs::write(&harness, verilator::emit_cpp_harness(module)).expect("write harness");

    // Stimulus + reference trace.
    let stim_path = dir.join("stimulus.bin");
    std::fs::write(&stim_path, stimulus_to_bytes(stimulus)).expect("write stimulus");
    let reference = run_reference(module, stimulus);

    let extra = foreign_sources(module);
    if tools.yosys {
        run_yosys(&sv, &extra, &module.name);
    }
    if tools.verilator {
        // Strict lint first (warnings fatal), then the pico-gs-flagged co-sim.
        lint_verilator(&sv, &extra, &module.name, lint);
        let dut = run_verilator(
            &dir,
            &sv,
            &extra,
            &harness,
            &module.name,
            &stim_path,
            stimulus.len(),
        );
        assert_eq!(
            dut.len(),
            reference.len(),
            "trace length mismatch for `{name}`"
        );
        // Find the first differing 8-byte word for a useful message.
        for (cycle, (a, b)) in reference
            .chunks_exact(8)
            .zip(dut.chunks_exact(8))
            .enumerate()
        {
            assert_eq!(a, b, "trace mismatch for `{name}` at output word {cycle}");
        }
    }
}

/// Write `contents` to `path` unless it already holds exactly that, so cargo
/// doesn't rebuild an unchanged crate.
pub fn write_if_changed(path: &Path, contents: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, contents).expect("write file");
}

/// Run each case's generated Rust simulator (`--emit rust`) over its stimulus
/// and return the traces, in the reference model's binary format.
///
/// The crates live in one cargo workspace under `root`, with a harness binary
/// that runs any of them over a raw stimulus file. Unchanged files aren't
/// rewritten, so reruns are incremental.
pub fn rust_sim_traces(root: &Path, cases: &[(String, &Module, &[Vec<u64>])]) -> Vec<Vec<u8>> {
    if cases.is_empty() {
        return Vec::new();
    }
    let mut members = vec!["\"harness\"".to_string()];
    let mut deps = String::new();
    let mut arms = String::new();
    for (name, module, _) in cases {
        let krate = format!("{name}-sim");
        for (rel, contents) in rust_sim::emit_crate(module, &krate) {
            write_if_changed(&root.join(&krate).join(rel), &contents);
        }
        members.push(format!("{krate:?}"));
        deps.push_str(&format!("{krate} = {{ path = \"../{krate}\" }}\n"));
        let lib = krate.replace('-', "_");
        let ni = module.inputs().count();
        arms.push_str(&format!(
            "        {name:?} => {{\n\
             \x20           let rows: Vec<[u64; {ni}]> = if {ni} == 0 {{\n\
             \x20               vec![[0; {ni}]; cycles]\n\
             \x20           }} else {{\n\
             \x20               stim.chunks_exact({ni}.max(1))\n\
             \x20                   .map(|c| core::array::from_fn(|k| c[k]))\n\
             \x20                   .collect()\n\
             \x20           }};\n\
             \x20           {lib}::{}::run_raw(&rows).iter().flatten().copied().collect()\n\
             \x20       }}\n",
            rust_sim::model_name(module)
        ));
    }
    write_if_changed(
        &root.join("Cargo.toml"),
        &format!(
            "[workspace]\nresolver = \"2\"\nmembers = [{}]\n",
            members.join(", ")
        ),
    );
    write_if_changed(
        &root.join("harness/Cargo.toml"),
        &format!(
            "[package]\nname = \"harness\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             publish = false\n\n[dependencies]\n{deps}"
        ),
    );
    write_if_changed(
        &root.join("harness/src/main.rs"),
        &format!(
            "//! Run one case's generated simulator over a raw stimulus file:\n\
             //! `harness <case> <cycles> <stimulus.bin> <trace.bin>`.\n\n\
             fn main() {{\n\
             \x20   let args: Vec<String> = std::env::args().collect();\n\
             \x20   let cycles: usize = args[2].parse().expect(\"cycles\");\n\
             \x20   let bytes = std::fs::read(&args[3]).expect(\"read stimulus\");\n\
             \x20   let stim: Vec<u64> = bytes\n\
             \x20       .chunks_exact(8)\n\
             \x20       .map(|b| u64::from_le_bytes(b.try_into().expect(\"8 bytes\")))\n\
             \x20       .collect();\n\
             \x20   let out: Vec<u64> = match args[1].as_str() {{\n\
             {arms}\
             \x20       other => panic!(\"unknown case {{other}}\"),\n\
             \x20   }};\n\
             \x20   let trace: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();\n\
             \x20   std::fs::write(&args[4], trace).expect(\"write trace\");\n\
             }}\n"
        ),
    );

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(&cargo)
        .args(["build", "--quiet", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .status()
        .expect("run cargo");
    assert!(status.success(), "building the generated simulators failed");
    let harness = root.join("target/debug/harness");

    cases
        .iter()
        .map(|(name, _, stimulus)| {
            let stim_path = root.join(format!("{name}.stim"));
            let trace_path = root.join(format!("{name}.trace"));
            std::fs::write(&stim_path, stimulus_to_bytes(stimulus)).expect("write stimulus");
            let status = Command::new(&harness)
                .arg(name)
                .arg(stimulus.len().to_string())
                .arg(&stim_path)
                .arg(&trace_path)
                .status()
                .expect("run harness");
            assert!(status.success(), "{name}: the generated simulator failed");
            std::fs::read(&trace_path).expect("read trace")
        })
        .collect()
}
