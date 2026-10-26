//! The v1.0.0-rc.1 conveniences: struct update, `Self`, constant folding, and
//! tuple / nested `match` patterns. The oracle runs `examples/fn_ergonomics.snt`.

use std::collections::HashMap;

use sinter_compiler::compile_str;
use sinter_compiler::sim::Simulator;

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

/// Simulate `src` for one cycle with `inputs`, returning the next cycle's
/// outputs (the registers after the first edge).
fn after_one_cycle(src: &str, inputs: &[(&str, u64)]) -> HashMap<String, u64> {
    let design = compile_str(src).expect("compiles");
    let mut sim = Simulator::new(&design.modules[0]);
    let ins: HashMap<String, u64> = inputs.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    sim.step(&ins);
    sim.step(&ins)
}

#[test]
fn struct_update_keeps_the_unlisted_fields() {
    let out = after_one_cycle(
        "struct T { pub a: u8 = 1, pub b: u8 = 2, pub c: u8 = 3 }
         impl T { #[clock(domain = sys)] fn tick(self, x: u8) -> Self { T { b: x, ..self } } }",
        &[("x", 9)],
    );
    assert_eq!((out["a_q"], out["b_q"], out["c_q"]), (1, 9, 3));
}

#[test]
fn struct_update_from_an_expression() {
    // The base is a call; the update takes its unlisted fields.
    let out = after_one_cycle(
        "struct P { a: u8, b: u8 }
         fn make(x: u8) -> P { P { a: x, b: (x + 1) as u8 wrap } }
         struct T { pub a: u8 = 0, pub b: u8 = 0 }
         impl T {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> T {
                 let p = P { a: 7, ..make(x) };
                 T { a: p.a, b: p.b }
             }
         }",
        &[("x", 4)],
    );
    assert_eq!((out["a_q"], out["b_q"]), (7, 5));
}

#[test]
fn struct_update_rejections() {
    compile_err(
        "struct T { pub a: u8 = 0 }
         impl T { #[clock(domain = sys)] fn tick(self) -> T { T { a: 1, ..self } } }",
        "every field of `T` is listed",
    );
    compile_err(
        "struct T { pub a: u8 = 0 }
         impl T { #[clock(domain = sys)] fn tick(self) -> T { T { z: 1, ..self } } }",
        "z",
    );
}

#[test]
fn self_names_the_impl_struct() {
    let out = after_one_cycle(
        "struct T { pub n: u8 = 5 }
         impl T {
             fn twice(self) -> Self { Self { n: (self.n + self.n) as u8 wrap } }
             #[clock(domain = sys)] fn tick(self) -> Self { self.twice() }
         }",
        &[],
    );
    assert_eq!(out["n_q"], 10);
}

#[test]
fn constants_fold_in_any_order() {
    let out = after_one_cycle(
        "const LAST: u8 = (N - 1) as u8 wrap;
         const N: u8 = 8;
         const NEG: i8 = -128;
         const SLOTS: u3 = 1_u2 << 1;
         struct T { pub last: u8 = LAST, pub neg: i8 = NEG, pub a: [u1; SLOTS] = [0; SLOTS] }
         impl T { #[clock(domain = sys)] fn tick(self) -> T { self } }",
        &[],
    );
    assert_eq!(out["last_q"], 7);
    assert_eq!(out["neg_q"], 0x80);
    assert!(out.contains_key("a_1_q") && !out.contains_key("a_2_q"));
}

#[test]
fn constant_rejections() {
    let with = |c: &str| {
        format!(
            "{c}
             struct T {{ pub v: u8 = 0 }}
             impl T {{ #[clock(domain = sys)] fn tick(self) -> T {{ self }} }}"
        )
    };
    compile_err(
        &with("const A: u8 = 1; const B: u8 = A + 1;"),
        "type mismatch: expected `u8`, found `u9`",
    );
    compile_err(
        &with("const A: u8 = B; const B: u8 = A;"),
        "depends on itself",
    );
    compile_err(&with("const A: i8 = -129;"), "does not fit in `i8`");
    compile_err(
        &with("const A: u8 = 3 + 4;"),
        "cannot infer a type for literal",
    );
}

#[test]
fn tuple_and_nested_patterns_select_the_right_arm() {
    let src = "
        struct T { pub v: u8 = 0 }
        impl T {
            #[clock(domain = sys)]
            fn tick(self, a: u1, o: Option<Option<u8>>) -> T {
                T { v: match (a, o) {
                    (1, Some(Some(x))) => x,
                    (0, Some(Some(_))) => 1,
                    (_, Some(None)) => 2,
                    (_, None) => 3,
                } }
            }
        }";
    let run = |a, outer, inner, x| {
        after_one_cycle(
            src,
            &[
                ("a", a),
                ("o_is_some", outer),
                ("o_payload_is_some", inner),
                ("o_payload_payload", x),
            ],
        )["v_q"]
    };
    assert_eq!(run(1, 1, 1, 42), 42);
    assert_eq!(run(0, 1, 1, 42), 1);
    assert_eq!(run(1, 1, 0, 42), 2);
    assert_eq!(run(1, 0, 1, 42), 3);
}

#[test]
fn exhaustiveness_names_a_missing_value() {
    let with = |m: &str| {
        format!(
            "enum Op {{ Add, Sub }}
             struct T {{ pub v: u8 = 0 }}
             impl T {{
                 #[clock(domain = sys)]
                 fn tick(self, a: u1, b: u1, op: Op, o: Option<Option<u8>>) -> T {{
                     T {{ v: {m} }}
                 }}
             }}"
        )
    };
    // All four `u1` pairs are exhaustive without a `_` arm.
    compile_str(&with(
        "match (a, b) { (0, 0) => 0, (0, 1) => 1, (1, 0) => 2, (1, 1) => 3 }",
    ))
    .expect("every pair is covered");
    compile_err(
        &with("match (a, b) { (0, 0) => 0, (1, _) => 2 }"),
        "no arm matches `(0, 1)`",
    );
    compile_err(
        &with("match o { Some(Some(_)) => 1, None => 2 }"),
        "no arm matches `Some(None)`",
    );
    compile_err(
        &with("match (op, o) { (Op::Add, _) => 1 }"),
        "no arm matches `(Op::Sub, _)`",
    );
    // A flat enum match still lists everything it misses.
    compile_err(&with("match op { Op::Add => 1 }"), "missing Sub");
}
