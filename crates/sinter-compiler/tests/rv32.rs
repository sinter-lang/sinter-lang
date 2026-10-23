//! The RV32 example cores (`examples/rv32`, DESIGN §9.12) against a reference
//! RV32I instruction-set simulator.
//!
//! Each test program in `tests/rv32/programs/` (and the example's own
//! `prog.s`) is assembled, run on the reference ISS, and run on each Sinter
//! core's cycle-accurate simulator, with and without clock-enable stalls: the
//! fetch-ahead `mini.snt` and the 5-stage `pipe.snt`. The output-port writes,
//! final registers, final pc, and halt state must match. The sim-vs-Verilog
//! half of the check is the conformance oracle (`conformance.rs`, projects
//! `rv32_mini` and `rv32_pipe`).
//!
//! `SINTER_BLESS=1` rewrites `examples/rv32/prog.hex` from `prog.s`.

#[path = "rv32/asm.rs"]
mod asm;
#[path = "rv32/iss.rs"]
mod iss;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_project, Module};

use iss::{Iss, WORDS};

fn example_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/rv32")
}

fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rv32/programs")
}

fn assemble_file(path: &Path) -> Vec<u32> {
    let src = std::fs::read_to_string(path).expect("read program");
    asm::assemble(&src).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The two cores: the root file of each, in `examples/rv32/`.
const CORES: [&str; 2] = ["mini", "pipe"];

/// Compile `core` (`mini` or `pipe`) with `program` as its instruction ROM.
fn core_with(core: &str, name: &str, program: &[u32]) -> Module {
    let dir = std::env::temp_dir().join(format!("sinter-rv32-{core}-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    let root = format!("{core}.snt");
    for f in [root.as_str(), "alu.snt", "decode.snt"] {
        std::fs::copy(example_dir().join(f), dir.join(f)).expect("copy core source");
    }
    let hex = asm::to_hex(program, WORDS).expect("program fits");
    std::fs::write(dir.join("prog.hex"), hex).expect("write prog.hex");
    let design = compile_project(&dir.join(&root)).expect("core compiles");
    design.modules.into_iter().next().expect("one module")
}

/// The architectural result of running a program.
#[derive(Debug, PartialEq)]
struct Outcome {
    port: Vec<u32>,
    regs: Vec<u32>,
    pc: u32,
    halted: bool,
}

/// Run the core until it halts (or `limit` cycles), driving `run` from
/// `enable(cycle)`. Returns the outcome, how many asserts failed, and the
/// cycles taken.
fn run_core(m: &Module, limit: usize, enable: impl Fn(usize) -> bool) -> (Outcome, usize, usize) {
    let mut sim = Simulator::new(m);
    let mut port = Vec::new();
    let mut count = 0;
    let mut out = HashMap::new();
    let mut cycles = 0;
    for k in 0..limit {
        cycles = k + 1;
        let run = u64::from(enable(k));
        out = sim.step(&HashMap::from([("run".to_string(), run)]));
        if out["port_count_q"] != count {
            count = out["port_count_q"];
            port.push(out["port_q"] as u32);
        }
        if out["halted_q"] == 1 {
            break;
        }
    }
    // The register file is internal (not `pub`), so read it from the simulator:
    // the values held during the last cycle, as a `_q` output would show.
    let outcome = Outcome {
        port,
        regs: (0..32)
            .map(|i| sim.signal(&format!("regs_{i}")).unwrap_or(0) as u32)
            .collect(),
        pc: out["pc_q"] as u32,
        halted: out["halted_q"] == 1,
    };
    (outcome, sim.assert_failures().len(), cycles)
}

fn run_iss(program: &[u32]) -> (Outcome, usize) {
    let mut iss = Iss::new(program);
    let steps = iss.run(100_000);
    let outcome = Outcome {
        port: iss.port.clone(),
        regs: iss.regs.to_vec(),
        pc: iss.pc,
        halted: iss.halted,
    };
    (outcome, steps)
}

/// A deterministic pseudo-random clock enable, high about 3 cycles in 4.
fn stalls(k: usize) -> bool {
    let mut x = (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    x % 4 != 0
}

fn check_program(name: &str, program: &[u32]) {
    let (expected, steps) = run_iss(program);
    assert!(expected.halted, "`{name}` does not halt on the ISS");
    for core_name in CORES {
        let core = core_with(core_name, name, program);
        // At most ~2 cycles per instruction (a load, a taken branch, or a
        // load-use stall), plus pipeline fill: 3x is ample, and 6x with stalls.
        for (label, limit, enable) in [
            (
                "free-running",
                3 * steps + 8,
                (|_| true) as fn(usize) -> bool,
            ),
            ("with stalls", 6 * steps + 16, stalls as fn(usize) -> bool),
        ] {
            let (got, asserts, _) = run_core(&core, limit, enable);
            assert_eq!(
                got, expected,
                "`{name}` on `{core_name}` ({label}) diverges from the ISS"
            );
            assert_eq!(
                asserts, 0,
                "`{name}` on `{core_name}` ({label}) failed an assert"
            );
        }
    }
}

#[test]
fn assembler_matches_reference_encodings() {
    let cases = [
        ("addi x1, x0, 1", 0x0010_0093),
        ("add x3, x1, x2", 0x0020_81b3),
        ("sub x3, x1, x2", 0x4020_81b3),
        ("lw x5, 8(x2)", 0x0081_2283),
        ("sw x5, 8(x2)", 0x0051_2423),
        ("sw x5, -4(x2)", 0xfe51_2e23),
        ("beq x1, x2, 8", 0x0020_8463),
        ("bne x1, x2, -4", 0xfe20_9ee3),
        ("jal x1, 16", 0x0100_00ef),
        ("jal x0, -8", 0xff9f_f06f),
        ("jalr x1, 4(x2)", 0x0041_00e7),
        ("lui x5, 0x12345", 0x1234_52b7),
        ("auipc x5, 1", 0x0000_1297),
        ("srai x1, x2, 3", 0x4031_5093),
        ("slli x1, x2, 31", 0x01f1_1093),
        ("sltiu x1, x2, -1", 0xfff1_3093),
        ("ecall", 0x0000_0073),
        ("ebreak", 0x0010_0073),
    ];
    for (src, word) in cases {
        assert_eq!(asm::assemble(src), Ok(vec![word]), "`{src}`");
    }
    // `li` of a value whose bit 11 is set rounds the upper part up.
    assert_eq!(
        asm::assemble("li a0, 0xDEADBEEF"),
        Ok(vec![0xdead_c537, 0xeef5_0513])
    );
    assert!(asm::assemble("addi x1, x0, 2048")
        .unwrap_err()
        .contains("line 1"));
}

#[test]
fn iss_matches_hand_checked_results() {
    let program = asm::assemble(
        "li   t6, 0x80000000
         li   a0, 0xDEADBEEF
         sw   a0, 0(t6)
         li   a1, -8
         srai a2, a1, 1
         sw   a2, 0(t6)
         sltu a3, a1, a0
         sw   a3, 0(t6)
         sw   a0, 16(x0)
         lw   a4, 16(x0)
         sw   a4, 0(t6)
         jal  ra, next
next:    sw   ra, 0(t6)
         ecall",
    )
    .expect("assembles");
    let mut iss = Iss::new(&program);
    iss.run(100);
    assert!(iss.halted);
    // ra = address of `next`. The two `li`s of 32-bit values take two words
    // each, so `jal` is word 13 and `next` is word 14, at byte 56.
    assert_eq!(iss.port, [0xDEAD_BEEF, (-4i32) as u32, 0, 0xDEAD_BEEF, 56]);
}

#[test]
fn example_program_hex_is_current() {
    let words = assemble_file(&example_dir().join("prog.s"));
    let hex = asm::to_hex(&words, WORDS).expect("program fits");
    let path = example_dir().join("prog.hex");
    if std::env::var_os("SINTER_BLESS").is_some() {
        std::fs::write(&path, &hex).expect("write prog.hex");
    }
    let current = std::fs::read_to_string(&path).expect("read prog.hex");
    assert!(
        current == hex,
        "examples/rv32/prog.hex is stale; regenerate with SINTER_BLESS=1"
    );
}

#[test]
fn example_program_runs_on_the_core() {
    let words = assemble_file(&example_dir().join("prog.s"));
    check_program("example", &words);
    // The program prints 12 Fibonacci numbers, then their sum.
    let (outcome, _) = run_iss(&words);
    let fib = [0, 1, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89];
    let mut expected: Vec<u32> = fib.to_vec();
    expected.push(fib.iter().sum());
    assert_eq!(outcome.port, expected);
}

#[test]
fn test_programs_match_the_iss() {
    let mut paths: Vec<_> = std::fs::read_dir(programs_dir())
        .expect("read programs dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "s") && !p.ends_with("illegal.s"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 5,
        "expected the test programs, found {paths:?}"
    );
    for path in paths {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("program");
        check_program(name, &assemble_file(&path));
    }
}

#[test]
fn an_illegal_instruction_halts_and_fails_the_assert() {
    let program = assemble_file(&programs_dir().join("illegal.s"));
    let (expected, steps) = run_iss(&program);
    assert!(expected.halted);
    assert_eq!(expected.port, [7]);
    for core_name in CORES {
        let core = core_with(core_name, "illegal", &program);
        let (got, asserts, _) = run_core(&core, 3 * steps + 8, |_| true);
        assert_eq!(got, expected, "`{core_name}`");
        assert_eq!(
            asserts, 1,
            "`{core_name}`: the illegal-instruction assert fires once"
        );
    }
}

/// The cycle cost of each core on the example program, for the record: the
/// 5-stage pipeline trades a load-use stall and a one-cycle taken-branch
/// penalty for a much shorter critical path, so it needs more cycles than the
/// fetch-ahead core but can be clocked faster.
#[test]
fn cycle_counts_are_plausible() {
    let program = assemble_file(&example_dir().join("prog.s"));
    let (_, steps) = run_iss(&program);
    for core_name in CORES {
        let core = core_with(core_name, "cycles", &program);
        let (outcome, _, cycles) = run_core(&core, 4 * steps, |_| true);
        assert!(outcome.halted, "`{core_name}` halts");
        // At least one cycle per instruction, at most two plus fill.
        assert!(
            (steps..=2 * steps + 8).contains(&cycles),
            "`{core_name}`: {cycles} cycles for {steps} instructions"
        );
    }
}
