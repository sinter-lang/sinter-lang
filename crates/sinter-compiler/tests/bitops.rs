//! BIT1/BIT2 bit-manipulation tests (DESIGN §6.13 / §9.10): constant `<<` / `>>`
//! (BIT1, logical for unsigned / arithmetic for signed), bit-slice `x[hi:lo]`,
//! concatenation `a @ b`, and the variable (barrel) shift by a runtime amount
//! (BIT2) — width inference, semantics, and the rejections. The sim↔Verilog
//! oracle is in `conformance.rs` (`fn_shift` / `fn_slice` / `fn_concat` /
//! `fn_barrel`).

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
fn bit_slice_extracts_fields() {
    let design = compile_str(include_str!("../../../examples/fn_slice.snt")).expect("compile");
    let m = &design.modules[0];
    let width = |name: &str| m.regs.iter().find(|r| r.name == name).map(|r| r.ty.width());
    assert_eq!(width("hi"), Some(4), "x[7:4] = u4");
    assert_eq!(width("mid"), Some(3), "x[5:3] = u3");
    assert_eq!(width("top"), Some(1), "x[7:7] = u1");

    let mut sim = Simulator::new(m);
    // Registered outputs appear the cycle after their inputs.
    sim.step(&[("x".to_string(), 0xB6)].into_iter().collect()); // 0xB6 = 1011_0110
    let out = sim.step(&[("x".to_string(), 0)].into_iter().collect());
    assert_eq!(out["hi_q"], 0xB, "x[7:4] of 0xB6");
    assert_eq!(out["lo_q"], 0x6, "x[3:0] of 0xB6");
    assert_eq!(out["mid_q"], 0b110, "x[5:3] of 0xB6");
    assert_eq!(out["top_q"], 1, "x[7:7] of 0xB6");
}

#[test]
fn bit_slice_requires_unsigned_and_in_range() {
    // A signed operand is rejected (would need a sign-changing cast).
    compile_err(
        "struct S { pub y: u4 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: i8) -> S { S { y: x[3:0] } } }",
        "unsigned",
    );
    // hi out of range for the operand width.
    compile_err(
        "struct S { pub y: u2 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> S { S { y: x[9:8] } } }",
        "out of range",
    );
}

#[test]
fn concat_joins_fields() {
    let design = compile_str(include_str!("../../../examples/fn_concat.snt")).expect("compile");
    let m = &design.modules[0];
    let width = |name: &str| m.regs.iter().find(|r| r.name == name).map(|r| r.ty.width());
    assert_eq!(width("swap"), Some(8), "u4 @ u4 = u8");
    assert_eq!(width("wide"), Some(12), "u8 @ u4 = u12");

    let mut sim = Simulator::new(m);
    sim.step(
        &[("x".to_string(), 0xCA), ("b".to_string(), 0x3)]
            .into_iter()
            .collect(),
    );
    let out = sim.step(
        &[("x".to_string(), 0), ("b".to_string(), 0)]
            .into_iter()
            .collect(),
    );
    // swap = x[3:0] @ x[7:4] = 0xA @ 0xC = 0xAC.
    assert_eq!(out["swap_q"], 0xAC, "nibble swap of 0xCA");
    // wide = x @ b = (0xCA << 4) | 0x3 = 0xCA3.
    assert_eq!(out["wide_q"], 0xCA3, "0xCA @ 0x3");
}

#[test]
fn concat_operands_must_be_unsigned() {
    compile_err(
        "struct S { pub y: u12 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, a: i8, b: u4) -> S { S { y: a @ b } } }",
        "unsigned",
    );
}

#[test]
fn variable_barrel_shift_semantics() {
    let design = compile_str(include_str!("../../../examples/fn_barrel.snt")).expect("compile");
    let m = &design.modules[0];
    // A variable shift keeps the operand width (unlike a constant shift).
    let width = |n: &str| m.regs.iter().find(|r| r.name == n).map(|r| r.ty.width());
    assert_eq!(width("sll"), Some(8), "u8 << sh stays u8");
    assert_eq!(width("sra"), Some(8), "i8 >> sh stays i8");

    let mut sim = Simulator::new(m);
    // Registered outputs appear the cycle after their inputs.
    let mut barrel = |x: u64, s: u64, sh: u64| -> (u64, u64, u64) {
        sim.step(
            &[
                ("x".to_string(), x),
                ("s".to_string(), s),
                ("sh".to_string(), sh),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        );
        let o = sim.step(
            &[
                ("x".to_string(), 0),
                ("s".to_string(), 0),
                ("sh".to_string(), 0),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        );
        (o["sll_q"], o["srl_q"], o["sra_q"])
    };
    // Shift by 0 is the identity.
    assert_eq!(barrel(0xAB, 0x05, 0), (0xAB, 0xAB, 0x05));
    // Logical left/right within the width (bits shifted out are lost).
    assert_eq!(barrel(0xAB, 0, 4), ((0xABu64 << 4) & 0xFF, 0xAB >> 4, 0));
    // Arithmetic right shift of a negative: -8 >> 1 = -4 (i8 = 0xFC).
    assert_eq!(barrel(0, 0xF8, 1).2, 0xFC);
    // Shift == width (8): logical -> 0; arithmetic -> the sign fill.
    assert_eq!(barrel(0xFF, 0xFF, 8), (0, 0, 0xFF), "s=-1 sign-fills");
    assert_eq!(barrel(0xFF, 0x7F, 8), (0, 0, 0), "s=+127 -> 0");
    // Shift > width (up to 15): same as >= width.
    assert_eq!(barrel(0xFF, 0xF0, 15), (0, 0, 0xFF), "s=-16 sign-fills");
}

#[test]
fn named_const_shift_amount_is_a_same_width_barrel() {
    // A *literal* amount widens (`u8 << 2` = u10); a named `const` is a
    // non-literal, so it takes the same-width barrel path (`u8 << K` = u8) — no
    // silent widening (and any width-mismatched use is a loud type error). This
    // pins the DESIGN §6.13 literal-vs-runtime rule.
    let design = compile_str(
        "const K: u8 = 2;\n\
         struct S { pub y: u8 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> (S, u8) { (S { y: 0 }, x << K) } }",
    )
    .expect("compile");
    let m = &design.modules[0];
    // The `x << K` output port is u8 (same width), not u10 (a widened literal).
    let out = m.ports.iter().find(|p| p.name == "out").expect("out port");
    assert_eq!(
        out.ty.width(),
        8,
        "`x << K` (K a const) is a same-width barrel"
    );
}

#[test]
fn variable_shift_amount_must_be_unsigned() {
    compile_err(
        "struct S { pub y: u8 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8, sh: i4) -> S { S { y: x << sh } } }",
        "unsigned",
    );
}

#[test]
fn right_shift_past_width_is_rejected() {
    compile_err(
        "struct S { pub y: u1 = 0 }\n\
         impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> S { S { y: (x >> 8) as u1 wrap } } }",
        "must be less than the operand width",
    );
}
