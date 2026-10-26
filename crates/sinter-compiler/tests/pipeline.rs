//! PL3a desugaring tests (DESIGN §3.1 / §9.3): the full `compile_str` path that
//! turns a `pipeline fn` into a synthesized instance struct + `advance` method.
//!
//! Elaborated-shape and fill-region behaviour live in `elaborate.rs`; the
//! sim<->Verilog oracle is in `conformance.rs` (`fn_mac_pipeline`). These cover
//! the desugar pass's own checks and a chaining case the headline example does
//! not exercise.

use std::collections::HashMap;

use sinter_compiler::compile_str;
use sinter_compiler::sim::Simulator;

/// Assert `compile_str(src)` fails with a message containing `needle`.
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
fn calling_a_pipeline_fn_directly_is_rejected() {
    // The full pipeline (desugar + check) still rejects a direct call: a pipeline
    // is instantiated and advanced, never called inline.
    compile_err(
        "pipeline fn mac(a: u8, b: u8) -> u16 { let p = (a * b) as u16 wrap; yield; p } \
         fn use_it(a: u8, b: u8) -> u16 { mac(a, b) }",
        "cannot be called directly",
    );
}

#[test]
fn carried_value_without_a_cast_is_rejected() {
    // `p` is carried across the `yield` but its `let` has no explicit width cast,
    // so PL3a cannot pin its register type — rejected with guidance.
    compile_err(
        "pipeline fn f(a: u8, b: u8) -> u16 { let p = a * b; yield; p }",
        "needs an explicit width-fixing cast",
    );
}

#[test]
fn non_scalar_carried_value_is_rejected() {
    // A value carried across a `yield` must be a scalar — an aggregate (here the
    // array param `a`, read in both stages) has no `0` reset literal.
    compile_err(
        "pipeline fn f(a: [u8; 2]) -> u8 { let p = (a[0]) as u8 wrap; yield; (p + a[1]) as u8 wrap }",
        "must be a scalar",
    );
}

#[test]
fn nested_yield_is_rejected() {
    // `yield` may only appear at the top level of a `pipeline fn`.
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let x = if a == 0 { yield; a } else { a }; yield; x }",
        "only appear at the top level",
    );
}

#[test]
fn yield_tap_is_rejected() {
    // Mid-pipeline `yield <expr>` taps are not part of the model (final output only).
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let b = (a) as u8 wrap; yield b; b }",
        "final output only",
    );
}

#[test]
fn synthesized_struct_name_collision_is_rejected() {
    // The synthesized `<name>__pipe` must not clash with a user-declared type.
    compile_err(
        "struct mac__pipe { x: u8 = 0 } \
         pipeline fn mac(a: u8, b: u8) -> u16 { let p = (a * b) as u16 wrap; yield; p }",
        "collides with an existing type",
    );
}

#[test]
fn value_carried_across_multiple_boundaries_chains() {
    // A pure delay line: `r` is computed in stage 0 and read only in the tail, so
    // it is registered across BOTH `yield` boundaries (r_s0 -> r_s1). The output
    // is the input delayed by the latency (2), proving the stage-to-stage chain.
    let src = "
        pipeline fn delay(x: u8) -> u8 {
            let r = (x) as u8 wrap;
            yield;
            yield;
            r
        }
        struct Top { inst: delay }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, x: u8) -> (Top, (u8, u1)) {
                let (i2, result, valid) = self.inst.advance(x, 1);
                (Top { inst: i2 }, (result, valid))
            }
        }
    ";
    let design = compile_str(src).expect("delay pipeline should compile");
    let m = &design.modules[0];
    // Two valid bits + the value carried across both boundaries.
    let names: Vec<&str> = m.regs.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"inst_r_s0"), "stage-0 register: {names:?}");
    assert!(names.contains(&"inst_r_s1"), "stage-1 register: {names:?}");

    let mut sim = Simulator::new(m);
    let step = |sim: &mut Simulator, x: u64| -> HashMap<String, u64> {
        sim.step(&[("x".to_string(), x)].into_iter().collect())
    };
    assert_eq!(step(&mut sim, 5)["out_1"], 0, "cycle 0: filling");
    assert_eq!(step(&mut sim, 9)["out_1"], 0, "cycle 1: filling");
    let d2 = step(&mut sim, 0);
    assert_eq!(d2["out_1"], 1);
    assert_eq!(d2["out_0"], 5, "input from cycle 0, delayed by latency 2");
    let d3 = step(&mut sim, 0);
    assert_eq!(d3["out_0"], 9, "input from cycle 1");
}

// --- review fixes: nested-binder shadowing, annotation widths, diagnostics ---

#[test]
fn nested_block_let_shadowing_a_pipeline_value_is_rejected() {
    // A block-local `let` that reuses a pipeline value's name would be silently
    // hijacked into a stage-register read; PL3a rejects the shadow.
    compile_err(
        "pipeline fn f(a: u8) -> u8 { \
            let p = (a) as u8 wrap; yield; \
            { let p = (a + a) as u8 wrap; (p + p) as u8 wrap } }",
        "cannot shadow",
    );
}

#[test]
fn match_arm_binding_shadowing_a_pipeline_value_is_rejected() {
    compile_err(
        "pipeline fn f(a: u8) -> u8 { \
            let p = (a) as u8 wrap; let q = (a) as u8 wrap; yield; \
            match q { p => p } }",
        "cannot shadow",
    );
}

#[test]
fn non_shadowing_match_binding_in_a_stage_compiles() {
    // A fresh match-arm binding (no collision) is fine — the rejection is precise.
    let src = "
        pipeline fn f(a: u8) -> u8 {
            let p = (match a { m => m }) as u8 wrap;
            yield;
            p
        }
        struct Top { inst: f }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, a: u8) -> (Top, (u8, u1)) {
                let (i2, r, v) = self.inst.advance(a, 1);
                (Top { inst: i2 }, (r, v))
            }
        }
    ";
    let design = compile_str(src).expect("fresh match binding should compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let step =
        |sim: &mut Simulator, a: u64| sim.step(&[("a".to_string(), a)].into_iter().collect());
    assert_eq!(step(&mut sim, 7)["out_1"], 0);
    let d1 = step(&mut sim, 0);
    assert_eq!(d1["out_1"], 1);
    assert_eq!(d1["out_0"], 7);
}

#[test]
fn carried_value_with_a_type_annotation_compiles() {
    // A carried `let`-local may pin its register width with a type annotation
    // instead of an explicit cast.
    let src = "
        pipeline fn f(a: u8) -> u8 {
            let p: u8 = a;
            yield;
            p
        }
        struct Top { inst: f }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, a: u8) -> (Top, (u8, u1)) {
                let (i2, r, v) = self.inst.advance(a, 1);
                (Top { inst: i2 }, (r, v))
            }
        }
    ";
    let design = compile_str(src).expect("annotation-typed carried local should compile");
    assert!(design.modules[0]
        .regs
        .iter()
        .any(|r| r.name == "inst_p_s0" && r.ty.width() == 8));
}

#[test]
fn duplicate_pipeline_fn_reports_duplicate_function() {
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let p = (a) as u8 wrap; yield; p } \
         pipeline fn f(a: u8) -> u8 { let p = (a) as u8 wrap; yield; p }",
        "duplicate function",
    );
}

#[test]
fn carrying_an_aggregate_across_a_yield_is_rejected() {
    // A stage register is scalar (it resets to `0`); carrying an `Option<T>`
    // across a `yield` is rejected with a clear error (not a misleading
    // `literal 0 does not fit` from the synthesized register default).
    compile_err(
        "pipeline fn p(x: u8) -> u8 { \
            let a: Option<u8> = Some(x); yield; \
            match a { Some(v) => v, None => 0 } }",
        "must be a scalar",
    );
}

#[test]
fn in_valid_is_a_reserved_pipeline_parameter_name() {
    // `in_valid` is the synthesized input-valid parameter (PL3b); a pipeline
    // cannot reuse the name.
    compile_err(
        "pipeline fn f(a: u8, in_valid: u8) -> u8 { let p = (a) as u8 wrap; yield; p }",
        "reserved pipeline parameter",
    );
}

#[test]
fn in_valid_is_a_reserved_pipeline_local_name() {
    // The reservation also covers a top-level `let` binding.
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let in_valid = (a) as u8 wrap; yield; in_valid }",
        "reserved pipeline local",
    );
}

#[test]
fn in_valid_is_reserved_in_a_nested_binder() {
    // …and a nested block-local `let` (uniform reservation, not just top level).
    compile_err(
        "pipeline fn f(a: u8) -> u8 { \
            let p = (a) as u8 wrap; yield; \
            { let in_valid = (a) as u8 wrap; (p + in_valid) as u8 wrap } }",
        "reserved pipeline name",
    );
}

#[test]
fn pipeline_instance_in_an_aggregate_field_is_rejected() {
    // A pipeline instance must be a plain field in PL3a, not nested in an array.
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let p = (a) as u8 wrap; yield; p } \
         struct Top { insts: [f; 2] }",
        "must be held in a plain field",
    );
}

#[test]
fn value_used_in_non_consecutive_stages_stays_wavefront_coherent() {
    // `r` is read only in the tail (stage 2), skipping stage 1, so it is registered
    // THROUGH stage 1 (r_s0 -> r_s1) and must recombine with `t` (derived from a
    // stage-0 value) on the SAME wavefront.
    let src = "
        pipeline fn f(a: u8, b: u8) -> u8 {
            let r = (a) as u8 wrap;
            let s = (b) as u8 wrap;
            yield;
            let t = (s + 1) as u8 wrap;
            yield;
            (r + t) as u8 wrap
        }
        struct Top { inst: f }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, a: u8, b: u8) -> (Top, (u8, u1)) {
                let (i2, r, v) = self.inst.advance(a, b, 1);
                (Top { inst: i2 }, (r, v))
            }
        }
    ";
    let design = compile_str(src).expect("non-consecutive carry should compile");
    let m = &design.modules[0];
    let names: Vec<&str> = m.regs.iter().map(|r| r.name.as_str()).collect();
    for n in ["inst_r_s0", "inst_r_s1", "inst_s_s0", "inst_t_s1"] {
        assert!(names.contains(&n), "expected register `{n}` in {names:?}");
    }

    let mut sim = Simulator::new(m);
    let step = |sim: &mut Simulator, a: u64, b: u64| {
        sim.step(
            &[("a".to_string(), a), ("b".to_string(), b)]
                .into_iter()
                .collect(),
        )
    };
    assert_eq!(step(&mut sim, 2, 3)["out_1"], 0);
    assert_eq!(step(&mut sim, 0, 0)["out_1"], 0);
    let d2 = step(&mut sim, 0, 0);
    assert_eq!(d2["out_1"], 1);
    // r = a0 = 2; t = (b0 + 1) = 4; (r + t) = 6 — operands from the same wavefront.
    assert_eq!(d2["out_0"], 6);
}

// --- PL3c: global-stall backpressure (guarded hold) ------------------------

#[test]
fn pipeline_global_stall_freezes_and_resumes() {
    // `fn_pipeline_stall.snt`: `en` gates the guarded hold. With `en = 0` every
    // pipeline register (and the captured outputs) freezes; `en = 1` advances.
    let design =
        compile_str(include_str!("../../../examples/fn_pipeline_stall.snt")).expect("compile");
    let m = &design.modules[0];
    let mut sim = Simulator::new(m);
    let step = |sim: &mut Simulator, a: u64, b: u64, en: u64| {
        sim.step(
            &[
                ("a".to_string(), a),
                ("b".to_string(), b),
                ("en".to_string(), en),
            ]
            .into_iter()
            .collect(),
        )
    };
    // Run advancing for a few cycles so the stage registers hold real data.
    step(&mut sim, 3, 4, 1);
    step(&mut sim, 5, 6, 1);
    step(&mut sim, 7, 8, 1);

    // The observable register set (every `_q`), captured each cycle.
    let snapshot = |m: &sinter_compiler::ir::Module, row: &HashMap<String, u64>| -> Vec<u64> {
        m.outputs()
            .map(|p| p.name.as_str())
            .filter(|n| n.ends_with("_q"))
            .map(|n| row[n])
            .collect()
    };

    // Two consecutive stall cycles (with DIFFERENT garbage inputs) must observe
    // the identical register state — the pipe is frozen.
    let s1 = step(&mut sim, 99, 99, 0);
    let s2 = step(&mut sim, 11, 22, 0);
    let f1 = snapshot(m, &s1);
    let f2 = snapshot(m, &s2);
    assert_eq!(f1, f2, "every register frozen across the stall");

    // Resuming (en = 1) advances again. Mealy timing: the resumed step commits
    // its advance *after* the observation, so the change is visible the next
    // cycle — step twice and confirm the register set moved off the frozen value.
    step(&mut sim, 1, 1, 1);
    let r = step(&mut sim, 1, 1, 1);
    let f3 = snapshot(m, &r);
    assert_ne!(f3, f2, "the pipeline advances once `en` is high again");
}
