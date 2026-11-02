//! The oracle's tool runners, shared by `conformance.rs` and `fuzz.rs`:
//! Yosys synthesis, Verilator lint and co-simulation against the reference
//! model, and the generated Rust simulators (DESIGN §7.2 / §7.4). The
//! Verilator build and run are the library's (`sinter_compiler::cosim`, also
//! behind `sinter sim --cosim`); these wrappers panic on failure.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{resets_to_bytes, run_reference_with_resets, stimulus_to_bytes};
use sinter_compiler::{cosim, rust_sim, verilog};

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

/// The repository's Verilator flag file, mirroring the downstream pico-gs
/// config; `cosim::VERILATOR_FLAGS` must list the same flags.
pub const VERILATOR_FLAGS_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../verilator.f");

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
    cosim::foreign_sources(module)
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
    run_verilator_with_resets(dir, sv, extra, harness, top, stim, cycles, None)
}

/// [`run_verilator`], with the harness's optional reset schedule (RST1).
#[allow(clippy::too_many_arguments)]
pub fn run_verilator_with_resets(
    dir: &Path,
    sv: &Path,
    extra: &[PathBuf],
    harness: &Path,
    top: &str,
    stim: &Path,
    cycles: usize,
    resets: Option<&Path>,
) -> Vec<u8> {
    let exe = cosim::verilate(dir, sv, extra, harness, top).unwrap_or_else(|e| panic!("{e}"));
    cosim::run_harness(dir, &exe, stim, cycles, resets).unwrap_or_else(|e| panic!("{top}: {e}"))
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
/// asserting its trace matches the reference bit-for-bit. `children` are the
/// modules generated for `module`'s `#[module]` instances, which the tools read
/// alongside it; the reference simulates `module` alone (it inlines them).
pub fn co_simulate(
    tools: &Tools,
    name: &str,
    module: &Module,
    children: &[Module],
    stimulus: &[Vec<u64>],
    lint: Lint,
) {
    co_simulate_with_resets(tools, name, module, children, stimulus, &[], lint);
}

/// [`co_simulate`], with `rst_n` asserted in each cycle `k` where
/// `resets[k]` (RST1): the reference uses `step_in_reset`, the Verilator
/// harness its reset schedule.
pub fn co_simulate_with_resets(
    tools: &Tools,
    name: &str,
    module: &Module,
    children: &[Module],
    stimulus: &[Vec<u64>],
    resets: &[bool],
    lint: Lint,
) {
    let dir = workdir(name);
    let reference = run_reference_with_resets(module, stimulus, resets);

    // The emitted SV, for Yosys and the lint.
    let sv = dir.join(format!("{}.sv", module.name));
    std::fs::write(&sv, verilog::emit_module(module)).expect("write sv");
    let mut extra = foreign_sources(module);
    for child in children {
        let path = dir.join(format!("{}.sv", child.name));
        std::fs::write(&path, verilog::emit_module(child)).expect("write child sv");
        extra.push(path);
        extra.extend(foreign_sources(child));
    }
    extra.sort();
    extra.dedup();
    if tools.yosys {
        run_yosys(&sv, &extra, &module.name);
    }
    if tools.verilator {
        // Strict lint first (warnings fatal), then the pico-gs-flagged co-sim.
        lint_verilator(&sv, &extra, &module.name, lint);
        let dut = cosim::run(&dir, module, children, stimulus, resets)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            dut.len(),
            reference.len(),
            "trace length mismatch for `{name}`"
        );
        if let Some(m) = cosim::first_mismatch(module, &reference, &dut) {
            panic!(
                "trace mismatch for `{name}` at cycle {}: `{}` is {} in the simulator, {} in \
                 the Verilog",
                m.cycle, m.port, m.expected, m.actual
            );
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
    rust_sim_traces_with_resets(root, cases, &[])
}

/// [`rust_sim_traces`], where case `i` asserts `rst_n` in each cycle `k`
/// with `resets[i][k]` (RST1; the generated `step_in_reset`). A case past
/// the end of `resets` has no resets.
pub fn rust_sim_traces_with_resets(
    root: &Path,
    cases: &[(String, &Module, &[Vec<u64>])],
    resets: &[&[bool]],
) -> Vec<Vec<u8>> {
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
             \x20           // Step one cycle at a time, checking `eval` predicts\n\
             \x20           // `step`'s outputs without changing anything.\n\
             \x20           let mut sim = {lib}::{model}::new();\n\
             \x20           let mut out = Vec::new();\n\
             \x20           for (k, row) in rows.iter().enumerate() {{\n\
             \x20               let ins = {lib}::Inputs::from_raw(row);\n\
             \x20               if rst.get(k) == Some(&1) {{\n\
             \x20                   out.extend(sim.step_in_reset(&ins).to_raw());\n\
             \x20                   continue;\n\
             \x20               }}\n\
             \x20               let predicted = sim.eval(&ins);\n\
             \x20               assert_eq!(predicted, sim.eval(&ins), \"eval changed the state\");\n\
             \x20               let o = sim.step(&ins);\n\
             \x20               assert_eq!(predicted, o, \"eval disagrees with step\");\n\
             \x20               out.extend(o.to_raw());\n\
             \x20           }}\n\
             \x20           out\n\
             \x20       }}\n",
            model = rust_sim::model_name(module)
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
             //! `harness <case> <cycles> <stimulus.bin> <trace.bin> [<resets.bin>]`.\n\n\
             fn main() {{\n\
             \x20   let args: Vec<String> = std::env::args().collect();\n\
             \x20   let cycles: usize = args[2].parse().expect(\"cycles\");\n\
             \x20   let bytes = std::fs::read(&args[3]).expect(\"read stimulus\");\n\
             \x20   let stim: Vec<u64> = bytes\n\
             \x20       .chunks_exact(8)\n\
             \x20       .map(|b| u64::from_le_bytes(b.try_into().expect(\"8 bytes\")))\n\
             \x20       .collect();\n\
             \x20   let rst: Vec<u8> = args.get(5).map(|p| std::fs::read(p).expect(\"read resets\")).unwrap_or_default();\n\
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
        .enumerate()
        .map(|(i, (name, _, stimulus))| {
            let stim_path = root.join(format!("{name}.stim"));
            let trace_path = root.join(format!("{name}.trace"));
            std::fs::write(&stim_path, stimulus_to_bytes(stimulus)).expect("write stimulus");
            let mut cmd = Command::new(&harness);
            cmd.arg(name)
                .arg(stimulus.len().to_string())
                .arg(&stim_path)
                .arg(&trace_path);
            if let Some(r) = resets.get(i).filter(|r| !r.is_empty()) {
                let rst_path = root.join(format!("{name}.rst"));
                std::fs::write(&rst_path, resets_to_bytes(r)).expect("write resets");
                cmd.arg(&rst_path);
            }
            let status = cmd.status().expect("run harness");
            assert!(status.success(), "{name}: the generated simulator failed");
            std::fs::read(&trace_path).expect("read trace")
        })
        .collect()
}
