//! COSIM1 (DESIGN §9.15): the library's co-simulation runner, shared by the
//! conformance oracle and `sinter sim --cosim`.

#[path = "support/oracle.rs"]
mod oracle;

use sinter_compiler::compile_str;
use sinter_compiler::cosim::{self, Mismatch, VERILATOR_FLAGS};
use sinter_compiler::sim::{gen_stimulus, run_reference};

const COUNTER: &str = include_str!("../../../examples/fn_counter.snt");

fn words(values: &[u64]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `verilator.f` mirrors the downstream config; the library's flags must be
/// the same list.
#[test]
fn the_flags_match_verilator_f() {
    let text = std::fs::read_to_string(oracle::VERILATOR_FLAGS_FILE).expect("read verilator.f");
    let file: Vec<&str> = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .flat_map(str::split_whitespace)
        .collect();
    assert_eq!(file, VERILATOR_FLAGS);
}

#[test]
fn first_mismatch_names_the_cycle_and_port() {
    let m = compile_str(COUNTER).expect("compile").modules.remove(0);
    let outputs: Vec<String> = m.outputs().map(|p| p.name.clone()).collect();
    let n = outputs.len();
    let reference: Vec<u64> = (0..3 * n as u64).collect();
    assert_eq!(
        cosim::first_mismatch(&m, &words(&reference), &words(&reference)),
        None
    );
    let mut dut = reference.clone();
    dut[n + n - 1] = 99;
    assert_eq!(
        cosim::first_mismatch(&m, &words(&reference), &words(&dut)),
        Some(Mismatch {
            cycle: 1,
            port: outputs[n - 1].clone(),
            expected: (2 * n - 1) as u64,
            actual: 99,
        })
    );
    // A trace that stops early mismatches where it stops.
    let short = words(&reference[..n]);
    let m1 = cosim::first_mismatch(&m, &words(&reference), &short).expect("short trace");
    assert_eq!((m1.cycle, m1.actual), (1, 0));
}

#[test]
fn run_matches_the_simulator() {
    if !oracle::tools().verilator {
        return;
    }
    let m = compile_str(COUNTER).expect("compile").modules.remove(0);
    let stimulus = gen_stimulus(&m, 40, 3);
    let dir = oracle::workdir("cosim-lib");
    let dut = cosim::run(&dir, &m, &[], &stimulus, &[]).expect("co-simulate");
    assert_eq!(
        cosim::first_mismatch(&m, &run_reference(&m, &stimulus), &dut),
        None
    );
}
