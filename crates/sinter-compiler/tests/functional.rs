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
fn clock_root_must_return_self() {
    reject(
        "struct A { x: u1 = 0 } impl A { #[clock(domain = sys)] fn tick(self) -> u1 { self.x } }",
        "must return",
    );
}
