//! EXT1: foreign Verilog modules through `#[verilog]` bindings (DESIGN §6.15 /
//! §9.13). The lowered shape (port wires, the instance record, model-only
//! state), the emitted Verilog, and the rules that keep the reference model
//! and the real module in lockstep. The sim-vs-Verilog check of a model
//! against real IP is in `conformance.rs` (`verilog_acc`).

use std::collections::HashMap;

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_project, compile_str, verilog, Module};

fn module(src: &str) -> Module {
    let design = compile_str(src).expect("source compiles");
    design.modules.into_iter().next().expect("one module")
}

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

/// A registered accumulator model (`Acc`) bound to Verilog `acc8`, plus a
/// parent tick. `{tick}` is the parent's `tick` body.
fn with_tick(tick: &str) -> String {
    format!(
        "#[verilog(module = \"acc8\")]
         struct Acc {{ acc: u8 = 0, }}
         struct AccOut {{ sum: u8, }}
         impl Acc {{
             fn step(self, din: u8) -> (Acc, AccOut) {{
                 (Acc {{ acc: (self.acc + din) as u8 wrap }}, AccOut {{ sum: self.acc }})
             }}
         }}
         struct Top {{ a: Acc, b: Acc, }}
         impl Top {{
             #[clock(domain = sys)]
             fn tick(self, x: u8, c: u1) -> (Top, u8) {{ {tick} }}
         }}"
    )
}

const GOOD_TICK: &str = "
    let (a, o) = self.a.step(x);
    let (b, p) = self.b.step(o.sum);
    (Top { a: a, b: b }, p.sum)
";

#[test]
fn a_step_lowers_to_port_wires_and_an_instance() {
    let m = module(&with_tick(GOOD_TICK));
    assert_eq!(m.instances.len(), 2);
    let a = &m.instances[0];
    assert_eq!(a.name, "u_a");
    assert_eq!(a.module, "acc8");
    assert_eq!(a.model, "Acc");
    assert_eq!(a.clk.as_deref(), Some("clk"));
    assert_eq!(a.rst_n.as_deref(), Some("rst_n"));
    assert_eq!(a.inputs, [("din".to_string(), "u_a_din".to_string())]);
    assert_eq!(a.outputs, [("sum".to_string(), "u_a_sum".to_string())]);
    assert_eq!(a.model_regs, ["a_acc"]);
    assert_eq!(a.model_lets, ["u_a__next_acc"]);

    // The model's registers are simulated but get no `_q` port.
    assert!(m.regs.iter().any(|r| r.name == "a_acc"));
    let outputs: Vec<_> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(outputs, ["out"]);
}

#[test]
fn the_simulator_runs_the_reference_model() {
    let m = module(&with_tick(GOOD_TICK));
    let mut sim = Simulator::new(&m);
    // `b` accumulates `a`'s running total, each one cycle behind.
    let outs: Vec<u64> = [1, 2, 3, 4]
        .iter()
        .map(|&x| sim.step(&HashMap::from([("x".to_string(), x)]))["out"])
        .collect();
    // a.sum: 0, 1, 3, 6; b.sum: 0, 0, 1, 4.
    assert_eq!(outs, [0, 0, 1, 4]);
}

#[test]
fn verilog_instantiates_the_module_instead_of_the_model() {
    let sv = verilog::emit_module(&module(&with_tick(GOOD_TICK)));
    let expected_a = "\
    acc8 u_a (
        .clk   (clk),
        .rst_n (rst_n),
        .din   (u_a_din),
        .sum   (u_a_sum)
    );
";
    assert!(sv.contains(expected_a), "instance `u_a` missing:\n{sv}");
    assert!(sv.contains("        u_b_din = u_a_sum;\n"));
    assert!(sv.contains("        out = u_b_sum;\n"));
    // Nothing from the model survives: no model registers, next-state wires,
    // or assignments to the instance outputs.
    for gone in ["a_acc", "__next_", "u_a_sum = ", "always_ff"] {
        assert!(!sv.contains(gone), "`{gone}` should not be emitted:\n{sv}");
    }
}

#[test]
fn a_stateless_model_is_combinational_and_instanced_per_call() {
    let m = module(
        "#[verilog(module = \"add8\")]
         struct Add {}
         struct AddOut { s: u8, }
         impl Add {
             fn step(self, a: u8, b: u8) -> (Add, AddOut) {
                 (self, AddOut { s: (a + b) as u8 wrap })
             }
         }
         struct Top { v: u8 = 0, adder: Add, }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> Top {
                 let (_a, o) = self.adder.step(x, self.v);
                 let (_b, p) = self.adder.step(o.s, 1);
                 Top { v: p.s, adder: self.adder }
             }
         }",
    );
    let names: Vec<_> = m.instances.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["u_add8_0", "u_add8_1"]);
    assert!(m
        .instances
        .iter()
        .all(|i| i.clk.is_none() && i.rst_n.is_none()));
    let sv = verilog::emit_module(&m);
    assert!(sv.contains("    add8 u_add8_0 (\n        .a (u_add8_0_a),"));
}

#[test]
fn clock_and_reset_ports_can_be_renamed_or_omitted() {
    let src = with_tick(GOOD_TICK).replace(
        "#[verilog(module = \"acc8\")]",
        "#[verilog(module = \"acc8\", clk = \"clk_i\", rst_n = none)]",
    );
    let m = module(&src);
    assert_eq!(m.instances[0].clk.as_deref(), Some("clk_i"));
    assert_eq!(m.instances[0].rst_n, None);
    let sv = verilog::emit_module(&m);
    assert!(sv.contains("    acc8 u_a (\n        .clk_i (clk),\n        .din   (u_a_din),"));
}

#[test]
fn a_stateful_instance_must_be_stepped_every_cycle() {
    compile_err(
        &with_tick(
            "let (a, o) = if c == 1 { self.a.step(x) } else { self.a.step(0) };
             (Top { a: a, b: self.b }, o.sum)",
        ),
        "must be stepped every cycle, outside any `if` / `match` arm",
    );
    compile_err(
        &with_tick("let (a, o) = self.a.step(x); (Top { a: a, b: self.b }, o.sum)"),
        "`#[verilog]` instance `self.b` (`Acc`) is never stepped",
    );
    compile_err(
        &with_tick(
            "let (a, o) = self.a.step(x);
             let (b, p) = self.b.step(x);
             (Top { a: if c == 1 { a } else { self.a }, b: b }, p.sum)",
        ),
        "the state of `#[verilog]` instance `u_a` (`Acc`) must be the result of its `step`",
    );
    compile_err(
        &with_tick(
            "let (a, o) = self.a.step(x);
             let (b, p) = self.b.step(x);
             (Top { a: b, b: a }, o.sum)",
        ),
        "must be the result of its `step`",
    );
}

#[test]
fn a_stateful_instance_is_stepped_at_most_once_and_through_its_field() {
    compile_err(
        &with_tick(
            "let (a, o) = self.a.step(x);
             let (a2, p) = self.a.step(o.sum);
             let (b, q) = self.b.step(x);
             (Top { a: a2, b: b }, p.sum)",
        ),
        "`self.a` is stepped more than once in one cycle",
    );
    compile_err(
        &with_tick(
            "let copy = self.a;
             let (a, o) = copy.step(x);
             let (b, p) = self.b.step(x);
             (Top { a: a, b: b }, o.sum)",
        ),
        "step the `#[verilog]` instance `Acc` through its state field",
    );
}

#[test]
fn model_state_is_private_to_the_model() {
    compile_err(
        &with_tick(
            "let (a, o) = self.a.step(x);
             let (b, p) = self.b.step(x);
             (Top { a: a, b: b }, self.a.acc)",
        ),
        "`a_acc` is internal state of the `#[verilog]` model `Acc`",
    );
}

#[test]
fn ports_are_scalars_in_ext1() {
    compile_err(
        "#[verilog(module = \"m\")]
         struct M { v: u8 = 0, }
         struct MOut { pair: [u8; 2], }
         impl M {
             fn step(self, x: u8) -> (M, MOut) { (M { v: x }, MOut { pair: [x, x] }) }
         }
         struct Top { m: M, }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> Top { let (m, o) = self.m.step(x); Top { m: m } }
         }",
        "output `MOut.pair` must be a scalar",
    );
}

#[test]
fn a_model_cannot_hold_a_memory() {
    compile_err(
        "#[verilog(module = \"m\")]
         struct M { data: mem<u8, 4> = 0, }
         struct MOut { q: u8, }
         impl M {
             fn step(self, a: u2) -> (M, MOut) {
                 (M { data: self.data.next(a, 0, a, 0) }, MOut { q: self.data.value() })
             }
         }
         struct Top { m: M, }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, a: u2) -> Top { let (m, o) = self.m.step(a); Top { m: m } }
         }",
        "cannot hold a `mem` / `rom` in EXT1",
    );
}

#[test]
fn malformed_bindings_are_rejected() {
    let bind = |attr: &str| with_tick(GOOD_TICK).replace("#[verilog(module = \"acc8\")]", attr);
    compile_err(
        &bind("#[verilog(src = \"a.v\")]"),
        "`#[verilog]` needs the Verilog module name",
    );
    compile_err(
        &bind("#[verilog(module = \"acc8\", width = 8)]"),
        "unknown `#[verilog]` argument `width`",
    );
    compile_err(
        &bind("#[verilog(module = acc8)]"),
        "`#[verilog]` argument `module` takes a string",
    );
}

#[test]
fn a_missing_source_file_is_an_error() {
    let dir = std::env::temp_dir().join("sinter-foreign-missing-src");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    let src = with_tick(GOOD_TICK).replace(
        "#[verilog(module = \"acc8\")]",
        "#[verilog(module = \"acc8\", src = \"rtl/acc8.v\")]",
    );
    std::fs::write(dir.join("main.snt"), src).expect("write main.snt");
    let err = compile_project(&dir.join("main.snt"))
        .expect_err("rtl/acc8.v does not exist")
        .to_string();
    assert!(
        err.contains("`#[verilog]` source `rtl/acc8.v` not found"),
        "got: {err}"
    );
}

/// A `multiplex fn` and a `#[verilog]` instance in one design: the multiply-CSE
/// pass must keep the wires topologically ordered (each reads only signals
/// defined before it) and leave the model's wires alone.
#[test]
fn multiplex_and_foreign_instances_combine() {
    let m = module(
        "multiplex fn dot2(a0: u8, b0: u8, a1: u8, b1: u8) -> u17 {
             let p = (a0 * b0) as u17 wrap;
             yield;
             (p + (a1 * b1)) as u17 wrap
         }
         #[verilog(module = \"inc17\")]
         struct Inc {}
         struct IncOut { y: u17, }
         impl Inc {
             fn step(self, x: u17) -> (Inc, IncOut) {
                 (self, IncOut { y: (x + 1) as u17 wrap })
             }
         }
         struct Top { inst: dot2, inc: Inc, }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, a: u8, b: u8) -> (Top, u17) {
                 let (inst2, result, valid) = self.inst.advance(a, b, a, b, 1);
                 let (_i, o) = self.inc.step(result);
                 (Top { inst: inst2, inc: self.inc }, o.y)
             }
         }",
    );
    assert_eq!(m.instances.len(), 1);

    // Topological order: every wire reads only inputs, registers, or earlier wires.
    let mut defined: std::collections::HashSet<String> = m
        .ports
        .iter()
        .map(|p| p.name.clone())
        .chain(m.regs.iter().map(|r| r.name.clone()))
        .collect();
    for l in &m.lets {
        let mut stack = vec![&l.value];
        while let Some(e) = stack.pop() {
            use sinter_compiler::ir::ExprKind as K;
            match &e.kind {
                K::Ident(n) => assert!(
                    defined.contains(n),
                    "`{}` reads `{n}` before it is defined",
                    l.name
                ),
                K::Literal(_) => {}
                K::Unary { expr, .. }
                | K::Cast { expr, .. }
                | K::Shl { expr, .. }
                | K::Shr { expr, .. } => stack.push(expr),
                K::Binary { lhs, rhs, .. } => stack.extend([&**lhs, &**rhs]),
                K::VarShift { expr, amount, .. } => stack.extend([&**expr, &**amount]),
                K::If { cond, then, els } => stack.extend([&**cond, &**then, &**els]),
            }
        }
        defined.insert(l.name.clone());
    }

    // dot2(3, 3, 3, 3) = 18, which the model increments to 19. The multiplex
    // result is valid on the second cycle of each two-cycle pass.
    let mut sim = Simulator::new(&m);
    let inputs = HashMap::from([("a".to_string(), 3), ("b".to_string(), 3)]);
    let outs: Vec<u64> = (0..4).map(|_| sim.step(&inputs)["out"]).collect();
    assert!(outs.contains(&19), "outputs were {outs:?}");
}
