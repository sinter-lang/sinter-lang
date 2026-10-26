//! M1 multiplex tests (DESIGN §3.2/§3.3): the desugar shape, the one-physical-
//! multiplier (one-DSP) invariant, time-folded simulation, and the M1
//! rejections. The sim↔Verilog oracle is in `conformance.rs` (the dot2/dot3
//! examples).

use std::collections::HashMap;

use sinter_compiler::compile_str;
use sinter_compiler::ir::{Expr, ExprKind, Module};
use sinter_compiler::sim::Simulator;
use sinter_syntax::ast::BinOp;

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

/// Count every IR multiply reachable in an expression.
fn count_mul(e: &Expr) -> usize {
    let here = usize::from(matches!(e.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
    here + match &e.kind {
        ExprKind::Unary { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Shl { expr, .. }
        | ExprKind::Shr { expr, .. } => count_mul(expr),
        ExprKind::Binary { lhs, rhs, .. } => count_mul(lhs) + count_mul(rhs),
        ExprKind::VarShift { expr, amount, .. } => count_mul(expr) + count_mul(amount),
        ExprKind::If { cond, then, els } => count_mul(cond) + count_mul(then) + count_mul(els),
        ExprKind::Literal(_) | ExprKind::Ident(_) => 0,
    }
}

/// Every multiply in the whole module (lets + register next-states + outputs).
fn total_muls(m: &Module) -> usize {
    m.lets.iter().map(|l| count_mul(&l.value)).sum::<usize>()
        + m.reg_updates
            .iter()
            .map(|(_, e)| count_mul(e))
            .sum::<usize>()
        + m.out_drives
            .iter()
            .map(|(_, e)| count_mul(e))
            .sum::<usize>()
}

#[test]
fn dot2_folds_to_one_register_set_plus_phase_counter() {
    let design =
        compile_str(include_str!("../../../examples/fn_dot2_multiplex.snt")).expect("compile");
    let m = &design.modules[0];
    let regs: HashMap<&str, u32> = m
        .regs
        .iter()
        .map(|r| (r.name.as_str(), r.ty.width()))
        .collect();
    // ONE register per carried value (a1, b1, p) + the phase counter `ph` — NOT a
    // pipeline-style `_s{k}` chain. a0/b0 are read live at phase 0 (not latched).
    assert_eq!(
        regs.get("inst_ph"),
        Some(&1),
        "phase counter (1 bit for 2 phases)"
    );
    assert_eq!(regs.get("inst_a1_r"), Some(&8));
    assert_eq!(regs.get("inst_b1_r"), Some(&8));
    assert_eq!(regs.get("inst_p_r"), Some(&17));
    assert_eq!(
        m.regs.len(),
        4,
        "no extra registers (a0/b0 not latched): {regs:?}"
    );
    assert!(
        m.regs.iter().all(|r| r.reset == 0),
        "all registers reset to 0"
    );
}

#[test]
fn dot2_uses_exactly_one_multiplier() {
    // The one-DSP invariant (DESIGN §3.2): the two source `*` fold to a single
    // physical multiply (hoisted into one IR `let` by the Mul-CSE pass).
    let design =
        compile_str(include_str!("../../../examples/fn_dot2_multiplex.snt")).expect("compile");
    let m = &design.modules[0];
    assert_eq!(total_muls(m), 1, "exactly one multiplier");
    assert_eq!(
        m.lets.iter().filter(|l| count_mul(&l.value) > 0).count(),
        1,
        "the multiply is hoisted into exactly one shared wire"
    );
}

#[test]
fn dot3_uses_exactly_one_multiplier_across_three_phases() {
    let design =
        compile_str(include_str!("../../../examples/fn_dot3_multiplex.snt")).expect("compile");
    let m = &design.modules[0];
    assert_eq!(total_muls(m), 1, "three phases share one multiplier");
    // a2/b2 are live across TWO phase boundaries -> one latch each, held across
    // the intervening phase.
    let names: Vec<&str> = m.regs.iter().map(|r| r.name.as_str()).collect();
    for n in ["inst_a2_r", "inst_b2_r", "inst_acc0_r", "inst_acc1_r"] {
        assert!(names.contains(&n), "expected register `{n}` in {names:?}");
    }
}

#[test]
fn dot2_simulates_one_result_every_two_cycles() {
    let design =
        compile_str(include_str!("../../../examples/fn_dot2_multiplex.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let step = |sim: &mut Simulator, a0: u64, b0: u64, a1: u64, b1: u64| {
        sim.step(
            &[
                ("a0".to_string(), a0),
                ("b0".to_string(), b0),
                ("a1".to_string(), a1),
                ("b1".to_string(), b1),
            ]
            .into_iter()
            .collect(),
        )
    };
    // A datum entering at phase 0 (cycle 0) yields its result at phase 1 (cycle 1).
    // Inputs are held across the 2-cycle period so the latched a1/b1 match.
    assert_eq!(step(&mut sim, 3, 4, 5, 6)["out_1"], 0, "phase 0: filling");
    let r = step(&mut sim, 3, 4, 5, 6);
    assert_eq!(r["out_1"], 1, "phase 1: result valid");
    assert_eq!(r["out_0"], 3 * 4 + 5 * 6, "dot2 = 42");
    // Next period: a new datum.
    assert_eq!(step(&mut sim, 2, 2, 2, 2)["out_1"], 0);
    let r2 = step(&mut sim, 2, 2, 2, 2);
    assert_eq!(r2["out_1"], 1);
    assert_eq!(r2["out_0"], 2 * 2 + 2 * 2, "dot2 = 8");
}

#[test]
fn dot3_simulates_one_result_every_three_cycles() {
    let design =
        compile_str(include_str!("../../../examples/fn_dot3_multiplex.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let step = |sim: &mut Simulator, v: [u64; 6]| {
        sim.step(
            &[
                ("a0".to_string(), v[0]),
                ("b0".to_string(), v[1]),
                ("a1".to_string(), v[2]),
                ("b1".to_string(), v[3]),
                ("a2".to_string(), v[4]),
                ("b2".to_string(), v[5]),
            ]
            .into_iter()
            .collect(),
        )
    };
    let d = [2u64, 3, 4, 5, 6, 7]; // 2*3 + 4*5 + 6*7 = 6 + 20 + 42 = 68
    assert_eq!(step(&mut sim, d)["out_1"], 0, "phase 0");
    assert_eq!(step(&mut sim, d)["out_1"], 0, "phase 1");
    let r = step(&mut sim, d);
    assert_eq!(r["out_1"], 1, "phase 2: valid");
    assert_eq!(r["out_0"], 68, "dot3");
    // The phase counter wrapped 2 -> 0: a fresh period begins.
    assert_eq!(step(&mut sim, d)["out_1"], 0, "wrapped to phase 0");
}

#[test]
fn more_than_one_multiply_per_phase_is_rejected() {
    compile_err(
        "multiplex fn m(a: u8, b: u8, c: u8, d: u8) -> u17 { \
            let p = (a * b + c * d) as u17 wrap; yield; p }",
        "at most one `*` per phase",
    );
}

#[test]
fn multiplex_reserved_names_are_rejected() {
    compile_err(
        "multiplex fn m(a: u8, ph: u8) -> u8 { let p = (a) as u8 wrap; yield; p }",
        "reserved multiplex name",
    );
    compile_err(
        "multiplex fn m(a: u8) -> u8 { let shared = (a) as u8 wrap; yield; shared }",
        "reserved multiplex name",
    );
}

#[test]
fn multiplex_reserved_names_are_rejected_in_a_nested_binder() {
    // The reservation is uniform: a nested block-local `let shared` (which would
    // otherwise shadow the synthesized shared-multiply binding and break the
    // injected reference) and a nested `let ph` are both rejected, not just the
    // top level.
    compile_err(
        "multiplex fn m(a: u8, c: u8) -> u17 { let p = (a) as u17 wrap; yield; \
            { let shared = (c) as u17 wrap; (p + shared) as u17 wrap } }",
        "reserved multiplex name",
    );
    compile_err(
        "multiplex fn m(a: u8, c: u8) -> u17 { let p = (a) as u17 wrap; yield; \
            { let ph = (c) as u17 wrap; (p + ph) as u17 wrap } }",
        "reserved multiplex name",
    );
}

#[test]
fn multiplex_structural_errors_use_multiplex_wording() {
    // The desugar reuses the pipeline analysis, but its diagnostics must name the
    // right function color (a `multiplex fn`, not a `pipeline fn`).
    compile_err(
        "multiplex fn m(a: u8, b: u8) -> u17 { (a * b) as u17 wrap }",
        "`multiplex fn` needs at least one `yield`",
    );
    compile_err(
        "multiplex fn m(self) -> u8 { yield; (0) as u8 wrap }",
        "`multiplex fn` cannot take `self`",
    );
}

#[test]
fn multiplex_non_scalar_carried_value_is_rejected() {
    // A value carried across a phase boundary must be a scalar (an aggregate has
    // no `0` reset literal); here the array param `a` is read at both phases.
    compile_err(
        "multiplex fn m(a: [u8; 2]) -> u8 { let p = (a[0]) as u8 wrap; yield; (p + a[1]) as u8 wrap }",
        "must be a scalar",
    );
}

/// Phase 0 waits while `in_valid` is 0: no datum enters, and the next valid
/// cycle starts the period.
#[test]
fn phase_zero_waits_for_in_valid() {
    let src = "
        multiplex fn dot2(a0: u8, b0: u8, a1: u8, b1: u8) -> u17 {
            let p = (a0 * b0) as u17 wrap;
            yield;
            (p + (a1 * b1)) as u17 wrap
        }
        struct Top { inst: dot2 }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self, a: u8, go: u1) -> (Top, (u17, u1)) {
                let (inst, result, valid) = self.inst.advance(a, a, a, a, go);
                (Top { inst: inst }, (result, valid))
            }
        }";
    let design = compile_str(src).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let mut step = |a: u64, go: u64| {
        let out = sim.step(
            &[("a".to_string(), a), ("go".to_string(), go)]
                .into_iter()
                .collect(),
        );
        (out["out_0"], out["out_1"])
    };
    // Idle in phase 0: nothing becomes valid however long `go` stays low.
    for _ in 0..3 {
        assert_eq!(step(5, 0).1, 0, "waiting in phase 0");
    }
    // `go` takes 3; its result (3*3 + 3*3) is valid in the next cycle.
    assert_eq!(step(3, 1).1, 0, "phase 0 takes the datum");
    assert_eq!(step(9, 0), (18, 1), "phase 1: the result");
    assert_eq!(step(9, 0).1, 0, "back in phase 0, waiting again");
}
