//! REG1 runtime-indexed register-file tests (DESIGN §6.14 / §9.11): a `[T; N]`
//! vector-of-registers read at a runtime index `a[i]` (combinational N:1 mux
//! tree, any number of read ports) and written via `a.set(i, v)` (per-element
//! guarded hold). The sim↔Verilog oracle is in `conformance.rs` (`fn_regfile`).

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

/// Step `fn_regfile`; returns the two combinational read-port outputs (this
/// cycle's `regs[rs1]` / `regs[rs2]`, before this cycle's write commits).
fn rf(sim: &mut Simulator, we: u64, rd: u64, wdata: u64, rs1: u64, rs2: u64) -> (u64, u64) {
    let out = sim.step(
        &[
            ("we".to_string(), we),
            ("rd".to_string(), rd),
            ("wdata".to_string(), wdata),
            ("rs1".to_string(), rs1),
            ("rs2".to_string(), rs2),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    );
    (out["out_0"], out["out_1"])
}

#[test]
fn regfile_lowers_to_flops_not_a_memory() {
    let design = compile_str(include_str!("../../../examples/fn_regfile.snt")).expect("compile");
    let m = &design.modules[0];
    // A vector-of-registers is SROA-flattened to flops, NOT an opaque `mem`.
    assert_eq!(m.mems.len(), 0, "register-backed, no block RAM");
    assert_eq!(m.regs.len(), 8, "8 registers: {:?}", m.regs);
    assert!(m.regs.iter().any(|r| r.name == "regs_0"));
    assert!(m.regs.iter().any(|r| r.name == "regs_7"));
}

#[test]
fn two_read_ports_and_one_write_port() {
    let design = compile_str(include_str!("../../../examples/fn_regfile.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // Write reg 3 = 55, then reg 5 = 77 (each commits the next cycle).
    rf(&mut sim, 1, 3, 55, 0, 0);
    rf(&mut sim, 1, 5, 77, 0, 0);
    // Both are now visible; read them on the two ports in the SAME cycle
    // (combinational reads), no write this cycle.
    assert_eq!(rf(&mut sim, 0, 0, 0, 3, 5), (55, 77));
    // Read the same register on both ports at once.
    assert_eq!(rf(&mut sim, 0, 0, 0, 5, 5), (77, 77));
    // A gated write (we = 0) changes nothing.
    rf(&mut sim, 0, 3, 200, 0, 0);
    assert_eq!(rf(&mut sim, 0, 0, 0, 3, 5), (55, 77), "we=0 held both");
}

#[test]
fn register_zero_is_hardwired() {
    let design = compile_str(include_str!("../../../examples/fn_regfile.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // Attempt to write reg 0 (rd = 0): the `rd != 0` enable guard drops it.
    rf(&mut sim, 1, 0, 0xFF, 0, 0);
    rf(&mut sim, 1, 0, 0x42, 0, 0);
    // Reg 0 reads 0 on both ports regardless.
    assert_eq!(rf(&mut sim, 0, 0, 0, 0, 0), (0, 0), "x0 stays zero");
    // A real register still works alongside x0.
    rf(&mut sim, 1, 2, 88, 0, 0);
    assert_eq!(rf(&mut sim, 0, 0, 0, 0, 2), (0, 88));
}

#[test]
fn array_has_only_the_set_method() {
    compile_err(
        "struct S { pub a: [u8; 4] = [0, 0, 0, 0] }\n\
         impl S { #[clock(domain = sys)] fn tick(self, i: u2) -> (S, u8) { (self, self.a.get(i)) } }",
        "no method `get`",
    );
}
