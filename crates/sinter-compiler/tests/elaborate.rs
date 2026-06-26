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
