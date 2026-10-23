//! Shared wires: elaboration gives each non-trivial `let` binding (and call
//! argument) a named IR wire, so a value used several times is computed once
//! instead of being copied into every use.

use std::collections::HashMap;

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_str, verilog, Module};

fn module(src: &str) -> Module {
    let design = compile_str(src).expect("source compiles");
    design.modules.into_iter().next().expect("one module")
}

#[test]
fn a_let_used_several_times_is_one_wire() {
    let m = module(
        "struct S { pub regs: [u8; 4] = [0, 0, 0, 0], pub a: u8 = 0, pub b: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, i: u2) -> S {
                 let x = self.regs[i];
                 S { regs: self.regs, a: x, b: x ^ 1 }
             }
         }",
    );
    let x = m
        .lets
        .iter()
        .find(|l| l.name == "x")
        .expect("`x` is a wire");
    assert_eq!(x.ty.width(), 8);
    let sv = verilog::emit_module(&m);
    // The 4-way read mux appears once, in `x`'s assignment; both uses name it.
    assert_eq!(sv.matches("(i == 2'h0)").count(), 1, "one read mux:\n{sv}");
    assert!(sv.contains("        x = ((i == 2'h0) ? regs_0 : "));
    assert!(sv.contains("        a_next = x;\n"));
    assert!(sv.contains("        b_next = (x ^ 8'h1);\n"));
}

#[test]
fn wire_names_avoid_keywords_and_generated_names() {
    let m = module(
        "struct S { pub v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S {
                 let end = (x + 1) as u8 wrap;
                 let v = (end + 1) as u8 wrap;
                 S { v: v }
             }
         }",
    );
    let names: Vec<_> = m.lets.iter().map(|l| l.name.as_str()).collect();
    // `end` is a SystemVerilog keyword; `v` is the register's own name.
    assert_eq!(names, ["end_w", "v_w"]);
}

#[test]
fn a_wire_read_only_by_an_assert_is_simulated_but_not_emitted() {
    let m = module(
        "struct S { pub v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S {
                 let in_range = x < 200;
                 assert!(in_range == 1, \"x too big\");
                 S { v: x }
             }
         }",
    );
    assert!(m.lets.iter().any(|l| l.name == "in_range"));
    let sv = verilog::emit_module(&m);
    assert!(!sv.contains("in_range"), "assert-only wire emitted:\n{sv}");

    let mut sim = Simulator::new(&m);
    sim.step(&HashMap::from([("x".to_string(), 250)]));
    assert_eq!(sim.assert_failures().len(), 1);
}

#[test]
fn registers_and_ports_cannot_be_named_after_verilog_keywords() {
    for src in [
        "struct S { pub end: u8 = 0, }
         impl S { #[clock(domain = sys)] fn tick(self, x: u8) -> S { S { end: x } } }",
        "struct S { pub v: u8 = 0, }
         impl S { #[clock(domain = sys)] fn tick(self, begin: u8) -> S { S { v: begin } } }",
    ] {
        let err = compile_str(src).expect_err("keyword name").to_string();
        assert!(err.contains("is a SystemVerilog keyword"), "got: {err}");
    }
}
