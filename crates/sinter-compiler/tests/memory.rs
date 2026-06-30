//! MEM1 memory tests (DESIGN §6.6/§6.9/§9.6): the lowered shape (one opaque
//! array + a read-data register, never SROA-flattened), the `value()`/`next()`
//! surface, the `Static(1)` READ_FIRST timing, address masking, and the M1
//! rejections. The sim↔Verilog oracle is in `conformance.rs` (the `fn_ram`
//! example).

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

/// Step `fn_ram` (inputs `we`, `waddr`, `wdata`, `raddr`), returning `out`.
fn ram_step(sim: &mut Simulator, we: u64, waddr: u64, wdata: u64, raddr: u64) -> u64 {
    sim.step(
        &[
            ("we".to_string(), we),
            ("waddr".to_string(), waddr),
            ("wdata".to_string(), wdata),
            ("raddr".to_string(), raddr),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    )["out"]
}

#[test]
fn ram_lowers_to_one_opaque_array_plus_read_register() {
    let design = compile_str(include_str!("../../../examples/fn_ram.snt")).expect("compile");
    let m = &design.modules[0];
    // The array is ONE opaque memory (not 16 SROA-flattened registers).
    assert_eq!(m.mems.len(), 1, "one memory");
    let mem = &m.mems[0];
    assert_eq!(mem.name, "data");
    assert_eq!(mem.depth, 16);
    assert_eq!(mem.addr_bits, 4, "log2(16)");
    assert_eq!(mem.elem_ty.width(), 8);
    assert_eq!(mem.reset, vec![0u64; 16], "zero-initialized");
    // The only register is the memory-owned read-data register (reset 0).
    assert_eq!(m.regs.len(), 1, "only the read-data register: {:?}", m.regs);
    assert_eq!(m.regs[0].name, "data_rdata");
    assert_eq!(m.regs[0].reset, 0);
    // One read port (drives the read-data reg) and one write port, both derived
    // from the threaded `next()`.
    assert_eq!(m.mem_reads.len(), 1);
    assert_eq!(m.mem_reads[0].rdata, "data_rdata");
    assert_eq!(m.mem_writes.len(), 1);
    // The read-data register is driven by the read, not a normal reg_update.
    assert!(!m.reg_updates.iter().any(|(n, _)| n == "data_rdata"));
}

#[test]
fn read_has_static1_latency_and_read_first() {
    let design = compile_str(include_str!("../../../examples/fn_ram.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    // Write 99 to addr 5.
    assert_eq!(
        ram_step(&mut sim, 1, 5, 99, 0),
        0,
        "fill: read-data still 0"
    );
    // Same cycle: write 77 to addr 5 AND read addr 5 -> the read sees the OLD 99
    // (READ_FIRST), surfacing one cycle later.
    assert_eq!(
        ram_step(&mut sim, 1, 5, 77, 5),
        0,
        "read-data is last read (0)"
    );
    assert_eq!(
        ram_step(&mut sim, 0, 0, 0, 5),
        99,
        "READ_FIRST: old value 99"
    );
    // The 77 written that same cycle is now visible on a fresh read.
    assert_eq!(
        ram_step(&mut sim, 0, 0, 0, 5),
        77,
        "new value 77 after the write"
    );
}

#[test]
fn wider_address_is_masked_to_the_depth() {
    // A `u8` address into a depth-4 memory is masked to 2 bits: 6 -> 2, 10 -> 2.
    let src = "
        struct M { d: mem<u8, 4> = 0 }
        impl M {
            #[clock(domain = sys)]
            fn tick(self, we: u1, waddr: u8, wdata: u8, raddr: u8) -> (M, u8) {
                let rd = self.d.value();
                (M { d: self.d.next(raddr, we, waddr, wdata) }, rd)
            }
        }
    ";
    let design = compile_str(src).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);
    let step = |sim: &mut Simulator, we, waddr, wdata, raddr| {
        sim.step(
            &[
                ("we".to_string(), we),
                ("waddr".to_string(), waddr),
                ("wdata".to_string(), wdata),
                ("raddr".to_string(), raddr),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        )["out"]
    };
    step(&mut sim, 1, 6, 50, 0); // write 50 to addr 6 (== index 2)
    step(&mut sim, 0, 0, 0, 10); // read addr 10 (== index 2)
    assert_eq!(
        step(&mut sim, 0, 0, 0, 0),
        50,
        "addr 6 and 10 alias to index 2"
    );
}

#[test]
fn discarded_next_commits_no_effect() {
    // The transition is committed only by threading `next()`'s result into `Self`.
    // A `next()` whose result is dropped (here `self.d` is threaded unchanged)
    // commits no write and schedules no read — the threading is load-bearing.
    let design = compile_str(
        "struct M { d: mem<u8, 4> = 0 } \
         impl M { #[clock(domain = sys)] fn tick(self, a: u2, v: u8) -> M { \
            let _drop = self.d.next(a, 1, a, v); M { d: self.d } } }",
    )
    .expect("compile");
    let m = &design.modules[0];
    assert!(
        m.mem_writes.is_empty(),
        "a discarded next() must commit no write"
    );
    assert!(
        m.mem_reads.is_empty(),
        "a discarded next() schedules no read"
    );
}

#[test]
fn value_is_a_pure_read_usable_more_than_once() {
    // `value()` is a pure accessor (it does not consume the single read port), so
    // it may be read any number of times.
    let src = "
        struct M { d: mem<u8, 4> = 0 }
        impl M {
            #[clock(domain = sys)]
            fn tick(self, a: u2) -> (M, u9) {
                let x = self.d.value();
                let y = self.d.value();
                (M { d: self.d.next(a, 0, 0, 0) }, (x + y) as u9 wrap)
            }
        }
    ";
    compile_str(src).expect("value() may be read multiple times");
}

// --- MEM1 rejections -------------------------------------------------------

#[test]
fn non_power_of_two_depth_is_rejected() {
    compile_err("struct S { m: mem<u8, 3> = 0 }", "power of two");
}

#[test]
fn oversized_element_is_rejected() {
    compile_err("struct S { m: mem<u65, 4> = 0 }", "64-bit limit");
}

#[test]
fn memory_as_a_function_parameter_is_rejected() {
    compile_err(
        "fn f(m: mem<u8, 4>) -> u8 { 0 }",
        "can only be a struct field",
    );
}

#[test]
fn memory_nested_in_an_aggregate_field_is_rejected() {
    compile_err("struct S { ms: [mem<u8, 4>; 2] }", "plain struct field");
}

#[test]
fn non_zero_memory_init_is_rejected() {
    compile_err("struct S { m: mem<u8, 4> = 5 }", "zero-initialized");
    compile_err("struct S { m: mem<u8, 4> }", "needs an explicit `= 0`");
}

#[test]
fn memory_as_a_return_type_parses_then_is_rejected_semantically() {
    // The `>` of `mem<…>` must not be swallowed by the expression grammar: a
    // memory return type PARSES (then the checker rejects it cleanly).
    compile_err(
        "fn f(a: u8) -> mem<u8, 4> { 0 }",
        "can only be a struct field",
    );
}

#[test]
fn conditional_next_is_rejected() {
    // `next()` returns a handle that is threaded into the next-state, so it cannot
    // be selected by an `if`/`match` — gate the write with `we` instead.
    compile_err(
        "struct R { d: mem<u8, 4> = 0 } \
         impl R { #[clock(domain = sys)] fn tick(self, c: u1, a: u2, v: u8) -> R { \
            let d = if c == 1 { self.d.next(a, 1, a, v) } else { self.d }; R { d: d } } }",
        "must be unconditional",
    );
}

#[test]
fn read_data_register_colliding_with_an_input_is_rejected() {
    // The synthesized `<mem>_rdata` register must not clash with an input port.
    compile_err(
        "struct R { data: mem<u8, 4> = 0 } \
         impl R { #[clock(domain = sys)] fn tick(self, data_rdata: u8, a: u2) -> (R, u8) { \
            let rd = self.data.value(); (R { data: self.data.next(a, 0, 0, 0) }, rd) } }",
        "collides",
    );
}

#[test]
fn the_same_memory_through_both_if_branches_compiles() {
    // Selecting the *same* memory in both arms is a no-op (the `mux` mem arm
    // returns the handle), so a conditional pass-through type-checks and lowers.
    let src = "
        struct R { d: mem<u8, 4> = 0 }
        impl R {
            #[clock(domain = sys)]
            fn tick(self, c: u1, a: u2) -> (R, u8) {
                let rd = self.d.value();
                (R { d: if c == 1 { self.d } else { self.d } }, rd)
            }
        }
    ";
    let design = compile_str(src).expect("same-memory if-passthrough should compile");
    assert_eq!(design.modules[0].mems.len(), 1);
}

#[test]
fn nested_struct_memory_with_explicit_zero_default_compiles() {
    // A nested struct whose `mem` field is zero-initialized through an explicit
    // outer struct-literal default (`= Inner { data: 0 }`) is accepted, and the
    // memory flattens with the nested path.
    let src = "
        struct Inner { data: mem<u8, 4> = 0 }
        struct Outer { inner: Inner = Inner { data: 0 } }
        impl Outer {
            #[clock(domain = sys)]
            fn tick(self, a: u2) -> (Outer, u8) {
                let rd = self.inner.data.value();
                (Outer { inner: Inner { data: self.inner.data.next(a, 0, 0, 0) } }, rd)
            }
        }
    ";
    let design = compile_str(src).expect("nested-struct memory should compile");
    let m = &design.modules[0];
    assert_eq!(m.mems.len(), 1);
    assert_eq!(m.mems[0].name, "inner_data", "flattened nested mem path");
}

#[test]
fn memory_array_is_zero_initialized_and_write_is_a_separate_block() {
    // The array is zero-initialized (the sim/Verilator reset contract) and the
    // write lives in its own `always_ff` (kept out of the async-reset block so
    // BRAM inference survives; the array itself has no content reset).
    let design = compile_str(include_str!("../../../examples/fn_ram.snt")).expect("compile");
    let sv = sinter_compiler::verilog::emit_module(&design.modules[0]);
    assert!(sv.contains("initial begin"), "array zero-init:\n{sv}");
    assert!(
        sv.contains("// Memory write port: data"),
        "separate write block:\n{sv}"
    );
}
