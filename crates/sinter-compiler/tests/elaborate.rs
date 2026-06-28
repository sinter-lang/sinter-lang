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

fn input4(a: (&str, u64), b: (&str, u64), c: (&str, u64), d: (&str, u64)) -> HashMap<String, u64> {
    [
        (a.0.to_string(), a.1),
        (b.0.to_string(), b.1),
        (c.0.to_string(), c.1),
        (d.0.to_string(), d.1),
    ]
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

// --- F4: fixed-point arithmetic and the full cast-mode set -----------------

#[test]
fn fn_fixed_multiplies_q_format() {
    let design = compile_str(include_str!("../../../examples/fn_fixed.snt")).expect("compile");
    let m = &design.modules[0];
    assert_eq!(m.regs[0].name, "p");
    // i8.8 erases to a 16-bit integer in the IR.
    assert_eq!(m.regs[0].ty.width(), 16);
    let mut sim = Simulator::new(m);
    // 2.0 (Q4.4 = 32) * 3.0 (Q4.4 = 48) = 6.0 (Q8.8 = 1536).
    sim.step(&input2(("a", 32), ("b", 48)));
    assert_eq!(sim.step(&input2(("a", 0), ("b", 0)))["p_q"], 1536);
    // -2.0 (Q4.4 bits 224) * 3.0 = -6.0 (Q8.8 = 65536 - 1536 = 64000).
    sim.step(&input2(("a", 224), ("b", 48)));
    assert_eq!(sim.step(&input2(("a", 0), ("b", 0)))["p_q"], 64000);
}

#[test]
fn fn_sat_saturates() {
    let design = compile_str(include_str!("../../../examples/fn_sat.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // +7 each cycle saturates at 7 (i4 max) instead of wrapping to -2.
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 0);
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 7);
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 7);
    assert_eq!(sim.step(&input("x", 7))["acc_q"], 7);
}

#[test]
fn fn_round_rounds_half_up() {
    let design = compile_str(include_str!("../../../examples/fn_round.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // Q4.8 -> Q4.4 rounds: (x + 8) >> 4. x=24 -> 2 (truncation would give 1).
    sim.step(&input("x", 24));
    assert_eq!(sim.step(&input("x", 0))["y_q"], 2);
    // x=8 -> (8+8)>>4 = 1 (truncation would give 0).
    sim.step(&input("x", 8));
    assert_eq!(sim.step(&input("x", 0))["y_q"], 1);
}

#[test]
fn widening_cast_sign_extends() {
    // An i4 -> i8 widening `wrap` cast sign-extends (value-preserving).
    let src = "struct S { wide: i8 = 0 }
        impl S { #[clock(domain = sys)] fn tick(self, x: i4) -> S { S { wide: x as i8 wrap } } }";
    let design = compile_str(src).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // x = -3 (i4 bits 1101 = 13) -> wide = -3 (i8 bits 1111_1101 = 253).
    sim.step(&input("x", 13));
    assert_eq!(sim.step(&input("x", 0))["wide_q"], 253);
}

// --- PL3a: pipeline-function elaboration (DESIGN §3.1 / §9.3) ---------------

#[test]
fn pipeline_elaborates_to_stage_registers() {
    // The `pipeline fn mac` desugars to a `mac__pipe` instance owned by `Top`,
    // flattened to leaf registers: a valid bit per stage (v0, v1) and the values
    // carried across each `yield` (c and prod across boundary 0, sum across
    // boundary 1) — all reset to a defined 0 (oracle-safe).
    let design =
        compile_str(include_str!("../../../examples/fn_mac_pipeline.snt")).expect("compile");
    assert_eq!(design.modules.len(), 1, "exactly one elaborated module");
    let m = &design.modules[0];
    assert_eq!(m.name, "Top");

    let regs: HashMap<&str, &sinter_compiler::ir::Reg> =
        m.regs.iter().map(|r| (r.name.as_str(), r)).collect();
    let expect: &[(&str, u32)] = &[
        ("inst_v0", 1),
        ("inst_v1", 1),
        ("inst_c_s0", 16),
        ("inst_prod_s0", 16),
        ("inst_sum_s1", 16),
    ];
    assert_eq!(
        m.regs.len(),
        expect.len(),
        "no extra/missing stage registers"
    );
    for (name, width) in expect {
        let r = regs
            .get(name)
            .unwrap_or_else(|| panic!("missing register `{name}`"));
        assert_eq!(r.ty.width(), *width, "width of `{name}`");
        assert_eq!(r.reset, 0, "reset of `{name}` must be a defined 0");
    }

    // Inputs are the pipeline operands plus the input-valid bit; outputs are
    // every `_q` then the result and its valid bit.
    let inputs: Vec<&str> = m.inputs().map(|p| p.name.as_str()).collect();
    assert_eq!(inputs, ["a", "b", "c", "valid"]);
    let outputs: Vec<&str> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(outputs.last(), Some(&"out_1"), "valid is the last output");
    assert!(outputs.contains(&"out_0"), "result output present");

    // PL3b: v0's next-state is the `in_valid` input (here the parent's `valid`
    // port), not a baked literal — so a bubble (`valid = 0`) enters the chain.
    let v0_next = m
        .reg_updates
        .iter()
        .find(|(n, _)| n == "inst_v0")
        .map(|(_, e)| &e.kind)
        .expect("inst_v0 has a next-state");
    assert!(
        matches!(v0_next, sinter_compiler::ir::ExprKind::Ident(n) if n == "valid"),
        "PL3b: inst_v0 <= valid input, got {v0_next:?}"
    );
}

#[test]
fn pipeline_is_none_during_fill_then_emits_aligned_results() {
    // With `valid = 1` every cycle (free-running), out_1 is 0 for the first
    // `latency` (2) cycles — "None during fill" — then 1, and each result combines
    // operands from the SAME wavefront (latency cycles earlier), proving the
    // carried registers keep the datum coherent.
    let design =
        compile_str(include_str!("../../../examples/fn_mac_pipeline.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    let d0 = sim.step(&input4(("a", 3), ("b", 4), ("c", 5), ("valid", 1)));
    assert_eq!(d0["out_1"], 0, "cycle 0: filling -> invalid");
    let d1 = sim.step(&input4(("a", 6), ("b", 7), ("c", 8), ("valid", 1)));
    assert_eq!(d1["out_1"], 0, "cycle 1: filling -> invalid");

    let d2 = sim.step(&input4(("a", 0), ("b", 0), ("c", 0), ("valid", 1)));
    assert_eq!(d2["out_1"], 1, "cycle 2: first result valid");
    assert_eq!(d2["out_0"], 3 * 4 + 5, "result is (a0*b0)+c0 = 17");

    let d3 = sim.step(&input4(("a", 0), ("b", 0), ("c", 0), ("valid", 1)));
    assert_eq!(d3["out_1"], 1, "cycle 3: second result valid");
    assert_eq!(d3["out_0"], 6 * 7 + 8, "result is (a1*b1)+c1 = 50");
}

#[test]
fn pipeline_input_bubble_becomes_output_bubble_at_latency() {
    // PL3b: a `valid = 0` cycle injects a bubble that surfaces as `out_1 = 0`
    // exactly `latency` (2) cycles later, while the valid data on either side
    // still emerges at the right time and value.
    let design =
        compile_str(include_str!("../../../examples/fn_mac_pipeline.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // cycle 0: valid datum A = (2,3,1) -> (2*3)+1 = 7
    sim.step(&input4(("a", 2), ("b", 3), ("c", 1), ("valid", 1)));
    // cycle 1: BUBBLE (valid = 0); the payload values are don't-care
    sim.step(&input4(("a", 9), ("b", 9), ("c", 9), ("valid", 0)));
    // cycle 2: valid datum B = (4,5,2) -> (4*5)+2 = 22
    let c2 = sim.step(&input4(("a", 4), ("b", 5), ("c", 2), ("valid", 1)));
    assert_eq!(c2["out_1"], 1, "cycle 2: datum A emerges (latency 2)");
    assert_eq!(c2["out_0"], 2 * 3 + 1, "A = 7");
    // cycle 3: the bubble from cycle 1 reaches the output -> invalid.
    let c3 = sim.step(&input4(("a", 0), ("b", 0), ("c", 0), ("valid", 0)));
    assert_eq!(c3["out_1"], 0, "cycle 3: the input bubble surfaces");
    // cycle 4: datum B emerges, valid again.
    let c4 = sim.step(&input4(("a", 0), ("b", 0), ("c", 0), ("valid", 0)));
    assert_eq!(c4["out_1"], 1, "cycle 4: datum B emerges");
    assert_eq!(c4["out_0"], 4 * 5 + 2, "B = 22");
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

// --- O1: Option<T> elaboration (DESIGN §6.5 / §9.4) ------------------------

#[test]
fn fn_option_elaborates_to_validity_and_payload_regs() {
    // An `Option<u9>` register erases to two leaves — `last_is_some` (u1) and
    // `last_payload` (u9) — both resetting to a defined 0 (i.e. `None`).
    let design = compile_str(include_str!("../../../examples/fn_option.snt")).expect("compile");
    let m = &design.modules[0];
    let regs: HashMap<&str, &sinter_compiler::ir::Reg> =
        m.regs.iter().map(|r| (r.name.as_str(), r)).collect();
    assert_eq!(m.regs.len(), 2, "Option<u9> flattens to is_some + payload");
    let is_some = regs.get("last_is_some").expect("validity register");
    assert_eq!(is_some.ty.width(), 1);
    assert_eq!(is_some.reset, 0, "reset is `None`");
    let payload = regs.get("last_payload").expect("payload register");
    assert_eq!(payload.ty.width(), 9);
    assert_eq!(payload.reset, 0, "None payload resets to a defined 0");
    let outs: Vec<&str> = m.outputs().map(|p| p.name.as_str()).collect();
    for o in ["last_is_some_q", "last_payload_q", "out"] {
        assert!(outs.contains(&o), "expected output `{o}` in {outs:?}");
    }
}

#[test]
fn option_leaf_colliding_with_a_sibling_input_field_is_rejected() {
    // An `Option<u8>` field flattens to `_is_some`/`_payload` leaves; a sibling
    // field whose name matches one of them would emit a duplicate Verilog port
    // (which Verilator rejects), so the elaborator rejects it up front.
    let src = "
        struct In { a: Option<u8>, a_is_some: u1 }
        struct Top { x: u8 = 0 }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, i: In) -> Top { Top { x: 0 } }
        }
    ";
    match compile_str(src) {
        Ok(_) => panic!("expected a port-collision rejection"),
        Err(e) => assert!(e.to_string().contains("collides"), "unexpected error: {e}"),
    }
}

#[test]
fn fn_option_some_none_and_match_simulate() {
    // classify(x) = Some(x+x) for x<128 else None; `out` decodes the PREVIOUS
    // cycle's stored Option via `match` (Some -> value, None -> 0).
    let design = compile_str(include_str!("../../../examples/fn_option.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    let d0 = sim.step(&input("x", 10)); // store Some(20); last was None
    assert_eq!(d0["out"], 0);
    assert_eq!(d0["last_is_some_q"], 0);
    let d1 = sim.step(&input("x", 200)); // store None; last was Some(20)
    assert_eq!(d1["out"], 20);
    assert_eq!(d1["last_is_some_q"], 1);
    let d2 = sim.step(&input("x", 5)); // store Some(10); last was None
    assert_eq!(d2["out"], 0);
    assert_eq!(d2["last_is_some_q"], 0);
    let d3 = sim.step(&input("x", 0)); // last was Some(10)
    assert_eq!(d3["out"], 10);
    assert_eq!(d3["last_is_some_q"], 1);
}
