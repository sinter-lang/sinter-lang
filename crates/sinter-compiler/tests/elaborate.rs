//! F2 tests: elaboration of the functional surface to the IR (DESIGN §2.1).
//!
//! These check the elaborated [`ir::Module`] shape and run the Rust reference
//! simulator over it. The sim<->Verilog agreement is covered by the conformance
//! oracle (`conformance.rs`, which includes `fn_counter`).

use std::collections::HashMap;

use sinter_compiler::compile_str;
use sinter_compiler::sim::Simulator;

fn input(name: &str, v: u64) -> HashMap<String, u64> {
    [(name.to_string(), v)].into_iter().collect()
}

fn input2(a: (&str, u64), b: (&str, u64)) -> HashMap<String, u64> {
    [(a.0.to_string(), a.1), (b.0.to_string(), b.1)]
        .into_iter()
        .collect()
}

#[test]
fn fn_counter_elaborates_to_expected_module() {
    let src = include_str!("../../../examples/fn_counter.snt");
    let design = compile_str(src).expect("fn_counter should compile");

    assert_eq!(design.modules.len(), 1);
    let m = &design.modules[0];
    assert_eq!(m.name, "Counter");
    assert_eq!(m.clock_domain, "sys");

    assert_eq!(m.regs.len(), 1);
    assert_eq!(m.regs[0].name, "value");
    assert_eq!(m.regs[0].reset, 0);

    let inputs: Vec<&str> = m.inputs().map(|p| p.name.as_str()).collect();
    assert_eq!(inputs, ["en"]);
    // Every leaf register is observable (DESIGN §9.1 locked decision).
    let outputs: Vec<&str> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(outputs, ["value_q"]);
}

#[test]
fn fn_counter_simulates_with_enable_and_hold() {
    let src = include_str!("../../../examples/fn_counter.snt");
    let design = compile_str(src).expect("compile");
    let m = &design.modules[0];

    let mut sim = Simulator::new(m);
    // Mealy timing: the output is the pre-update (current) value.
    for expect in 0u64..5 {
        assert_eq!(sim.step(&input("en", 1))["value_q"], expect);
    }
    // With en low, the value holds at 5.
    assert_eq!(sim.step(&input("en", 0))["value_q"], 5);
    assert_eq!(sim.step(&input("en", 0))["value_q"], 5);
}

#[test]
fn fn_counter_wraps_at_the_declared_width() {
    let src = include_str!("../../../examples/fn_counter.snt");
    let design = compile_str(src).expect("compile");
    let m = &design.modules[0];

    let mut sim = Simulator::new(m);
    let mut outs = Vec::new();
    for _ in 0..258 {
        outs.push(sim.step(&input("en", 1))["value_q"]);
    }
    // Counts 0..=255 then wraps to 0 — the `as u8 wrap` cast end to end.
    assert_eq!(outs[255], 255);
    assert_eq!(outs[256], 0);
    assert_eq!(outs[257], 1);
}

#[test]
fn fn_counter_matches_legacy_counter_module() {
    // The functional counter must be bit-identical to the legacy `module`
    // counter under the same stimulus — same source, two surfaces.
    let fnc = compile_str(include_str!("../../../examples/fn_counter.snt")).expect("fn");
    let leg = compile_str(include_str!("../../../examples/counter.snt")).expect("legacy");
    let fm = &fnc.modules[0];
    let lm = &leg.modules[0];

    let mut fs = Simulator::new(fm);
    let mut ls = Simulator::new(lm);
    for en in [1u64, 1, 0, 1, 1, 1, 0, 0, 1] {
        let f = fs.step(&input("en", en))["value_q"];
        let l = ls.step(&input("en", en))["count"];
        assert_eq!(f, l, "divergence at en={en}");
    }
}

// --- F3: enums / match / sub-state methods / nested structs ----------------

#[test]
fn fn_updown_elaborates_with_nested_state() {
    let src = include_str!("../../../examples/fn_updown.snt");
    let design = compile_str(src).expect("fn_updown should compile");
    let m = &design.modules[0];
    assert_eq!(m.name, "Top");
    // The nested Digit flattens to one leaf register `d_value`.
    assert_eq!(m.regs.len(), 1);
    assert_eq!(m.regs[0].name, "d_value");
    let inputs: Vec<&str> = m.inputs().map(|p| p.name.as_str()).collect();
    assert_eq!(inputs, ["up", "down"]);
    assert!(m.outputs().any(|p| p.name == "d_value_q"));
}

#[test]
fn fn_updown_counts_and_wraps() {
    let design = compile_str(include_str!("../../../examples/fn_updown.snt")).expect("compile");
    let m = &design.modules[0];
    let mut sim = Simulator::new(m);
    let q = "d_value_q";

    // Up three times: Mealy output is the pre-update value.
    for expect in 0u64..3 {
        assert_eq!(sim.step(&input2(("up", 1), ("down", 0)))[q], expect);
    }
    // Now at 3. Down twice -> outputs 3, 2; state -> 1.
    assert_eq!(sim.step(&input2(("up", 0), ("down", 1)))[q], 3);
    assert_eq!(sim.step(&input2(("up", 0), ("down", 1)))[q], 2);
    // Hold.
    assert_eq!(sim.step(&input2(("up", 0), ("down", 0)))[q], 1);
    assert_eq!(sim.step(&input2(("up", 0), ("down", 0)))[q], 1);
    // `up` wins a tie: from 1 -> 2.
    assert_eq!(sim.step(&input2(("up", 1), ("down", 1)))[q], 1);
    assert_eq!(sim.step(&input2(("up", 0), ("down", 0)))[q], 2);
}

#[test]
fn fn_updown_wraps_below_zero() {
    let design = compile_str(include_str!("../../../examples/fn_updown.snt")).expect("compile");
    let m = &design.modules[0];
    let mut sim = Simulator::new(m);
    // At reset 0, a decrement wraps the u4 to 15.
    assert_eq!(sim.step(&input2(("up", 0), ("down", 1)))["d_value_q"], 0);
    assert_eq!(sim.step(&input2(("up", 0), ("down", 0)))["d_value_q"], 15);
}

#[test]
fn literal_match_free_fn_decodes() {
    // Exercises free-function inlining + a multi-arm literal `match` with a
    // wildcard, behaviorally (the Go Board's seven-seg decode stays quiescent
    // over the oracle's stimulus, so pin the decode values directly here).
    let src = "
        fn decode(d: u2) -> u4 {
            match d {
                0 => 1,
                1 => 2,
                2 => 4,
                _ => 8,
            }
        }
        struct S { code: u4 = 0 }
        impl S {
            #[clock(domain = sys)]
            fn tick(self, d: u2) -> S { S { code: decode(d) } }
        }
    ";
    let design = compile_str(src).expect("compile");
    let m = &design.modules[0];
    let mut sim = Simulator::new(m);
    // Mealy output is the registered (previous-cycle) decode.
    let ds = [0u64, 1, 2, 3, 0];
    let expect = [0u64, 1, 2, 4, 8];
    for (d, e) in ds.iter().zip(expect) {
        assert_eq!(sim.step(&input("d", *d))["code_q"], e);
    }
}

#[test]
fn fn_signed_arithmetic_and_narrowing_wrap() {
    let design = compile_str(include_str!("../../../examples/fn_signed.snt")).expect("compile");
    let m = &design.modules[0];
    assert!(m.regs[0].ty.is_signed());
    let mut sim = Simulator::new(m);
    // i4 signed add then narrowing wrap. Outputs are the registered values.
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 0);
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 7);
    // 7 + 7 = 14 -> i4 wrap = 0b1110 = 14 (i.e. -2). Demonstrates signed wrap.
    assert_eq!(sim.step(&input("x", 0))["acc_q"], 14);
}

#[test]
fn widening_wrap_cast_is_rejected() {
    // `wrap` must narrow (DESIGN §6.3); widening is F4. Caught at elaboration.
    let src = "struct S { w: u6 = 0 }
        impl S { #[clock(domain = sys)] fn tick(self, x: u4) -> S { S { w: x as u6 wrap } } }";
    let e = compile_str(src)
        .expect_err("widening cast should be rejected")
        .to_string();
    assert!(e.contains("widening"), "got: {e}");
}

#[test]
fn sat_cast_is_rejected() {
    // sat/trunc/round casts are F4; only `wrap` lowers today.
    let src = "struct S { v: u4 = 0 }
        impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> S { S { v: x as u4 sat } } }";
    let e = compile_str(src)
        .expect_err("sat cast should be rejected")
        .to_string();
    assert!(e.contains("sat/trunc/round"), "got: {e}");
}

#[test]
fn go_board_example_elaborates() {
    // The full Go Board (debounce, edge-detect, BCD up/down, 7-seg decode,
    // nested sub-state structs, enum/match, methods, arrays) lowers to an IR
    // module. Behavior is checked by the conformance oracle (sim<->Verilog).
    let design =
        compile_str(include_str!("../../../examples/go_board_counter.snt")).expect("go board");
    let m = &design.modules[0];
    assert_eq!(m.name, "Top");
    assert_eq!(m.regs.len(), 11);
    // Two button inputs, eleven observation outputs.
    assert_eq!(m.inputs().count(), 2);
    assert_eq!(m.outputs().count(), 11);
}
