//! FLT0 sim-only `assert!` tests (DESIGN §6.12 / §9.9): the assert is a
//! diagnostic check the reference simulator evaluates each cycle, recorded as an
//! [`AssertFailure`] when it reads false, that emits **zero** bits in either
//! backend (so the SystemVerilog is byte-identical with and without it). The
//! sim↔Verilog oracle is in `conformance.rs` (the `fn_assert` example — an
//! invariant that always holds).

use std::collections::HashMap;

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_str, verilog};

/// Step a single-`inc`-input design, returning nothing (we inspect the sim's
/// recorded failures separately).
fn step(sim: &mut Simulator, inc: u64) {
    sim.step(
        &[("inc".to_string(), inc)]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    );
}

#[test]
fn a_holding_assert_records_no_failure() {
    let design = compile_str(include_str!("../../../examples/fn_assert.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // Drive the counter up to and past the cap; the clamp invariant holds, so no
    // assertion ever fires.
    for _ in 0..300 {
        step(&mut sim, 1);
    }
    assert!(
        sim.assert_failures().is_empty(),
        "a valid saturating counter must not trip its invariant: {:?}",
        sim.assert_failures()
    );
}

#[test]
fn a_violated_invariant_records_a_failure() {
    // A design whose `assert!` is false from the first cycle: the register resets
    // to 5, and the assertion claims it is 0. The sim must record the failure with
    // its message, every cycle it is false.
    let src = "\
struct Bad {
    v: u8 = 5,
}
impl Bad {
    #[clock(domain = sys)]
    fn tick(self) -> (Bad, u8) {
        assert!(self.v == 0, \"v was supposed to be zero\");
        (Bad { v: self.v }, self.v)
    }
}
";
    let design = compile_str(src).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    for _ in 0..3 {
        sim.step(&HashMap::new());
    }
    let fails = sim.assert_failures();
    assert_eq!(
        fails.len(),
        3,
        "one failure per cycle it was false: {fails:?}"
    );
    assert_eq!(fails[0].cycle, 0);
    assert_eq!(fails[0].msg, "v was supposed to be zero");
    assert_eq!(fails[2].cycle, 2);
}

#[test]
fn assert_emits_zero_bits_sv_is_byte_identical() {
    // The same design with and without the `assert!` line must lower to identical
    // SystemVerilog — the compile-out guarantee (DESIGN §6.12). The condition
    // references a live signal (`v`, the output), so removing the assert cannot
    // change dead-code elimination.
    let with = "\
struct M {
    v: u8 = 0,
}
impl M {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> (M, u8) {
        assert!(self.v <= 255, \"u8 is always <= 255\");
        (M { v: x }, self.v)
    }
}
";
    let without = "\
struct M {
    v: u8 = 0,
}
impl M {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> (M, u8) {
        (M { v: x }, self.v)
    }
}
";
    let sv_with = verilog::emit_module(&compile_str(with).expect("compile").modules[0]);
    let sv_without = verilog::emit_module(&compile_str(without).expect("compile").modules[0]);
    assert_eq!(
        sv_with, sv_without,
        "an `assert!` must emit zero bits: the SystemVerilog must be byte-identical"
    );
}

/// Shared: compile `src`, expect a semantic error containing `needle`.
fn compile_err(src: &str, needle: &str) {
    match compile_str(src) {
        Ok(_) => panic!("expected an error containing {needle:?}, but it compiled"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(needle),
                "error did not contain {needle:?}.\n  got: {msg}"
            );
        }
    }
}

#[test]
fn assert_condition_must_be_one_bit() {
    compile_err(
        "struct M { v: u8 = 0 }\n\
         impl M { #[clock(domain = sys)] fn tick(self) -> (M, u8) \
         { assert!(self.v, \"not a bool\"); (M { v: self.v }, self.v) } }",
        "expected `u1`",
    );
}

#[test]
fn assert_is_rejected_inside_a_branch() {
    // An `assert!` in an `if` arm is only conditionally reached, but the sim
    // evaluates the flat list unconditionally — so it is rejected (fold the branch
    // condition into the predicate instead).
    compile_err(
        "struct M { v: u8 = 0 }\n\
         impl M { #[clock(domain = sys)] fn tick(self, c: u1) -> (M, u8) {\n\
             let n: u8 = if c == 1 { assert!(self.v == 0, \"x\"); 1 } else { 0 };\n\
             (M { v: n }, self.v) } }",
        "if`/`match` arm",
    );
}

#[test]
fn assert_is_rejected_in_a_pipeline_fn() {
    compile_err(
        "pipeline fn p(a: u8) -> u8 {\n\
             let x: u8 = a;\n\
             yield;\n\
             assert!(x == x, \"nope\");\n\
             x }",
        "assert!",
    );
}

#[test]
fn assert_is_rejected_in_a_multiplex_fn() {
    compile_err(
        "multiplex fn m(a: u8) -> u8 {\n\
             let x: u8 = a;\n\
             yield;\n\
             assert!(x == x, \"nope\");\n\
             x }",
        "assert!",
    );
}
