//! End-to-end conformance oracle (DESIGN §7.4).
//!
//! For each example: compile to SystemVerilog + a Verilator harness, run the
//! Rust reference model over a deterministic stimulus, and — when the tools are
//! available — (1) synthesize the SV with Yosys and (2) simulate it with
//! Verilator, asserting its output trace matches the reference bit-for-bit.
//!
//! Every case's generated Rust simulator (`--emit rust`, DESIGN §7.2) is also
//! built and run over the same stimulus, and must match the reference too.
//!
//! Yosys/Verilator steps auto-skip when the tools are absent (e.g. a plain
//! sandbox) and run in the devcontainer / CI where `oss-cad-suite` provides
//! them. Set `SINTER_REQUIRE_TOOLS=1` to make their absence a failure.

#[path = "support/oracle.rs"]
mod oracle;

use std::path::{Path, PathBuf};

use oracle::{co_simulate, foreign_sources, run_verilator, rust_sim_traces, tools, workdir, Lint};
use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference, stimulus_to_bytes, Simulator};
use sinter_compiler::stimulus::Stimulus;
use sinter_compiler::{compile_str, verilator, verilog};

const CYCLES: usize = 64;

struct Example {
    name: &'static str,
    source: &'static str,
}

const EXAMPLES: &[Example] = &[
    Example {
        name: "fn_counter",
        source: include_str!("../../../examples/fn_counter.snt"),
    },
    // Signed arithmetic and a wrapping cast.
    Example {
        name: "fn_accumulator",
        source: include_str!("../../../examples/fn_accumulator.snt"),
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
    // CLK4: an `async_fifo<u8, 16>` from a period-3 domain to a period-4 one:
    // gray pointers through synchronizers and a dual-clock RAM.
    Example {
        name: "fn_async_fifo",
        source: include_str!("../../../examples/fn_async_fifo.snt"),
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
    // v1.0.0-rc.1 conveniences: folded constants, `Self`, `..self`, and
    // tuple / nested `match` patterns, lowered to the same muxes as ever.
    Example {
        name: "fn_ergonomics",
        source: include_str!("../../../examples/fn_ergonomics.snt"),
    },
    // Hierarchy: three `#[module]` levels (one used twice), a block RAM inside
    // a module, struct and tuple ports. The flat simulator against the
    // hierarchical Verilog.
    Example {
        name: "fn_hier",
        source: include_str!("../../../examples/fn_hier.snt"),
    },
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

fn compile_one(source: &str) -> Module {
    compile_all(source).swap_remove(0)
}

/// The top module first, then any `#[module]` children.
fn compile_all(source: &str) -> Vec<Module> {
    let design = compile_str(source).expect("example should compile");
    assert!(!design.modules.is_empty(), "a module");
    design.modules
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

fn compile_project_all(root: &str) -> Vec<Module> {
    sinter_compiler::compile_project(std::path::Path::new(root))
        .expect("project should compile")
        .modules
}

/// One oracle case: the top module first, then its `#[module]` children.
struct Case {
    name: String,
    modules: Vec<Module>,
    cycles: usize,
}

impl Case {
    fn top(&self) -> &Module {
        &self.modules[0]
    }
}

/// Every oracle case — the single-file examples plus the multi-file projects.
fn all_cases() -> Vec<Case> {
    let mut cases: Vec<Case> = EXAMPLES
        .iter()
        .map(|ex| Case {
            name: ex.name.to_string(),
            modules: compile_all(ex.source),
            cycles: CYCLES,
        })
        .collect();
    cases.extend(PROJECTS.iter().map(|p| Case {
        name: p.name.to_string(),
        modules: compile_project_all(p.root),
        cycles: p.cycles,
    }));
    // FLT1b fault-latch examples, compiled in `--faults=on` mode.
    cases.extend(FAULT_EXAMPLES.iter().map(|ex| Case {
        name: ex.name.to_string(),
        modules: vec![compile_one_faults_on(ex.source)],
        cycles: CYCLES,
    }));
    cases
}

#[test]
fn examples_pass_the_oracle() {
    let tools = tools();
    for case in &all_cases() {
        let stimulus = gen_stimulus(case.top(), case.cycles, 0xDEAD_BEEF);
        co_simulate(
            &tools,
            &case.name,
            case.top(),
            &case.modules[1..],
            &stimulus,
            Lint::Strict,
        );
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
            &[],
            &stim.inputs,
            Lint::Strict,
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

/// Generated-simulator edge cases with no example of their own: a design with
/// no inputs, and inputs named after Rust keywords (raw identifiers).
const RUST_SIM_EXTRA: &[Example] = &[
    Example {
        name: "rsim_no_inputs",
        source: "struct Free { pub n: u4 = 3, }\n\
                 impl Free {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self) -> Free { Free { n: (self.n + 1) as u4 wrap } }\n\
                 }",
    },
    Example {
        name: "rsim_keywords",
        source: "struct Kw { pub acc: i12 = 0, }\n\
                 impl Kw {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self, loop: i12, move: u1) -> Kw {\n\
                 \x20       Kw { acc: if move == 1 { (self.acc - loop) as i12 wrap } else { self.acc } }\n\
                 \x20   }\n\
                 }",
    },
];

/// The generated Rust simulator (`--emit rust`, DESIGN §7.2) of every oracle
/// case matches the interpreter over the same stimulus. The interpreter is
/// itself diffed against Verilator above, so this ties the compiled model to
/// the Verilog too.
#[test]
fn examples_match_the_generated_rust_simulator() {
    let mut cases = all_cases();
    cases.extend(RUST_SIM_EXTRA.iter().map(|ex| Case {
        name: ex.name.to_string(),
        modules: vec![compile_one(ex.source)],
        cycles: CYCLES,
    }));
    let stimuli: Vec<Vec<Vec<u64>>> = cases
        .iter()
        .map(|c| gen_stimulus(c.top(), c.cycles, 0xDEAD_BEEF))
        .collect();
    let runs: Vec<(String, &Module, &[Vec<u64>])> = cases
        .iter()
        .zip(&stimuli)
        .map(|(c, stim)| (c.name.clone(), c.top(), stim.as_slice()))
        .collect();
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rust-sim");
    for ((name, module, stimulus), got) in runs.iter().zip(rust_sim_traces(&root, &runs)) {
        assert!(
            got == run_reference(module, stimulus),
            "{name}: the generated Rust simulator diverges from the interpreter"
        );
    }
}

/// Verilog backend bugs the fuzzer found (`tests/fuzz.rs`), reduced. Each
/// passes the oracle only with its fix. The lint is `Lint::Design`, since
/// these leave input bits unread.
const FUZZ_REGRESSIONS: &[Example] = &[
    // A size cast keeps its operand's signedness: `as u8 wrap` of a signed
    // value emitted `8'(signed)`, so `255 >= x` compared signed and failed.
    Example {
        name: "fuzz_sign_leak",
        source: "struct Top { pub f0: u1 = 1, }\n\
                 impl Top {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self, i0: u17) -> (Top, u1) {\n\
                 \x20       let a: u8 = ((((self.f0 as i8 sat) << 55) as i24 sat) >> 14) as u8 wrap;\n\
                 \x20       let b: u8 = ((-(i0 as i39 wrap)) * ((i0 as u16 wrap)[15:14] as i8 wrap)) as u8 wrap;\n\
                 \x20       (Top { f0: self.f0 }, ((((((self.f0 as i8 sat) << 55) as i24 sat) >> 14) as u8 wrap) >= (((-(i0 as i39 wrap)) * ((i0 as u16 wrap)[15:14] as i8 wrap)) as u8 wrap)) & (a >= b))\n\
                 \x20   }\n\
                 }",
    },
    // A widening cast sizes its operand's operators: `16'(a - b)` subtracted
    // at 16 bits, and `16'(~x)` set the high bits. (`~8'(x)` also made Yosys
    // read a cast of size `~8`.)
    Example {
        name: "fuzz_width_leak",
        source: "struct Top { pub f0: u1 = 0, }\n\
                 impl Top {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self, i0: u17, i1: u17) -> (Top, (u16, u16, i16)) {\n\
                 \x20       (Top { f0: self.f0 }, ((((i0 as u8 wrap) - (i1 as u8 wrap)) as u16 wrap), ((~(i0 as u8 wrap)) as u16 wrap), ((-(i0 as i8 wrap)) as i16 wrap)))\n\
                 \x20   }\n\
                 }",
    },
    // Verilator 5.045 sign-extends or arithmetic-shifts an inline zero-extended
    // variable from the variable's own top bit.
    Example {
        name: "fuzz_verilator_zext",
        source: "struct Top { pub f1: u15 = 0x4000, }\n\
                 impl Top {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self, i0: u9) -> (Top, (i22, i24)) {\n\
                 \x20       let l0: u9 = i0 | 0x100;\n\
                 \x20       (Top { f1: ((((l0[0:0] as i18 sat) + 0x7_i3) as i33 wrap) as u15 wrap) }, (((l0 as i22 wrap) >> (l0 as u7 wrap)), (((l0 as i18 wrap) & 0x3ffff_i18) as i24 wrap)))\n\
                 \x20   }\n\
                 }",
    },
    // A module with no ports besides `clk` and `rst_n`, and a memory written
    // every cycle, even from the reset state (the harness must not clock
    // during reset).
    Example {
        name: "fuzz_no_ports",
        source: "struct Top { f0: u17 = 0, m: mem<u32, 8> = 0, }\n\
                 impl Top {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self) -> Top {\n\
                 \x20       Top { f0: (self.f0 + 1) as u17 wrap, m: self.m.next(0, 1, self.f0[2:0], self.m.value()) }\n\
                 \x20   }\n\
                 }",
    },
    // Written to during reset, the memory's first read must still be its
    // initial contents.
    Example {
        name: "fuzz_mem_reset",
        source: "struct Top { pub rd: u24 = 0, m: mem<u24, 2> = 0, }\n\
                 impl Top {\n\
                 \x20   #[clock(domain = sys)]\n\
                 \x20   fn tick(self) -> Top {\n\
                 \x20       Top { rd: self.m.value(), m: self.m.next(0, 1, 0, 0x74422) }\n\
                 \x20   }\n\
                 }",
    },
];

#[test]
fn fuzz_regressions_pass_the_oracle() {
    let tools = tools();
    for ex in FUZZ_REGRESSIONS {
        let module = compile_one(ex.source);
        let stimulus = gen_stimulus(&module, CYCLES, 6);
        co_simulate(&tools, ex.name, &module, &[], &stimulus, Lint::Design);
    }
}
