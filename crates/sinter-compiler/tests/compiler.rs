//! Unit/behavioral tests for `sinter-compiler`.

use std::collections::HashMap;

use sinter_compiler::sim::{gen_stimulus, run_reference, Simulator};
use sinter_compiler::{compile_str, verilog};

const COUNTER: &str = include_str!("../../../examples/counter.snt");
const ACCUMULATOR: &str = include_str!("../../../examples/accumulator.snt");

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
        assert_eq!(out["count"], expected, "cycle {expected}");
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
    assert_eq!(held["count"], 3);
    let still = sim.step(&HashMap::from([("en".to_string(), 0u64)]));
    assert_eq!(still["count"], 3);
}

#[test]
fn counter_wraps_at_eight_bits() {
    let m = only_module(COUNTER);
    let mut sim = Simulator::new(&m);
    let mut last = 0;
    for _ in 0..260 {
        last = sim.step(&HashMap::from([("en".to_string(), 1u64)]))["count"];
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
        ]))["sum"];
    }
    // Sampled on the 4th cycle: acc has been decremented three times → -3.
    assert_eq!(out as u16 as i16, -3);
}

#[test]
fn verilog_emission_is_well_formed() {
    let m = only_module(COUNTER);
    let sv = verilog::emit_module(&m);
    assert!(sv.starts_with("`default_nettype none"));
    assert!(sv.contains("module counter ("));
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

// --- Error paths -----------------------------------------------------------

#[test]
fn rejects_double_driven_register() {
    let src = "module m { in a: u1; out o: u1; reg r: u1 = 0; r <= a; r <= a; o = r; }";
    assert!(compile_str(src).is_err());
}

#[test]
fn rejects_undriven_register() {
    let src = "module m { in a: u1; out o: u1; reg r: u1 = 0; o = r; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("no next-state"), "got: {err}");
}

#[test]
fn rejects_undriven_output() {
    let src = "module m { in a: u1; out o: u1; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("never driven"), "got: {err}");
}

#[test]
fn rejects_combinational_cycle() {
    // Wires are declare-before-use, so a mutual reference (which would form a
    // combinational cycle) is unrepresentable: `p` cannot name `q` yet.
    let src = "module m { in a: u4; out o: u4; let p: u4 = q + a; let q: u4 = p + a; o = q; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("unknown name `q`"), "got: {err}");
}

#[test]
fn rejects_self_referential_wire() {
    let src = "module m { in a: u4; out o: u4; let p: u4 = p + a; o = p; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("unknown name `p`"), "got: {err}");
}

#[test]
fn forward_wire_chain_evaluates_in_order() {
    // Declare-before-use still allows ordinary chains.
    let src = "module m { in a: u8; out o: u8; let p: u8 = a + 1; let q: u8 = p + 1; o = q; }";
    let design = compile_str(src).expect("should compile");
    let module = &design.modules[0];
    let mut sim = Simulator::new(module);
    let out = sim.step(&HashMap::from([("a".to_string(), 10u64)]));
    assert_eq!(out["o"], 12);
}

#[test]
fn rejects_narrowing_literal() {
    let src = "module m { in a: u1; out o: u4; o = 99; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("does not fit"), "got: {err}");
}

#[test]
fn rejects_type_mismatch() {
    let src = "module m { in a: u8; in b: u4; out o: u8; o = a + b; }";
    let err = compile_str(src).unwrap_err().to_string();
    assert!(err.contains("type mismatch"), "got: {err}");
}

#[test]
fn rejects_assigning_input() {
    let src = "module m { in a: u8; out o: u8; a = o; o = a; }";
    assert!(compile_str(src).is_err());
}
