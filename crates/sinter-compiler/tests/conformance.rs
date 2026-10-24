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
use sinter_compiler::sim::{gen_stimulus, run_reference, stimulus_to_bytes, Simulator};
use sinter_compiler::stimulus::Stimulus;
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
    // O1 `Option<T>`: Some/None construction, an Option-typed register (reset
    // `None`), and a `match` that lowers to a mux on the validity bit.
    Example {
        name: "fn_option",
        source: include_str!("../../../examples/fn_option.snt"),
    },
    // O2 `?`: validity AND-reduction (DESIGN §6.4) — two `?`s combine into a
    // single result validity, consumed by a guarded hold.
    Example {
        name: "fn_option_try",
        source: include_str!("../../../examples/fn_option_try.snt"),
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
    // MEM1: a 1R1W synchronous block RAM (`mem<u8, 16>`). The pseudo-random
    // stimulus drives we/waddr/wdata/raddr, exercising write+read round-trips,
    // the `Static(1)` read latency, and same-address READ_FIRST collisions —
    // co-simulated sim vs Verilog bit-for-bit (DESIGN §6.6/§6.9/§9.6/§7.4).
    Example {
        name: "fn_ram",
        source: include_str!("../../../examples/fn_ram.snt"),
    },
    // ROM1: a read-only `rom<u8, 8>` lookup table (squares) initialized from an
    // inline array literal. The pseudo-random `addr` reads its compile-time
    // contents at the `Static(1)` latency — co-simulated sim vs Verilog bit-for-bit
    // (DESIGN §6.6/§6.9/§9.7/§7.4).
    Example {
        name: "fn_rom",
        source: include_str!("../../../examples/fn_rom.snt"),
    },
    // CHAN1: a fixed-capacity lossless `fifo<u8, 4>`. The pseudo-random stimulus
    // drives `incoming` (an `Option<u8>` push) and `take` (a pop flag), exercising
    // enqueue, guarded-hold on full, pop-from-front, and same-cycle push+pop
    // (READ_FIRST) stream-through. Lowered by the source-to-source `fifo` desugar
    // to a shift-register struct — co-simulated sim vs Verilog bit-for-bit
    // (DESIGN §6.11 / §9.8 / §7.4).
    Example {
        name: "fn_fifo",
        source: include_str!("../../../examples/fn_fifo.snt"),
    },
    // CHAN2: a lossy overwrite-oldest `ring<u8, 4>`. Same stimulus shape as
    // `fn_fifo` — an `Option<u8>` push and a pop flag — but the producer ignores
    // `is_full()`, so the random drive keeps the ring saturated and exercises the
    // drop-oldest path (the `shiftf` term a `fifo` never takes) alongside ordinary
    // enqueue/pop — co-simulated sim vs Verilog bit-for-bit
    // (DESIGN §6.11 / §9.8 / §7.4).
    // CHAN3: a fifo's producer and consumer each see only a directional view
    // (`Sender` / `Receiver`); pushes go through `Sender::offer`.
    Example {
        name: "fn_fifo_views",
        source: include_str!("../../../examples/fn_fifo_views.snt"),
    },
    // CHAN4: a register fifo and a BRAM fifo (a `mem` ring buffer with a
    // prefetched second word and a bypass) fed the same traffic. The oracle
    // checks the BRAM version's Verilog matches its model bit-for-bit.
    Example {
        name: "fn_fifo_bram",
        source: include_str!("../../../examples/fn_fifo_bram.snt"),
    },
    // CLK1 (DESIGN §6.7 / §9.14): two clock domains with coprime periods (3, 7)
    // and no crossing. The sim and the Verilator harness replay the same edge
    // schedule, including both edges at once.
    Example {
        name: "fn_two_clocks",
        source: include_str!("../../../examples/fn_two_clocks.snt"),
    },
    // CLK2: `sync` and `pulse` crossings from a period-2 domain into a
    // period-5 one; the synchronizers are the only cross-domain reads.
    Example {
        name: "fn_cdc_sync",
        source: include_str!("../../../examples/fn_cdc_sync.snt"),
    },
    // CLK3: a `mailbox` (req / ack handshake) carrying u8 words from a
    // period-3 domain to a period-4 one through Sender / Receiver views.
    Example {
        name: "fn_mailbox",
        source: include_str!("../../../examples/fn_mailbox.snt"),
    },
    Example {
        name: "fn_ring",
        source: include_str!("../../../examples/fn_ring.snt"),
    },
    // CHAN2: a depth-1 newest-wins `flow<u8>`. The random `incoming` stimulus
    // exercises overwrite (push over a live sample) and hold (no push), with the
    // slot erased to an `Option<u8>` register — co-simulated sim vs Verilog
    // bit-for-bit (DESIGN §6.11 / §9.8 / §7.4).
    Example {
        name: "fn_flow",
        source: include_str!("../../../examples/fn_flow.snt"),
    },
    // FLT0: a sim-only `assert!` fault check (a saturating counter's clamp
    // invariant). The invariant holds for all stimulus, so the sim never aborts
    // and the emitted SV carries zero assertion bits — co-simulated sim vs Verilog
    // bit-for-bit, confirming the assert is compile-out clean (DESIGN §6.12 / §9.9
    // / §7.4).
    Example {
        name: "fn_assert",
        source: include_str!("../../../examples/fn_assert.snt"),
    },
    // BIT1a: constant bit shifts `<<` / `>>`. The random `x`/`s` stimulus drives
    // `x >> 4` (logical, u8->u4), `x << 2` (widening, u8->u10), and `s >> 3`
    // (arithmetic, i8->i5) — co-simulated sim vs Verilog bit-for-bit reusing the
    // IR const-shift nodes (DESIGN §6.13 / §9.10 / §7.4).
    Example {
        name: "fn_shift",
        source: include_str!("../../../examples/fn_shift.snt"),
    },
    // BIT1b: bit-slice `x[hi:lo]`. The random `x` stimulus drives field
    // extraction — `x[7:4]`, `x[3:0]`, a middle field `x[5:3]`, and a single bit
    // `x[7:7]` — co-simulated sim vs Verilog bit-for-bit, lowering to a logical
    // shift + truncate (DESIGN §6.13 / §9.10 / §7.4).
    Example {
        name: "fn_slice",
        source: include_str!("../../../examples/fn_slice.snt"),
    },
    // BIT1c: bit concatenation `a @ b`. The random `x`/`b` stimulus drives a
    // slice+concat nibble swap (`x[3:0] @ x[7:4]`) and a widening join
    // (`x @ b`, u8@u4->u12) — co-simulated sim vs Verilog bit-for-bit, lowering
    // to a shift + zero-extend + or (DESIGN §6.13 / §9.10 / §7.4).
    Example {
        name: "fn_concat",
        source: include_str!("../../../examples/fn_concat.snt"),
    },
    // REG1: a runtime-indexed register file (`[u8; 8]`, 2 read ports + 1 write).
    // The random stimulus drives `we`/`rd`/`wdata`/`rs1`/`rs2`, exercising the
    // combinational N:1 read mux trees and the `we`-gated 1:N write decode (with
    // register 0 hardwired to zero) — co-simulated sim vs Verilog bit-for-bit
    // (DESIGN §6.14 / §9.11 / §7.4).
    Example {
        name: "fn_regfile",
        source: include_str!("../../../examples/fn_regfile.snt"),
    },
    // BIT2: variable (barrel) shift by a runtime amount. The random `x`/`s`/`sh`
    // stimulus drives `x << sh` (logical), `x >> sh` (logical), and `s >> sh`
    // (arithmetic), with `sh: u4` exercising shifts past the 8-bit width (-> 0 /
    // sign fill) — co-simulated sim vs a native Verilog barrel shift bit-for-bit
    // (DESIGN §6.13 / §9.10 / §7.4).
    Example {
        name: "fn_barrel",
        source: include_str!("../../../examples/fn_barrel.snt"),
    },
    // M1 multiplex: a `multiplex fn` time-folds its `yield`-phases onto one
    // register per carried value + a phase counter, sharing ONE multiplier across
    // the phases (operand-muxed, IR Mul-CSE'd). dot2 (2 phases) / dot3 (3 phases,
    // wrap-to-0) co-sim sim vs Verilog bit-for-bit (DESIGN §3.2 / §3.3 / §7.4).
    Example {
        name: "fn_dot2_multiplex",
        source: include_str!("../../../examples/fn_dot2_multiplex.snt"),
    },
    Example {
        name: "fn_dot3_multiplex",
        source: include_str!("../../../examples/fn_dot3_multiplex.snt"),
    },
    // MEM2 (DESIGN §9.6): a `mem<u32, 16, 2>` with two read ports and a u4
    // byte-enable write mask. The random stimulus covers partial writes and
    // same-cycle read/write collisions on both ports.
    Example {
        name: "fn_mem2",
        source: include_str!("../../../examples/fn_mem2.snt"),
    },
    // Sign-changing casts (DESIGN §6.3): `uN` <-> `iN` in `wrap` and `sat`
    // modes at narrower / equal / wider widths. The random `u`/`s` stimulus hits
    // both clamp branches; co-simulated sim vs Verilog bit-for-bit.
    Example {
        name: "fn_signcast",
        source: include_str!("../../../examples/fn_signcast.snt"),
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
    /// Cycles of random stimulus to co-simulate.
    cycles: usize,
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
        cycles: CYCLES,
    },
    // ROM1b: a `rom<u8, 8>` whose contents are loaded from `table.hex` via
    // `include_hex!` (compile-time, relative to the source, embedded inline). The
    // random `addr` reads the file's contents at the `Static(1)` latency —
    // co-simulated sim vs Verilog bit-for-bit (DESIGN §9.7 / §7.4).
    Project {
        name: "rom_hex",
        root: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/rom_hex/main.snt"
        ),
        cycles: CYCLES,
    },
    // EXT1: a `#[verilog]` binding to the hand-written `rtl/acc16.v`. The sim
    // runs the Sinter reference model; Verilator runs the real module. Both
    // must agree bit-for-bit, so this checks the model against the IP
    // (DESIGN §6.15 / §9.13 / §7.4).
    Project {
        name: "verilog_acc",
        root: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/verilog_acc/main.snt"
        ),
        cycles: CYCLES,
    },
    // RV32 cores (DESIGN §9.12), sharing examples/rv32's decoder, ALU, and
    // program (Fibonacci to the output port). `mini` is the fetch-ahead
    // stepping stone (step 5), `pipe` the 5-stage pipeline with forwarding and
    // hazard detection (step 6). The random `run` input stalls them about half
    // the time, so 1000 cycles cover the whole program, its halt, and the
    // halted state.
    Project {
        name: "rv32_mini",
        root: concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/rv32/mini.snt"),
        cycles: 1000,
    },
    Project {
        name: "rv32_pipe",
        root: concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/rv32/pipe.snt"),
        cycles: 1000,
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

/// FLT1b: examples compiled with `--faults=on` so the synthesized silicon fault
/// latch (the sticky bitmap + the `fault_code` sideband) is co-simulated. The
/// sideband is an ordinary output port, so the oracle diffs it sim↔Verilog with no
/// harness change (DESIGN §6.12 / §9.9).
const FAULT_EXAMPLES: &[Example] = &[Example {
    name: "fn_fault",
    source: include_str!("../../../examples/fn_fault.snt"),
}];

fn compile_one_faults_on(source: &str) -> Module {
    let design = sinter_compiler::compile_str_faults(source, sinter_compiler::FaultMode::On)
        .expect("fault example should compile in on-mode");
    design.modules.into_iter().next().expect("one module")
}

fn compile_project_one(root: &str) -> Module {
    let design = sinter_compiler::compile_project(std::path::Path::new(root))
        .expect("project should compile");
    design.modules.into_iter().next().expect("one module")
}

/// Every oracle case as `(name, module, cycles)` — the single-file examples
/// plus the multi-file projects.
fn all_cases() -> Vec<(String, Module, usize)> {
    let mut cases: Vec<(String, Module, usize)> = EXAMPLES
        .iter()
        .map(|ex| (ex.name.to_string(), compile_one(ex.source), CYCLES))
        .collect();
    cases.extend(
        PROJECTS
            .iter()
            .map(|p| (p.name.to_string(), compile_project_one(p.root), p.cycles)),
    );
    // FLT1b fault-latch examples, compiled in `--faults=on` mode.
    cases.extend(FAULT_EXAMPLES.iter().map(|ex| {
        (
            ex.name.to_string(),
            compile_one_faults_on(ex.source),
            CYCLES,
        )
    }));
    cases
}

/// The Verilog sources of a module's `#[verilog]` instances (DESIGN §6.15),
/// which every tool below reads alongside the emitted SV.
fn foreign_sources(module: &Module) -> Vec<PathBuf> {
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
fn run_yosys(sv: &Path, extra: &[PathBuf], top: &str) {
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
fn lint_verilator(sv: &Path, extra: &[PathBuf], top: &str) {
    let out = Command::new("verilator")
        .args(["--lint-only", "-Wall", "--top-module"])
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
fn run_verilator(
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
struct Tools {
    yosys: bool,
    verilator: bool,
}

fn tools() -> Tools {
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
fn co_simulate(tools: &Tools, name: &str, module: &Module, stimulus: &[Vec<u64>]) {
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
        lint_verilator(&sv, &extra, &module.name);
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

#[test]
fn examples_pass_the_oracle() {
    let tools = tools();
    for (name, module, cycles) in &all_cases() {
        let stimulus = gen_stimulus(module, *cycles, 0xDEAD_BEEF);
        co_simulate(&tools, name, module, &stimulus);
    }
}

/// Examples with a directed CSV stimulus (`examples/<name>.csv`).
struct Directed {
    name: &'static str,
    source: &'static str,
    csv: &'static str,
}

const DIRECTED: &[Directed] = &[
    Directed {
        name: "fn_counter",
        source: include_str!("../../../examples/fn_counter.snt"),
        csv: include_str!("../../../examples/fn_counter.csv"),
    },
    Directed {
        name: "fn_mem2",
        source: include_str!("../../../examples/fn_mem2.snt"),
        csv: include_str!("../../../examples/fn_mem2.csv"),
    },
    Directed {
        name: "fn_signcast",
        source: include_str!("../../../examples/fn_signcast.snt"),
        csv: include_str!("../../../examples/fn_signcast.csv"),
    },
    Directed {
        name: "fn_signed",
        source: include_str!("../../../examples/fn_signed.snt"),
        csv: include_str!("../../../examples/fn_signed.csv"),
    },
];

/// Directed stimulus: the CSV's expected outputs hold on the reference model,
/// and the Verilog co-simulates bit-for-bit over the same directed rows — so a
/// directed test passing under `sinter sim` also holds for the emitted SV.
#[test]
fn directed_examples_pass_the_oracle() {
    let tools = tools();
    for ex in DIRECTED {
        let module = compile_one(ex.source);
        let stim = Stimulus::parse_csv(&module, ex.csv).expect("example CSV parses");
        assert!(stim.expectation_count() > 0, "`{}` checks nothing", ex.name);

        let mut sim = Simulator::new(&module);
        let mut mismatches = Vec::new();
        for (k, row) in stim.inputs.iter().enumerate() {
            let inputs = module
                .inputs()
                .map(|p| p.name.clone())
                .zip(row.iter().copied())
                .collect();
            mismatches.extend(stim.check_cycle(&module, k, &sim.step(&inputs)));
        }
        assert!(
            mismatches.is_empty(),
            "`{}` expectations failed:\n{}",
            ex.name,
            mismatches
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );

        co_simulate(
            &tools,
            &format!("{}-directed", ex.name),
            &module,
            &stim.inputs,
        );
    }
}

/// EXT1 (DESIGN §6.15): the oracle checks a `#[verilog]` reference model
/// against the real module. A model that reports `sum` one cycle early must
/// produce a different trace from the Verilated `acc16`.
#[test]
fn wrong_verilog_model_fails_the_oracle() {
    let tools = tools();
    if !tools.verilator {
        return;
    }
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/verilog_acc");
    let dir = workdir("verilog_acc-wrong-model");
    std::fs::create_dir_all(dir.join("rtl")).expect("create rtl dir");
    std::fs::copy(example.join("rtl/acc16.v"), dir.join("rtl/acc16.v")).expect("copy acc16.v");
    let main = std::fs::read_to_string(example.join("main.snt")).expect("read main.snt");
    let wrong = main.replace("sum: self.acc", "sum: next");
    assert_ne!(main, wrong, "the mutation must apply");
    std::fs::write(dir.join("main.snt"), wrong).expect("write main.snt");

    let module = compile_project_one(dir.join("main.snt").to_str().expect("utf-8 path"));
    let sv = dir.join(format!("{}.sv", module.name));
    std::fs::write(&sv, verilog::emit_module(&module)).expect("write sv");
    let harness = dir.join(format!("{}_tb.cpp", module.name));
    std::fs::write(&harness, verilator::emit_cpp_harness(&module)).expect("write harness");
    let stimulus = gen_stimulus(&module, CYCLES, 0xDEAD_BEEF);
    let stim_path = dir.join("stimulus.bin");
    std::fs::write(&stim_path, stimulus_to_bytes(&stimulus)).expect("write stimulus");

    let reference = run_reference(&module, &stimulus);
    let dut = run_verilator(
        &dir,
        &sv,
        &foreign_sources(&module),
        &harness,
        &module.name,
        &stim_path,
        CYCLES,
    );
    assert_ne!(
        reference, dut,
        "a wrong `#[verilog]` model must disagree with the real module"
    );
}
