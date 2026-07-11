//! BIT1 bit-manipulation tests (DESIGN §6.13 / §9.10): constant `<<` / `>>`
//! width inference and semantics (logical for unsigned, arithmetic for signed),
//! plus the BIT1 rejections. The sim↔Verilog oracle is in `conformance.rs` (the
//! `fn_shift` example).

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

fn step(sim: &mut Simulator, x: u64, s: u64) -> HashMap<String, u64> {
    sim.step(
        &[("x".to_string(), x), ("s".to_string(), s)]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    )
}

#[test]
fn const_shift_widths_and_semantics() {
    let design = compile_str(include_str!("../../../examples/fn_shift.snt")).expect("compile");
    let m = &design.modules[0];
    // `x >> 4` -> u4, `x << 2` -> u10, `s >> 3` -> i5.
    let width = |name: &str| m.regs.iter().find(|r| r.name == name).map(|r| r.ty.width());
    assert_eq!(width("hi"), Some(4), "u8 >> 4 = u4");
    assert_eq!(width("quad"), Some(10), "u8 << 2 = u10");
    assert_eq!(width("sar"), Some(5), "i8 >> 3 = i5");

    let mut sim = Simulator::new(m);
    // Registered outputs appear the cycle after their inputs, so apply then read.
    step(&mut sim, 0xED, 0xF8); // x = 0xED, s = -8 (i8)
    let out = step(&mut sim, 0, 0);
    assert_eq!(out["hi_q"], 0xE, "0xED >> 4 (logical)");
    assert_eq!(out["quad_q"], 0xED << 2, "0xED << 2 = 948");
    assert_eq!(
        out["sar_q"], 31,
        "-8 >> 3 = -1 (arithmetic) -> i5 = 0b11111"
    );

    step(&mut sim, 0x81, 40); // x = 0x81, s = 40 (positive)
    let out = step(&mut sim, 0, 0);
    assert_eq!(out["hi_q"], 0x8, "0x81 >> 4");
    assert_eq!(out["quad_q"], (0x81u64 << 2) & 0x3FF, "0x81 << 2 fits u10");
    assert_eq!(out["sar_q"], 5, "40 >> 3 = 5");
}

#[test]
fn shift_amount_must_be_a_literal() {
    compile_err(
        "struct S { y: u8 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8, k: u8) -> S { S { y: x >> k } } }",
        "must be a literal",
    );
}

#[test]
fn right_shift_past_width_is_rejected() {
    compile_err(
        "struct S { y: u1 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> S { S { y: (x >> 8) as u1 wrap } } }",
        "must be less than the operand width",
    );
}
