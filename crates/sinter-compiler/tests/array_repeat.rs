//! The array repeat `[value; N]` (DESIGN §6.6): expanded to `N` copies of
//! `value` before checking, with `N` a literal or a literal-valued `const`.

use std::collections::HashMap;

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_project, compile_str};

#[test]
fn repeat_resets_and_builds_arrays() {
    let design = compile_str(
        "const N: u32 = 4;
         struct S {
             regs: [u8; 32] = [0; 32],
             hist: [u4; N] = [0b1010_u4; N],
         }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S {
                 S { regs: [x; 32], hist: self.hist }
             }
         }",
    )
    .expect("compiles");
    let m = &design.modules[0];
    let reset = |n: &str| m.regs.iter().find(|r| r.name == n).map(|r| r.reset);
    assert_eq!(reset("regs_31"), Some(0));
    assert_eq!(reset("hist_3"), Some(0b1010));

    let mut sim = Simulator::new(m);
    sim.step(&HashMap::from([("x".to_string(), 7)]));
    let out = sim.step(&HashMap::from([("x".to_string(), 0)]));
    assert_eq!(out["regs_0_q"], 7);
    assert_eq!(out["regs_31_q"], 7);
}

#[test]
fn the_length_must_be_a_literal_or_const() {
    let err = compile_str(
        "struct S { v: [u8; 2] = [0; 2], }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S { S { v: [x; M] } }
         }",
    )
    .expect_err("M is undefined")
    .to_string();
    assert!(
        err.contains("needs a `const` with a literal value"),
        "got: {err}"
    );
}

#[test]
fn a_const_length_resolves_across_modules() {
    let dir = std::env::temp_dir().join("sinter-array-repeat-mods");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::write(dir.join("sizes.snt"), "pub const WIDTH: u32 = 3;\n").expect("write");
    std::fs::write(
        dir.join("main.snt"),
        "mod sizes;
         use root::sizes::WIDTH;
         struct S { v: [u8; WIDTH] = [9; WIDTH], }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S { S { v: self.v } }
         }",
    )
    .expect("write");
    let design = compile_project(&dir.join("main.snt")).expect("compiles");
    let resets: Vec<u64> = design.modules[0].regs.iter().map(|r| r.reset).collect();
    assert_eq!(resets, [9, 9, 9]);
}

/// Width-suffixed literals (desugared to casts) work as `const` values and
/// register reset values, not only in expressions.
#[test]
fn suffixed_literals_fold_in_consts_and_resets() {
    let design = compile_str(
        "const D: u7 = 0b0111111_u7;
         struct S { v: u4 = 0b1010_u4, w: u7 = D, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u4) -> S { S { v: x, w: self.w } }
         }",
    )
    .expect("compiles");
    let resets: Vec<u64> = design.modules[0].regs.iter().map(|r| r.reset).collect();
    assert_eq!(resets, [0b1010, 0b0111111]);
}
