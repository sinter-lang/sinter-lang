//! CHAN1 `fifo<T, N>` tests (DESIGN §6.11 / §9.8): the desugared shape (a
//! register-backed shift register, NOT an opaque `mem`), the `front()` /
//! `next(push, pop)` behavior — enqueue, guarded-hold on full (losslessness),
//! pop-from-front, same-cycle push+pop stream-through — and the CHAN1
//! rejections. The sim↔Verilog oracle is in `conformance.rs` (the `fn_fifo`
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

/// Step `fn_fifo` with `push` (an `Option<u8>`) and `take` (pop flag). Returns
/// the observed `front()` (this cycle, pre-transition) and the occupancy count.
fn fifo_step(sim: &mut Simulator, push: Option<u64>, take: u64) -> (Option<u64>, u64) {
    let (is_some, payload) = match push {
        Some(v) => (1, v),
        None => (0, 0),
    };
    let out = sim.step(
        &[
            ("incoming_is_some".to_string(), is_some),
            ("incoming_payload".to_string(), payload),
            ("take".to_string(), take),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    );
    let front = if out["out_is_some"] == 1 {
        Some(out["out_payload"])
    } else {
        None
    };
    (front, out["q_count_q"])
}

#[test]
fn fifo_lowers_to_flops_not_an_opaque_memory() {
    let design = compile_str(include_str!("../../../examples/fn_fifo.snt")).expect("compile");
    let m = &design.modules[0];
    // A shallow fifo is SROA-flattened to registers (4 data slots + count), NOT
    // held as an opaque `mem` array (that is the deep CHAN4 tier).
    assert_eq!(m.mems.len(), 0, "register-backed, no opaque memory");
    assert_eq!(m.regs.len(), 5, "4 data slots + 1 count: {:?}", m.regs);
    assert!(m.regs.iter().any(|r| r.name == "q_count"));
    assert!(m.regs.iter().any(|r| r.name == "q_data_0"));
    assert!(m.regs.iter().any(|r| r.name == "q_data_3"));
}

#[test]
fn enqueue_dequeue_and_stream_through() {
    let design = compile_str(include_str!("../../../examples/fn_fifo.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Each step reports the head/count of the *current* (pre-transition) state.
    // Fill 10, 20, 30 (no pop).
    assert_eq!(fifo_step(&mut sim, Some(10), 0), (None, 0), "empty");
    assert_eq!(fifo_step(&mut sim, Some(20), 0), (Some(10), 1));
    assert_eq!(fifo_step(&mut sim, Some(30), 0), (Some(10), 2));
    // No push, no pop -> guarded hold (nothing changes).
    assert_eq!(fifo_step(&mut sim, None, 0), (Some(10), 3));
    // Same-cycle push+pop: dequeue 10, enqueue 40 -> [20, 30, 40] (READ_FIRST:
    // the reported head is the OLD 10), count stays 3.
    assert_eq!(fifo_step(&mut sim, Some(40), 1), (Some(10), 3));
    // Drain in order: 20, 30, 40.
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(20), 3));
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(30), 2));
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(40), 1));
    // Empty again; a pop-when-empty is inert.
    assert_eq!(fifo_step(&mut sim, None, 1), (None, 0));
    assert_eq!(fifo_step(&mut sim, None, 1), (None, 0));
}

#[test]
fn push_into_full_is_a_lossless_guarded_hold() {
    let design = compile_str(include_str!("../../../examples/fn_fifo.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Fill to capacity 4.
    fifo_step(&mut sim, Some(1), 0);
    fifo_step(&mut sim, Some(2), 0);
    fifo_step(&mut sim, Some(3), 0);
    fifo_step(&mut sim, Some(4), 0);
    // Now full (count 4), head is 1.
    assert_eq!(fifo_step(&mut sim, None, 0), (Some(1), 4), "full");
    // Push while full with no pop: the word is refused (never overwrites the
    // newest), the queue holds — count stays 4, head still 1, no data lost.
    assert_eq!(
        fifo_step(&mut sim, Some(99), 0),
        (Some(1), 4),
        "push refused"
    );
    assert_eq!(
        fifo_step(&mut sim, Some(88), 0),
        (Some(1), 4),
        "still refused"
    );
    // Draining still yields the original 1, 2, 3, 4 — nothing was clobbered.
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(1), 4));
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(2), 3));
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(3), 2));
    assert_eq!(fifo_step(&mut sim, None, 1), (Some(4), 1));
    assert_eq!(fifo_step(&mut sim, None, 0), (None, 0));
}

#[test]
fn fifo_capacity_must_be_a_power_of_two() {
    compile_err(
        "struct Q { q: fifo<u8, 3> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { q: self.q.next(None, 0) } } }",
        "power of two",
    );
}

#[test]
fn fifo_field_takes_no_initializer() {
    compile_err(
        "struct Q { q: fifo<u8, 4> = 0 }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { q: self.q.next(None, 0) } } }",
        "no initializer",
    );
}
