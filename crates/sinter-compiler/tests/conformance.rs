//! End-to-end conformance oracle (DESIGN §7.4).
//!
//! For each example: compile to SystemVerilog + a Verilator harness, run the
//! Rust reference model over a deterministic stimulus, and — when the tools are
//! available — (1) synthesize the SV with Yosys and (2) simulate it with
//! Verilator, asserting its output trace matches the reference bit-for-bit.
//!
//! Yosys/Verilator steps auto-skip when the tools are absent (e.g. a plain
//! sandbox) and run in the devcontainer / CI where `oss-cad-suite` provides
//! them. Set `SINTER_REQUIRE_TOOLS=1` to make their absence a failure.

use std::path::{Path, PathBuf};
use std::process::Command;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference, stimulus_to_bytes};
use sinter_compiler::{compile_str, verilator, verilog};

const CYCLES: usize = 64;

/// Shared Verilator flag file mirroring the downstream pico-gs config (repo-root
/// `verilator.f`); drives the build / co-simulation step.
const VERILATOR_FLAGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../verilator.f");

struct Example {
    name: &'static str,
    source: &'static str,
}

const EXAMPLES: &[Example] = &[
    Example {
        name: "counter",
        source: include_str!("../../../examples/counter.snt"),
    },
    Example {
        name: "accumulator",
        source: include_str!("../../../examples/accumulator.snt"),
    },
    // Functional surface (DESIGN §2.1), lowered by F2 to the same IR.
    Example {
        name: "fn_counter",
        source: include_str!("../../../examples/fn_counter.snt"),
    },
    // Functional surface F3: enum/match, sub-state method, nested struct.
    Example {
        name: "fn_updown",
        source: include_str!("../../../examples/fn_updown.snt"),
    },
    // Signed arithmetic + signed narrowing `wrap` cast.
    Example {
        name: "fn_signed",
        source: include_str!("../../../examples/fn_signed.snt"),
    },
    // F4 fixed-point: Q-format multiply, saturating cast, rounding cast.
    Example {
        name: "fn_fixed",
        source: include_str!("../../../examples/fn_fixed.snt"),
    },
    Example {
        name: "fn_sat",
        source: include_str!("../../../examples/fn_sat.snt"),
    },
    Example {
        name: "fn_round",
        source: include_str!("../../../examples/fn_round.snt"),
    },
    // Nested width-inferred arithmetic: `a*b + c*d + acc` (multiply-accumulate).
    Example {
        name: "fn_mac",
        source: include_str!("../../../examples/fn_mac.snt"),
    },
    // PL3a/PL3b: a `pipeline fn` (latency 2) desugared to a stage-register instance
    // + `advance`. The `valid` input drives `in_valid`, so the pseudo-random
    // stimulus injects input bubbles (`valid = 0`) that the valid-bit chain carries
    // to `out_valid` exactly `latency` cycles later — co-simulated sim vs Verilog
    // bit-for-bit (DESIGN §3.1 / §9.3 / §7.4).
    Example {
        name: "fn_mac_pipeline",
        source: include_str!("../../../examples/fn_mac_pipeline.snt"),
    },
    // PL3c: global-stall backpressure — an `en`-gated guarded hold freezes the
    // whole pipeline; the random `en` stimulus exercises stall + advance, co-sim
    // bit-for-bit (DESIGN §3.1 / §9.3).
    Example {
        name: "fn_pipeline_stall",
        source: include_str!("../../../examples/fn_pipeline_stall.snt"),
    },
    // Functional surface F3 headline: the full Go Board (debounce, edge-detect,
    // BCD up/down, 7-seg decode, nested structs, enum/match, methods, arrays).
    Example {
        name: "go_board_counter",
        source: include_str!("../../../examples/go_board_counter.snt"),
    },
];

/// Multi-file projects (DESIGN §10), compiled via `compile_project` from their
/// root file and run through the same oracle as the single-file examples.
struct Project {
    name: &'static str,
    root: &'static str,
}

const PROJECTS: &[Project] = &[
    // Module-system example: `mod`/`use`/`pub` with a public struct field read
    // across the module boundary.
    Project {
        name: "sat_counter",
        root: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/sat_counter/main.snt"
        ),
    },
];

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn require_tools() -> bool {
    std::env::var("SINTER_REQUIRE_TOOLS").is_ok_and(|v| v == "1")
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sinter-conf-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    dir
}

fn compile_one(source: &str) -> Module {
    let design = compile_str(source).expect("example should compile");
    design.modules.into_iter().next().expect("one module")
}

fn compile_project_one(root: &str) -> Module {
    let design = sinter_compiler::compile_project(std::path::Path::new(root))
        .expect("project should compile");
    design.modules.into_iter().next().expect("one module")
}

/// Every oracle case as `(name, module)` — the single-file examples plus the
/// multi-file projects.
fn all_cases() -> Vec<(String, Module)> {
    let mut cases: Vec<(String, Module)> = EXAMPLES
        .iter()
        .map(|ex| (ex.name.to_string(), compile_one(ex.source)))
        .collect();
    cases.extend(
        PROJECTS
            .iter()
            .map(|p| (p.name.to_string(), compile_project_one(p.root))),
    );
    cases
}

/// Synthesize with Yosys to confirm the emitted SV is synthesizable.
fn run_yosys(sv: &Path, top: &str) {
    let script = format!(
        "read_verilog -sv {}; hierarchy -check -top {top}; proc; opt; synth -top {top}",
        sv.display()
    );
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
fn lint_verilator(sv: &Path, top: &str) {
    let out = Command::new("verilator")
        .args(["--lint-only", "-Wall", "--top-module"])
        .arg(top)
        .arg(sv)
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
fn run_verilator(dir: &Path, sv: &Path, harness: &Path, top: &str, stim: &Path) -> Vec<u8> {
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
        .arg(CYCLES.to_string())
        .output()
        .expect("run verilated sim");
    assert!(
        run.status.success(),
        "verilated sim failed for {top}:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&trace).expect("read dut trace")
}

#[test]
fn examples_pass_the_oracle() {
    let have_yosys = tool_available("yosys");
    let have_verilator = tool_available("verilator");

    if require_tools() {
        assert!(have_yosys, "SINTER_REQUIRE_TOOLS=1 but yosys is missing");
        assert!(
            have_verilator,
            "SINTER_REQUIRE_TOOLS=1 but verilator is missing"
        );
    }
    if !have_yosys && !have_verilator {
        eprintln!(
            "skipping Yosys/Verilator conformance: neither tool found \
             (install oss-cad-suite; set SINTER_REQUIRE_TOOLS=1 to enforce)"
        );
    }

    for (name, module) in &all_cases() {
        let dir = workdir(name);

        // Emit artifacts.
        let sv = dir.join(format!("{}.sv", module.name));
        std::fs::write(&sv, verilog::emit_module(module)).expect("write sv");
        let harness = dir.join(format!("{}_tb.cpp", module.name));
        std::fs::write(&harness, verilator::emit_cpp_harness(module)).expect("write harness");

        // Stimulus + reference trace.
        let stimulus = gen_stimulus(module, CYCLES, 0xDEAD_BEEF);
        let stim_path = dir.join("stimulus.bin");
        std::fs::write(&stim_path, stimulus_to_bytes(&stimulus)).expect("write stimulus");
        let reference = run_reference(module, &stimulus);

        if have_yosys {
            run_yosys(&sv, &module.name);
        }
        if have_verilator {
            // Strict lint first (warnings fatal), then the pico-gs-flagged co-sim.
            lint_verilator(&sv, &module.name);
            let dut = run_verilator(&dir, &sv, &harness, &module.name, &stim_path);
            assert_eq!(
                dut.len(),
                reference.len(),
                "trace length mismatch for `{}`",
                module.name
            );
            // Find the first differing 8-byte word for a useful message.
            for (cycle, (a, b)) in reference
                .chunks_exact(8)
                .zip(dut.chunks_exact(8))
                .enumerate()
            {
                assert_eq!(
                    a, b,
                    "trace mismatch for `{}` at output word {cycle}",
                    module.name
                );
            }
        }
    }
}
