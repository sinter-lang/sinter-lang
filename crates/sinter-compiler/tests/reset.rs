//! RST1 (DESIGN §9.15): `#[no_reset]` registers. They take their default at
//! power-on only: `rst_n` leaves them alone, in the Verilog (a reset-less
//! `always_ff` plus an `initial`) and in the simulators (`step_in_reset`).
//! The directed oracle case (`fn_no_reset.csv`, resets mid-run) is in
//! `conformance.rs`.

#[path = "support/oracle.rs"]
mod oracle;

use std::collections::HashMap;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{
    resets_to_bytes, run_reference_with_resets, stimulus_to_bytes, Simulator,
};
use sinter_compiler::stimulus::Stimulus;
use sinter_compiler::{compile_str, verilator, verilog};

const NO_RESET: &str = include_str!("../../../examples/fn_no_reset.snt");
const NO_RESET_CSV: &str = include_str!("../../../examples/fn_no_reset.csv");

fn module_of(src: &str) -> Module {
    compile_str(src)
        .expect("compile")
        .modules
        .into_iter()
        .next()
        .expect("one module")
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

fn inputs(x: u64, we: u64, a: u64) -> HashMap<String, u64> {
    [("x", x), ("we", we), ("a", a)]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect()
}

#[test]
fn the_attribute_reaches_every_register_under_the_field() {
    let m = module_of(NO_RESET);
    let flags: Vec<_> = m
        .regs
        .iter()
        .map(|r| (r.name.as_str(), r.no_reset))
        .collect();
    assert_eq!(
        flags,
        [
            ("phase", false),
            ("dp_acc", true),
            ("dp_last", true),
            ("bank_rdata", true)
        ]
    );
}

#[test]
fn inside_an_array_and_an_option_too() {
    let m = module_of(
        "struct Cell { #[no_reset] v: u8 = 0, k: u1 = 0, }
         struct Top {
             cells: [Cell; 2] = [Cell { v: 0, k: 0 }, Cell { v: 0, k: 0 }],
             #[no_reset] o: Option<u4> = None,
         }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self) -> (Top, u8) { (self, self.cells[0].v) }
         }",
    );
    let bare: Vec<_> = m
        .regs
        .iter()
        .filter(|r| r.no_reset)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(bare, ["cells_0_v", "cells_1_v", "o_is_some", "o_payload"]);
}

#[test]
fn verilog_has_a_reset_less_block_with_power_on_values() {
    let sv = verilog::emit_module(&module_of(NO_RESET));
    for line in [
        "    initial begin\n        dp_acc = 16'h0;\n        dp_last = 8'h0;\n        bank_rdata = 8'h0;\n    end",
        "    always_ff @(posedge clk) begin\n        dp_acc <= dp_acc_next;",
        "        if (!rst_n) begin\n            phase <= 4'h0;\n        end else begin",
    ] {
        assert!(sv.contains(line), "missing\n{line}\nin:\n{sv}");
    }
    assert!(
        !sv.contains("dp_acc <= 16'h0"),
        "dp_acc must not reset:\n{sv}"
    );
}

#[test]
fn a_design_without_reset_registers_sinks_rst_n() {
    let m = module_of(
        "struct T { #[no_reset] pub n: u8 = 5, }
         impl T { #[clock(domain = sys)] fn tick(self) -> T { T { n: (self.n + 1) as u8 wrap } } }",
    );
    let sv = verilog::emit_module(&m);
    assert!(sv.contains("wire _unused_rst_n = rst_n;"), "{sv}");
    assert!(!sv.contains("negedge rst_n"), "{sv}");
    assert!(sv.contains("n = 8'h5;"), "power-on value:\n{sv}");
    if oracle::tools().verilator {
        let dir = oracle::workdir("reset-all-bare");
        let path = dir.join("T.sv");
        std::fs::write(&path, &sv).expect("write sv");
        oracle::lint_verilator(&path, &[], "T", oracle::Lint::Strict);
    }
}

#[test]
fn step_in_reset_resets_only_the_reset_registers() {
    let m = module_of(NO_RESET);
    let mut sim = Simulator::new(&m);
    for x in [1, 2, 3] {
        sim.step(&inputs(x, 1, 3));
    }
    // phase = 3, acc = 6, bank[3] = 3, read register = 2 (the pre-edge word).
    let out = sim.step_in_reset(&inputs(4, 0, 3));
    assert_eq!(out["phase_q"], 0, "phase reads its reset value in reset");
    assert_eq!(out["dp_acc_q"], 6, "acc keeps its value");
    let out = sim.step(&inputs(0, 0, 3));
    assert_eq!(out["phase_q"], 0, "phase was held through the reset edge");
    assert_eq!(out["dp_acc_q"], 10, "acc updated on the reset edge");
    assert_eq!(out["out_1"], 3, "the bank's read register updated too");
}

#[test]
fn reset_returns_everything_to_power_on() {
    let m = module_of(NO_RESET);
    let mut sim = Simulator::new(&m);
    sim.step(&inputs(9, 1, 0));
    sim.step(&inputs(9, 1, 0));
    sim.reset();
    let out = sim.step(&inputs(0, 0, 0));
    assert_eq!((out["phase_q"], out["dp_acc_q"]), (0, 0));
}

#[test]
fn rst_n_column_is_parsed() {
    let m = module_of(NO_RESET);
    let stim = Stimulus::parse_csv(&m, NO_RESET_CSV).expect("parse");
    let asserted: Vec<usize> = (0..stim.cycles()).filter(|&k| stim.resets[k]).collect();
    assert_eq!(asserted, [3, 6, 7]);
    let plain = Stimulus::parse_csv(&m, "x\n1\n2").expect("parse");
    assert_eq!(plain.resets, [false, false]);
}

#[test]
fn rst_n_column_takes_0_or_1() {
    let m = module_of(NO_RESET);
    let err = Stimulus::parse_csv(&m, "x, rst_n\n1, 2").expect_err("bad value");
    assert!(
        err.to_string()
            .contains("`rst_n` is `0` (reset asserted) or `1`"),
        "{err}"
    );
}

#[test]
fn rst_n_column_needs_a_single_clock() {
    let m = module_of(include_str!("../../../examples/fn_two_clocks.snt"));
    let err = Stimulus::parse_csv(&m, "rst_n\n1").expect_err("multi-clock");
    assert!(
        err.to_string().contains("only in a single-clock design"),
        "{err}"
    );
}

#[test]
fn no_reset_takes_no_arguments() {
    compile_err(
        "struct T { #[no_reset(x)] n: u8 = 0, }
         impl T { #[clock(domain = sys)] fn tick(self) -> (T, u8) { (self, self.n) } }",
        "`#[no_reset]` doesn't take `x`",
    );
}

#[test]
fn no_reset_on_a_channel_is_rejected() {
    compile_err(
        "struct T { #[no_reset] q: fifo<u8, 4>, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self) -> T { T { q: self.q.next(self.q.sender().no_push(), 0) } }
         }",
        "`#[no_reset]` on `q`, which holds a channel",
    );
}

#[test]
fn no_reset_on_a_staged_function_is_rejected() {
    compile_err(
        "pipeline fn p(a: u8) -> u8 { let b: u8 = a; yield; b }
         struct T { #[no_reset] s: p, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self, a: u8) -> (T, u8) {
                 let (s, y, v) = self.s.advance(a, 1);
                 (T { s: s }, y)
             }
         }",
        "which holds the staged function `p`",
    );
}

#[test]
fn no_reset_on_a_nested_channel_is_rejected() {
    compile_err(
        "struct Q { q: fifo<u8, 4>, }
         struct T { #[no_reset] inner: Q, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self) -> T {
                 T { inner: Q { q: self.inner.q.next(self.inner.q.sender().no_push(), 0) } }
             }
         }",
        "which holds a channel (in field `q` of `Q`)",
    );
}

#[test]
fn no_reset_on_an_instance_is_rejected() {
    compile_err(
        "#[module]
         struct D { r: u8 = 0, }
         struct DOut { y: u8, }
         impl D { fn step(self, a: u8) -> (Self, DOut) { (Self { r: a }, DOut { y: self.r }) } }
         struct T { #[no_reset] d: D, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self, a: u8) -> (T, u8) {
                 let (d, o) = self.d.step(a);
                 (T { d: d }, o.y)
             }
         }",
        "which holds the instance `D`",
    );
}

#[test]
fn no_reset_in_a_verilog_model_is_rejected() {
    compile_err(
        "#[verilog(module = \"acc\", src = \"acc.v\")]
         struct A { #[no_reset] s: u8 = 0, }
         struct AOut { y: u8, }
         impl A { fn step(self, a: u8) -> (Self, AOut) { (Self { s: a }, AOut { y: self.s }) } }
         struct T { a: A, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> (T, u8) {
                 let (a, o) = self.a.step(x);
                 (T { a: a }, o.y)
             }
         }",
        "`A` is a `#[verilog]` model, and the real module decides what its reset does",
    );
}

/// The oracle tells a `#[no_reset]` register from a reset one: with the
/// attribute's effect dropped from the Verilog (every register reset), the
/// directed run's mid-run resets make the Verilated trace diverge.
#[test]
fn the_oracle_catches_a_verilog_that_resets_a_no_reset_register() {
    if !oracle::tools().verilator {
        return;
    }
    let m = module_of(NO_RESET);
    let stim = Stimulus::parse_csv(&m, NO_RESET_CSV).expect("parse");
    let reference = run_reference_with_resets(&m, &stim.inputs, &stim.resets);

    let mut wrong = m.clone();
    wrong.regs.iter_mut().for_each(|r| r.no_reset = false);
    let dir = oracle::workdir("reset-wrong-verilog");
    let sv = dir.join("Engine.sv");
    let harness = dir.join("Engine_tb.cpp");
    let stim_path = dir.join("stimulus.bin");
    let rst_path = dir.join("resets.bin");
    std::fs::write(&sv, verilog::emit_module(&wrong)).expect("write sv");
    std::fs::write(&harness, verilator::emit_cpp_harness(&wrong)).expect("write harness");
    std::fs::write(&stim_path, stimulus_to_bytes(&stim.inputs)).expect("write stimulus");
    std::fs::write(&rst_path, resets_to_bytes(&stim.resets)).expect("write resets");
    let dut = oracle::run_verilator_with_resets(
        &dir,
        &sv,
        &[],
        &harness,
        "Engine",
        &stim_path,
        stim.cycles(),
        Some(&rst_path),
    );
    assert_eq!(dut.len(), reference.len());
    assert_ne!(
        dut, reference,
        "resetting the no-reset registers must change the trace"
    );
}

/// A `#[no_reset]` field inside a `#[module]` child reaches the child's own
/// Verilog module, and the hierarchy co-simulates with mid-run resets.
#[test]
fn a_no_reset_field_inside_a_module_child() {
    let design = compile_str(
        "#[module]
         struct D { #[no_reset] r: u8 = 0, c: u2 = 0, }
         struct DOut { y: u8, }
         impl D {
             fn step(self, a: u8) -> (Self, DOut) {
                 (Self { r: a, c: (self.c + 1) as u2 wrap }, DOut { y: (self.r + self.c) as u8 wrap })
             }
         }
         struct T { d: D, }
         impl T {
             #[clock(domain = sys)]
             fn tick(self, a: u8) -> (T, u8) {
                 let (d, o) = self.d.step(a);
                 (T { d: d }, o.y)
             }
         }",
    )
    .expect("compile");
    let (top, children) = design.modules.split_first().expect("modules");
    let child = &children[0];
    let flags: Vec<_> = child
        .regs
        .iter()
        .map(|r| (r.name.as_str(), r.no_reset))
        .collect();
    assert_eq!(flags, [("r", true), ("c", false)]);
    let flat: Vec<_> = top
        .regs
        .iter()
        .filter(|r| r.no_reset)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(flat, ["d_r"], "the flat simulator sees the child's flag");

    let stimulus = sinter_compiler::sim::gen_stimulus(top, 24, 7);
    let resets: Vec<bool> = (0..24).map(|k| k == 5 || k == 6 || k == 17).collect();
    oracle::co_simulate_with_resets(
        &oracle::tools(),
        "reset-hier",
        top,
        children,
        &stimulus,
        &resets,
        oracle::Lint::Strict,
    );
}
