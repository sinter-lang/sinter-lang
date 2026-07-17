//! Channel tests — the `fifo` / `ring` / `flow` family (DESIGN §6.11 / §9.8).
//!
//! Covers the desugared shape (a register-backed shift register, NOT an opaque
//! `mem`), the `front()` / `next(push, pop)` behavior — enqueue, pop-from-front,
//! same-cycle push+pop stream-through — and the rejections. The load-bearing
//! distinction is the loss policy, which lives in the type:
//!
//! * `fifo` (CHAN1) — a push into a full queue is a guarded hold (refused).
//! * `ring` (CHAN2) — the same push drops the oldest word and lands.
//! * `flow` (CHAN2) — depth-1 newest-wins, `value()`, no pop.
//!
//! The sim↔Verilog oracle is in `conformance.rs` (`fn_fifo` / `fn_ring` /
//! `fn_flow`).

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

/// Step `fn_ring` with `push` (an `Option<u8>`) and `take` (pop flag). Returns
/// the observed `front()` (this cycle, pre-transition) and the occupancy count.
fn ring_step(sim: &mut Simulator, push: Option<u64>, take: u64) -> (Option<u64>, u64) {
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
    let front = if out["out_0_is_some"] == 1 {
        Some(out["out_0_payload"])
    } else {
        None
    };
    (front, out["r_count_q"])
}

#[test]
fn ring_lowers_to_the_same_flops_as_a_fifo() {
    let design = compile_str(include_str!("../../../examples/fn_ring.snt")).expect("compile");
    let m = &design.modules[0];
    // Same register-backed shift register as the `fifo` — only the loss policy
    // differs, so `ring` costs no extra state.
    assert_eq!(m.mems.len(), 0, "register-backed, no opaque memory");
    assert_eq!(m.regs.len(), 5, "4 data slots + 1 count: {:?}", m.regs);
    assert!(m.regs.iter().any(|r| r.name == "r_count"));
    assert!(m.regs.iter().any(|r| r.name == "r_data_0"));
    assert!(m.regs.iter().any(|r| r.name == "r_data_3"));
}

#[test]
fn ring_push_into_full_drops_the_oldest() {
    let design = compile_str(include_str!("../../../examples/fn_ring.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Fill to capacity 4 -> [1, 2, 3, 4].
    ring_step(&mut sim, Some(1), 0);
    ring_step(&mut sim, Some(2), 0);
    ring_step(&mut sim, Some(3), 0);
    ring_step(&mut sim, Some(4), 0);
    assert_eq!(ring_step(&mut sim, None, 0), (Some(1), 4), "full");

    // Push while full with no pop: unlike a `fifo` (which refuses the word), the
    // ring accepts it and drops the OLDEST -> [2, 3, 4, 99], count still 4.
    assert_eq!(ring_step(&mut sim, Some(99), 0), (Some(1), 4), "1 dropped");
    // The head has advanced to 2 — proof the oldest went, not the newest.
    assert_eq!(ring_step(&mut sim, Some(88), 0), (Some(2), 4), "2 dropped");
    // Now [3, 4, 99, 88]. Drain: the two survivors then the two newest words.
    assert_eq!(ring_step(&mut sim, None, 1), (Some(3), 4));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(4), 3));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(99), 2));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(88), 1));
    assert_eq!(ring_step(&mut sim, None, 0), (None, 0), "drained");
}

#[test]
fn ring_below_capacity_behaves_exactly_like_a_fifo() {
    let design = compile_str(include_str!("../../../examples/fn_ring.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Nothing is dropped while there is room: the lossy path is reached only by a
    // push into a *full* ring.
    assert_eq!(ring_step(&mut sim, Some(10), 0), (None, 0), "empty");
    assert_eq!(ring_step(&mut sim, Some(20), 0), (Some(10), 1));
    assert_eq!(ring_step(&mut sim, Some(30), 0), (Some(10), 2));
    // No push, no pop -> hold.
    assert_eq!(ring_step(&mut sim, None, 0), (Some(10), 3));
    // Same-cycle push+pop: dequeue 10, enqueue 40 -> [20, 30, 40] (READ_FIRST:
    // the reported head is the OLD 10), count stays 3.
    assert_eq!(ring_step(&mut sim, Some(40), 1), (Some(10), 3));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(20), 3));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(30), 2));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(40), 1));
    // Empty again; a pop-when-empty is inert.
    assert_eq!(ring_step(&mut sim, None, 1), (None, 0));
    assert_eq!(ring_step(&mut sim, None, 1), (None, 0));
}

#[test]
fn ring_full_push_with_pop_streams_through_without_dropping() {
    let design = compile_str(include_str!("../../../examples/fn_ring.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Fill to capacity 4 -> [1, 2, 3, 4].
    for v in 1..=4 {
        ring_step(&mut sim, Some(v), 0);
    }
    // Full + push + pop: the pop already retires the head, so the ring has room
    // and nothing is *dropped* — the head is delivered to the consumer, not lost.
    assert_eq!(ring_step(&mut sim, Some(5), 1), (Some(1), 4), "1 popped");
    assert_eq!(ring_step(&mut sim, Some(6), 1), (Some(2), 4), "2 popped");
    // [3, 4, 5, 6] — every word accounted for.
    assert_eq!(ring_step(&mut sim, None, 1), (Some(3), 4));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(4), 3));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(5), 2));
    assert_eq!(ring_step(&mut sim, None, 1), (Some(6), 1));
}

/// Step `fn_flow` with `push` (an `Option<u8>`); returns the observed `value()`
/// (this cycle, pre-transition).
fn flow_step(sim: &mut Simulator, push: Option<u64>) -> Option<u64> {
    let (is_some, payload) = match push {
        Some(v) => (1, v),
        None => (0, 0),
    };
    let out = sim.step(
        &[
            ("incoming_is_some".to_string(), is_some),
            ("incoming_payload".to_string(), payload),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>(),
    );
    if out["out_is_some"] == 1 {
        Some(out["out_payload"])
    } else {
        None
    }
}

#[test]
fn flow_erases_to_an_option_register() {
    let design = compile_str(include_str!("../../../examples/fn_flow.snt")).expect("compile");
    let m = &design.modules[0];
    // A depth-1 slot is exactly an `Option<u8>` register: a valid bit + payload,
    // no count and no data array (DESIGN §6.11).
    assert_eq!(m.mems.len(), 0);
    assert_eq!(m.regs.len(), 2, "valid bit + payload: {:?}", m.regs);
    assert!(m.regs.iter().any(|r| r.name == "latest_v_is_some"));
    assert!(m.regs.iter().any(|r| r.name == "latest_v_payload"));
}

#[test]
fn flow_is_newest_wins_and_holds_without_a_push() {
    let design = compile_str(include_str!("../../../examples/fn_flow.snt")).expect("compile");
    let mut sim = Simulator::new(&design.modules[0]);

    // Resets empty.
    assert_eq!(flow_step(&mut sim, Some(7)), None, "empty at reset");
    assert_eq!(flow_step(&mut sim, None), Some(7));
    // No push -> the sample holds (it is a "latest value" register, and reading
    // it does not consume it — there is no pop).
    assert_eq!(flow_step(&mut sim, None), Some(7), "held");
    assert_eq!(flow_step(&mut sim, None), Some(7), "still held");
    // A push always wins, overwriting the live sample: stale data is dropped by
    // design, which is the whole point of `flow`.
    assert_eq!(flow_step(&mut sim, Some(9)), Some(7), "7 still visible");
    assert_eq!(flow_step(&mut sim, Some(11)), Some(9), "9 overwrote 7");
    assert_eq!(flow_step(&mut sim, None), Some(11), "11 overwrote 9");
    assert_eq!(flow_step(&mut sim, None), Some(11));
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

#[test]
fn ring_capacity_must_be_a_power_of_two() {
    compile_err(
        "struct Q { r: ring<u8, 3> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { r: self.r.next(None, 0) } } }",
        "power of two",
    );
}

#[test]
fn ring_field_takes_no_initializer() {
    compile_err(
        "struct Q { r: ring<u8, 4> = 0 }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { r: self.r.next(None, 0) } } }",
        "no initializer",
    );
}

#[test]
fn flow_field_takes_no_initializer() {
    compile_err(
        "struct Q { f: flow<u8> = 0 }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { f: self.f.next(None) } } }",
        "no initializer",
    );
}

/// `flow<T>` has no pop, so it has no `front()` either — `value()` is the read
/// (DESIGN §6.11). Getting the surface wrong is a type error, not a silent
/// mis-lowering.
#[test]
fn flow_has_no_pop_or_front() {
    compile_err(
        "struct Q { f: flow<u8> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> (Q, Option<u8>) \
         { (Q { f: self.f.next(None) }, self.f.front()) } }",
        "front",
    );
    compile_err(
        "struct Q { f: flow<u8> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { f: self.f.next(None, 0) } } }",
        "next",
    );
}

/// The element type carries the loss policy, so the diagnostics must name the
/// kind the user actually wrote rather than a generic "channel".
#[test]
fn errors_name_the_channel_kind() {
    compile_err(
        "struct Q { r: ring<(u8, u8), 4> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { r: self.r.next(None, 0) } } }",
        "a `ring` element must be a scalar",
    );
    compile_err(
        "struct Q { f: flow<(u8, u8)> }\n\
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { f: self.f.next(None) } } }",
        "a `flow` element must be a scalar",
    );
}
