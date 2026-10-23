//! FLT1b silicon fault-latch tests (DESIGN §6.12 / §9.9): the `--faults=on`
//! pass synthesizes, from the FLT0 assert sites, a sticky one-hot bitmap register
//! and a priority-encoded `fault_code` sideband — with `--faults=off` provably
//! functional-neutral (byte-identical SV). The sim↔Verilog oracle is in
//! `conformance.rs` (the `fn_fault` example, co-simulated in on-mode).

use std::collections::HashMap;

use sinter_compiler::fault_latch::fault_table_json;
use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_str, compile_str_faults, verilog, FaultMode};

const SRC: &str = include_str!("../../../examples/fn_fault.snt");

/// Step `fn_fault` with inputs `a`/`b`, returning the observed `fault_code`.
fn step(sim: &mut Simulator, a: u64, b: u64) -> u64 {
    let out = sim.step(
        &[("a".to_string(), a), ("b".to_string(), b)]
            .into_iter()
            .collect::<HashMap<_, _>>(),
    );
    out["fault_code"]
}

#[test]
fn on_mode_adds_the_bitmap_register_and_code_sideband() {
    let design = compile_str_faults(SRC, FaultMode::On).expect("compile on");
    let m = &design.modules[0];
    assert!(
        m.regs.iter().any(|r| r.name == "__fault_bits"),
        "on-mode adds the sticky bitmap register: {:?}",
        m.regs
    );
    let code = m
        .outputs()
        .find(|p| p.name == "fault_code")
        .expect("on-mode adds the fault_code sideband output");
    // Two sites -> codes 0..=2 -> 2-bit code.
    assert_eq!(code.ty, sinter_syntax::ast::Type::Uint(2));
}

#[test]
fn off_mode_is_byte_identical_to_the_assert_free_module() {
    // `--faults=off` must emit exactly what the module would without any asserts —
    // the functional-neutral / compile-out guarantee (DESIGN §6.12).
    let off = verilog::emit_module(&compile_str(SRC).expect("compile off").modules[0]);
    let bare = "\
struct Mon {
    pub last: u8 = 0,
}
impl Mon {
    #[clock(domain = sys)]
    fn tick(self, a: u8, b: u8) -> (Mon, u8) {
        (Mon { last: a }, (a + b) as u8 wrap)
    }
}
";
    let bare_sv = verilog::emit_module(&compile_str(bare).expect("compile bare").modules[0]);
    assert_eq!(
        off, bare_sv,
        "`--faults=off` must be byte-identical to the assert-free module"
    );
    // And `fault_code` must NOT appear in off-mode.
    assert!(
        !off.contains("fault_code"),
        "off-mode must not emit the fault sideband"
    );
}

#[test]
fn fault_bits_latch_sticky_and_code_is_priority_encoded() {
    let design = compile_str_faults(SRC, FaultMode::On).expect("compile on");
    let mut sim = Simulator::new(&design.modules[0]);

    // Cycle 0: both checks hold (a<200, b<128); nothing latched yet -> code 0.
    // (fault_code reads the REGISTERED bitmap, so it lags a cycle.)
    assert_eq!(step(&mut sim, 10, 10), 0, "no fault at reset");
    // Fire only site 2 (b high bit set) this cycle: b >= 128, a still < 200.
    assert_eq!(
        step(&mut sim, 10, 200),
        0,
        "code still 0 the cycle it fires"
    );
    // Now bit 1 is latched -> code 2 (site 2). Inputs clean this cycle.
    assert_eq!(step(&mut sim, 10, 10), 2, "site 2 latched -> code 2");
    // Fire site 1 (a >= 200). Bit 0 latches; even with b clean, code 1 wins
    // (lowest-code priority) from next cycle on.
    assert_eq!(step(&mut sim, 250, 10), 2, "code 2 the cycle site 1 fires");
    assert_eq!(step(&mut sim, 10, 10), 1, "site 1 (lower code) now wins");
    // Sticky: even with all inputs clean, both bits stay set, code stays 1.
    assert_eq!(step(&mut sim, 0, 0), 1, "sticky: fault holds");
    assert_eq!(step(&mut sim, 0, 0), 1, "still sticky");
}

#[test]
fn a_multiply_in_a_fault_condition_is_rejected_on_mode() {
    // FLT1b does not lift a multiply into the fault channel (it bypasses the
    // datapath multiply-CSE and risks a Verilator WIDTHEXPAND lint). Under
    // `--faults=on` such a condition is a clear compile error; the SAME source is
    // fine sim-only (`--faults=off`), so the rejection is on-mode only.
    let src = "\
struct M {
    pub v: u8 = 0,
}
impl M {
    #[clock(domain = sys)]
    fn tick(self, a: u8, b: u8) -> (M, u8) {
        assert!(a * b < 100, \"product overflow\");
        (M { v: a }, self.v)
    }
}
";
    // off-mode: sim-only, compiles fine.
    assert!(
        compile_str(src).is_ok(),
        "a multiply in an assert cond is fine sim-only"
    );
    // on-mode: rejected with a scope note.
    match compile_str_faults(src, FaultMode::On) {
        Ok(_) => panic!("expected a rejection of `*` in a fault condition under --faults=on"),
        Err(e) => {
            let msg = e.to_string();
            assert!(msg.contains('*') && msg.contains("faults=on"), "got: {msg}");
        }
    }
}

#[test]
fn fault_table_maps_codes_to_backtraces() {
    let design = compile_str_faults(SRC, FaultMode::On).expect("compile on");
    let table = fault_table_json(&design.modules[0]);
    // Two entries, codes 1 and 2, carrying the messages and backtraces.
    assert!(table.contains("\"code\": 1"), "table has code 1:\n{table}");
    assert!(table.contains("\"code\": 2"), "table has code 2:\n{table}");
    assert!(
        table.contains("a exceeded threshold 200"),
        "table carries site 1's message:\n{table}"
    );
    assert!(
        table.contains("b high bit set"),
        "table carries site 2's message:\n{table}"
    );
    // The backtrace field is present (the root frame at minimum).
    assert!(
        table.contains("\"backtrace\""),
        "table has backtraces:\n{table}"
    );
}
