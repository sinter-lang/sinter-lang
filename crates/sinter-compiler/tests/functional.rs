//! F1 tests: resolution + type checking of the functional surface (DESIGN §2.1).
//!
//! These exercise F1 (`check_functional`) in isolation — not the full
//! `compile_str`, which also runs F2 elaboration and so would reject the
//! still-unlowered F3 constructs (enums/`match`/methods/arrays) that several
//! well-typed cases here intentionally use.

use sinter_compiler::functional::check_functional;
use sinter_syntax::parse_program;

/// Type-check a source through F1; `Ok(())` means it passed resolution + typing.
fn check(src: &str) -> Result<(), String> {
    let program = parse_program(src).map_err(|e| e.to_string())?;
    check_functional(&program).map_err(|e| e.to_string())
}

/// Assert the source is rejected and the error mentions `needle`.
fn reject(src: &str, needle: &str) {
    match check(src) {
        Ok(()) => panic!("expected rejection containing {needle:?}, but it type-checked"),
        Err(msg) => assert!(
            msg.contains(needle),
            "error did not contain {needle:?}.\n  got: {msg}"
        ),
    }
}

#[test]
fn go_board_example_type_checks() {
    let src = include_str!("../../../examples/go_board_counter.snt");
    check(src).expect("go_board_counter.snt should type-check");
}

#[test]
fn minimal_struct_impl_with_cast() {
    let src = "
        struct C { v: u8 = 0 }
        impl C {
            fn step(self, i: u1) -> C {
                C { v: if i == 1 { (self.v + 1) as u8 wrap } else { self.v } }
            }
        }
    ";
    check(src).expect("should type-check");
}

#[test]
fn exhaustive_enum_match() {
    let src = "
        enum E { A, B }
        fn classify(e: E) -> u1 {
            match e { E::A => 0, E::B => 1 }
        }
    ";
    check(src).expect("should type-check");
}

#[test]
fn tuple_returning_substep_destructured() {
    let src = "
        struct D { v: u4 = 0 }
        impl D {
            fn step(self) -> (D, u1) { (self, 0) }
        }
        struct Top { d: D }
        impl Top {
            #[clock(domain = sys)]
            fn tick(self) -> Top {
                let (d, _carry) = self.d.step();
                Top { d: d }
            }
        }
    ";
    check(src).expect("should type-check");
}

#[test]
fn unknown_type_is_rejected() {
    reject("struct S { a: Nope = 0 }", "unknown type `Nope`");
}

#[test]
fn unknown_field_is_rejected() {
    reject(
        "struct S { a: u8 = 0 } fn f(s: S) -> u8 { s.b }",
        "no field `b`",
    );
}

#[test]
fn missing_struct_field_is_rejected() {
    reject(
        "struct S { a: u8, b: u8 } fn f() -> S { S { a: 0 } }",
        "missing field",
    );
}

#[test]
fn field_type_mismatch_is_rejected() {
    reject(
        "enum E { X } struct S { a: u8 = 0 } fn f() -> S { S { a: E::X } }",
        "type mismatch",
    );
}

#[test]
fn unnarrowed_arithmetic_requires_a_cast() {
    // `s.a + 1` widens u8 -> u9; assigning it back to the u8 field must be a
    // type error until an explicit narrowing cast is written (DESIGN §6.2/6.3).
    reject(
        "struct S { a: u8 = 0 } fn f(s: S) -> S { S { a: s.a + 1 } }",
        "expected `u8`, found `u9`",
    );
}

#[test]
fn nested_arithmetic_type_checks() {
    // A multiply-accumulate: `a*b` and `c*d` each widen u4*u4 -> u8, and the
    // sum widens u8+u8 -> u9 (DESIGN §6.2). Nested operands must NOT be hinted
    // with their sibling's width — only bare literals are (regression: F1 once
    // pushed the inferred result type onto non-literal operands and rejected
    // this).
    let src = "fn mac(a: u4, b: u4, c: u4, d: u4) -> u9 { a * b + c * d }";
    check(src).expect("nested width-inferred arithmetic should type-check");
}

#[test]
fn out_of_range_field_default_is_rejected() {
    // A struct field's reset value is type-checked against the field type, so an
    // out-of-range initializer is a compile error rather than a silently-masked
    // reset (DESIGN §6.10).
    reject(
        "struct S { a: u4 = 99 }",
        "literal `99` does not fit in `u4`",
    );
}

#[test]
fn out_of_range_fixed_field_default_is_rejected() {
    // Fixed-point fields are checked too — `u4.4` is 8 erased bits (max 255), so
    // 999 does not fit. (Regression: `literal_fits` once returned true for any
    // fixed-point type, masking out-of-range fixed defaults.)
    reject(
        "struct S { a: u4.4 = 999 }",
        "literal `999` does not fit in `u4.4`",
    );
}

#[test]
fn non_exhaustive_enum_match_is_rejected() {
    reject(
        "enum E { A, B, C } fn f(e: E) -> u2 { match e { E::A => 0, E::B => 1 } }",
        "non-exhaustive",
    );
}

#[test]
fn scalar_match_without_wildcard_is_rejected() {
    reject(
        "fn f(x: u8) -> u8 { match x { 0 => 0, 1 => 1 } }",
        "non-exhaustive",
    );
}

#[test]
fn unknown_method_is_rejected() {
    reject(
        "struct S { a: u8 = 0 } fn f(s: S) -> u8 { s.bump() }",
        "no method `bump`",
    );
}

#[test]
fn multiple_clock_roots_are_rejected() {
    let src = "
        struct A { x: u1 = 0 }
        struct B { y: u1 = 0 }
        impl A { #[clock(domain = sys)] fn tick(self) -> A { self } }
        impl B { #[clock(domain = sys)] fn tick(self) -> B { self } }
    ";
    reject(src, "more than one `#[clock]` root");
}

#[test]
fn module_items_are_rejected_until_lowered() {
    // The module system parses (S1) but the single-file compile path does not yet
    // resolve/lower it (S2/S3) — reject rather than silently drop.
    reject("mod foo;", "not yet lowered");
    reject("use root::debounce::Debounce;", "not yet lowered");
}

#[test]
fn pipeline_fn_definition_type_checks() {
    // A well-formed `pipeline fn` type-checks across `yield` stage boundaries
    // (PL2). `a * b` widens u8*u8 -> u16; the tail matches the return type.
    check("pipeline fn mac(a: u8, b: u8) -> u16 { let p = a * b; yield; p }")
        .expect("a well-formed pipeline fn should type-check");
}

#[test]
fn pipeline_carries_locals_across_stages() {
    // `p` (stage 0) is used after two `yield`s — it lives across both boundaries
    // (registered in PL3); types flow through unchanged.
    check(
        "pipeline fn f(a: u8, b: u8) -> u16 { \
            let p = a * b; yield; \
            let q = (p + p) as u16 wrap; yield; \
            (p + q) as u16 wrap }",
    )
    .expect("cross-stage locals should type-check");
}

#[test]
fn pipeline_may_call_a_combinational_fn() {
    check(
        "fn dbl(x: u8) -> u9 { x + x } \
         pipeline fn f(a: u8) -> u9 { let y = dbl(a); yield; y }",
    )
    .expect("a pipeline may call a combinational fn (DESIGN §2.2)");
}

#[test]
fn calling_a_pipeline_fn_directly_is_rejected() {
    // A pipeline is instantiated + advanced (PL3), never called inline.
    reject(
        "pipeline fn mac(a: u8, b: u8) -> u16 { let p = a * b; yield; p } \
         fn use_it(a: u8, b: u8) -> u16 { mac(a, b) }",
        "cannot be called directly",
    );
}

#[test]
fn pipeline_fn_without_yield_is_rejected() {
    reject(
        "pipeline fn f(a: u8) -> u8 { a }",
        "needs at least one `yield`",
    );
}

#[test]
fn pipeline_yield_tap_is_not_yet_supported() {
    reject(
        "pipeline fn f(a: u8) -> u8 { let b = a; yield b; b }",
        "taps are not yet supported",
    );
}

#[test]
fn pipeline_fn_cannot_be_a_method() {
    reject(
        "struct S { x: u1 = 0 } impl S { pipeline fn p(self) -> u1 { yield; self.x } }",
        "cannot be a method",
    );
}

#[test]
fn multiplex_fn_is_rejected() {
    reject(
        "multiplex fn m(a: u8) -> u8 { yield; a }",
        "not yet supported",
    );
}

#[test]
fn yield_outside_a_pipeline_is_rejected() {
    // `yield` in a combinational fn (or nested in a pipeline's `if`/block).
    reject(
        "fn f(x: u8) -> u8 { yield; x }",
        "only valid in a `pipeline fn`",
    );
}

#[test]
fn clock_root_must_return_self() {
    reject(
        "struct A { x: u1 = 0 } impl A { #[clock(domain = sys)] fn tick(self) -> u1 { self.x } }",
        "must return",
    );
}
