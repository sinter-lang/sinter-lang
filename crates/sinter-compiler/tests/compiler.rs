//! Unit/behavioral tests for `sinter-compiler`.

use std::collections::HashMap;

use sinter_compiler::sim::{gen_stimulus, run_reference, Simulator};
use sinter_compiler::{compile_str, verilog};

const COUNTER: &str = include_str!("../../../examples/fn_counter.snt");
const ACCUMULATOR: &str = include_str!("../../../examples/fn_accumulator.snt");

fn only_module(src: &str) -> sinter_compiler::Module {
    let design = compile_str(src).expect("should compile");
    design.modules.into_iter().next().expect("one module")
}

#[test]
fn counter_counts_when_enabled() {
    let m = only_module(COUNTER);
    let mut sim = Simulator::new(&m);
    for expected in 0u64..10 {
        let inputs = HashMap::from([("en".to_string(), 1u64)]);
        let out = sim.step(&inputs);
        assert_eq!(out["value_q"], expected, "cycle {expected}");
    }
}

#[test]
fn counter_holds_when_disabled() {
    let m = only_module(COUNTER);
    let mut sim = Simulator::new(&m);
    // Enable for three cycles to reach value 3, then hold.
    for _ in 0..3 {
        sim.step(&HashMap::from([("en".to_string(), 1u64)]));
    }
    let held = sim.step(&HashMap::from([("en".to_string(), 0u64)]));
    assert_eq!(held["value_q"], 3);
    let still = sim.step(&HashMap::from([("en".to_string(), 0u64)]));
    assert_eq!(still["value_q"], 3);
}

#[test]
fn counter_wraps_at_eight_bits() {
    let m = only_module(COUNTER);
    let mut sim = Simulator::new(&m);
    let mut last = 0;
    for _ in 0..260 {
        last = sim.step(&HashMap::from([("en".to_string(), 1u64)]))["value_q"];
    }
    // 260 enabled cycles: the sampled value is (260 - 1) mod 256 = 3.
    assert_eq!(last, (260 - 1) % 256);
}

#[test]
fn accumulator_adds_signed_input() {
    let m = only_module(ACCUMULATOR);
    let mut sim = Simulator::new(&m);
    // Add -1 (0xFFFF in 16-bit two's complement) three times with valid high.
    let neg_one = 0xFFFFu64;
    let mut out = 0;
    for _ in 0..4 {
        out = sim.step(&HashMap::from([
            ("valid".to_string(), 1u64),
            ("x".to_string(), neg_one),
        ]))["sum_q"];
    }
    // Sampled on the 4th cycle: acc has been decremented three times → -3.
    assert_eq!(out as u16 as i16, -3);
}

#[test]
fn verilog_emission_is_well_formed() {
    let m = only_module(COUNTER);
    let sv = verilog::emit_module(&m);
    assert!(sv.starts_with("`default_nettype none"));
    assert!(sv.contains("module Counter ("));
    assert!(sv.contains("input  wire         clk,"));
    assert!(sv.contains("input  wire         rst_n,"));
    assert!(sv.contains("always_ff @(posedge clk or negedge rst_n)"));
    assert!(sv.contains("always_comb"));
    assert!(sv.contains("value <= 8'h0;"));
    // Combinational next-state must live outside always_ff.
    assert!(sv.contains("value_next ="));
    assert!(sv.contains("value <= value_next;"));
    assert!(sv.trim_end().ends_with("`default_nettype wire"));
}

#[test]
fn signed_nets_are_declared_signed() {
    let m = only_module(ACCUMULATOR);
    let sv = verilog::emit_module(&m);
    assert!(sv.contains("signed [15:0]"));
}

#[test]
fn reference_trace_has_expected_length() {
    let m = only_module(COUNTER);
    let stim = gen_stimulus(&m, 32, 0xC0FF_EE00);
    assert_eq!(stim.len(), 32);
    let trace = run_reference(&m, &stim);
    // One u64 (8 bytes) per output per cycle; counter has a single output.
    assert_eq!(trace.len(), 32 * 8);
}
