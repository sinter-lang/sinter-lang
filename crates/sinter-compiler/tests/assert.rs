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

#[test]
fn backtrace_records_the_inlined_call_path() {
    // An `assert!` inside a method that is inlined into the `#[clock]` root must
    // record the full static backtrace (root -> frame), while a root-level assert
    // records just the root frame — the FLT1a "where did it come from" property.
    let src = "\
struct Inner {
    v: u8 = 0,
}
impl Inner {
    fn check(self, x: u8) -> u8 {
        assert!(x == 0, \"x should be zero\");
        x
    }
}
struct Outer {
    inner: Inner,
}
impl Outer {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> (Outer, u8) {
        let y: u8 = self.inner.check(x);
        assert!(self.inner.v == 0, \"top-level invariant\");
        (Outer { inner: self.inner }, y)
    }
}
";
    let design = compile_str(src).expect("compile");
    let asserts = &design.modules[0].asserts;
    let nested = asserts
        .iter()
        .find(|a| a.msg == "x should be zero")
        .expect("the nested assert");
    let root = asserts
        .iter()
        .find(|a| a.msg == "top-level invariant")
        .expect("the root assert");
    // The nested assert's backtrace is the inlined path `root -> Type::method@<call
    // line>`, ending in the assert's own line. The `@N` call-site line makes two
    // calls to the same method distinguishable.
    assert!(
        nested.loc.starts_with("Outer -> Inner::check@") && nested.loc.ends_with(')'),
        "nested backtrace should show the inline path with a call-site line, got: {}",
        nested.loc
    );
    assert!(
        nested.loc.contains(" (line "),
        "nested backtrace should end in the assert's own line, got: {}",
        nested.loc
    );
    // The root assert has only the root frame (no inlining) — the FLT0 shape.
    assert!(
        root.loc.starts_with("Outer (line "),
        "root backtrace should be just the root frame, got: {}",
        root.loc
    );
}

#[test]
fn two_call_sites_of_one_method_get_distinct_backtraces() {
    // The same method inlined at two call sites must yield two DISTINCT
    // backtraces (the `@<call-site line>` disambiguates them), so a failure report
    // points at the actual path — the call-stack property the feature is for.
    let src = "\
struct Inner {
    v: u8 = 0,
}
impl Inner {
    fn check(self, x: u8) -> u8 {
        assert!(x == 0, \"x should be zero\");
        x
    }
}
struct Outer {
    a: Inner,
    b: Inner,
}
impl Outer {
    #[clock(domain = sys)]
    fn tick(self, x: u8) -> (Outer, u9) {
        let p: u8 = self.a.check(x);
        let q: u8 = self.b.check(x);
        (Outer { a: self.a, b: self.b }, p + q)
    }
}
";
    let design = compile_str(src).expect("compile");
    let locs: Vec<String> = design.modules[0]
        .asserts
        .iter()
        .map(|a| a.loc.clone())
        .collect();
    assert_eq!(locs.len(), 2, "one assert per inlined call site: {locs:?}");
    assert_ne!(
        locs[0], locs[1],
        "two call sites of the same method must have distinct backtraces: {locs:?}"
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
