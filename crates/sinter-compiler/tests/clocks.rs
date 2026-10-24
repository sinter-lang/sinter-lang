//! CLK1: several clock domains in one design (DESIGN §6.7 / §9.14). The
//! domain rule, the multi-clock edge scheduler, and the multi-clock module
//! interface. The sim-vs-Verilog check is in `conformance.rs`
//! (`fn_two_clocks`).

use std::collections::HashMap;

use sinter_compiler::sim::Simulator;
use sinter_compiler::{compile_str, compile_str_faults, verilog, FaultMode, Module};

const TWO_CLOCKS: &str = include_str!("../../../examples/fn_two_clocks.snt");

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

/// The two-domain design with `fast_body` / `slow_body` as the ticks' tails.
fn two(fast_body: &str, slow_body: &str) -> String {
    format!(
        "struct T {{
             #[domain(fast)] pub a: u8 = 0,
             #[domain(slow)] pub b: u8 = 0,
         }}
         impl T {{
             #[clock(domain = fast, period = 2)]
             fn tick_fast(self, x: u8) -> T {{ {fast_body} }}
             #[clock(domain = slow, period = 5)]
             fn tick_slow(self, y: u8) -> T {{ {slow_body} }}
         }}"
    )
}

#[test]
fn each_domain_is_recorded_on_its_state_and_ports() {
    let m = module(TWO_CLOCKS);
    let domains: Vec<_> = m
        .domains
        .iter()
        .map(|d| (d.name.as_str(), d.period))
        .collect();
    assert_eq!(domains, [("fast", 3), ("slow", 7)]);
    let reg = |n: &str| m.regs.iter().find(|r| r.name == n).map(|r| r.domain);
    assert_eq!(reg("fast_count"), Some(0));
    assert_eq!(reg("slow_sum"), Some(1));
    let port = |n: &str| m.ports.iter().find(|p| p.name == n).map(|p| p.domain);
    assert_eq!(port("fast_en"), Some(0));
    assert_eq!(port("slow_add"), Some(1));
    assert_eq!(port("slow_sum_q"), Some(1));
    // A tick's extra outputs carry its domain name.
    assert_eq!(port("fast_out"), Some(0));
}

#[test]
fn the_scheduler_steps_from_edge_to_edge() {
    let m = module(TWO_CLOCKS);
    let mut sim = Simulator::new(&m);
    let mut seen = Vec::new();
    for _ in 0..8 {
        sim.step(&HashMap::new());
        let ticks: Vec<&str> = m
            .domains
            .iter()
            .zip(sim.ticking())
            .filter(|(_, &t)| t)
            .map(|(d, _)| d.name.as_str())
            .collect();
        seen.push((sim.time(), ticks.join(",")));
    }
    let expected = [
        (0, "fast,slow"),
        (3, "fast"),
        (6, "fast"),
        (7, "slow"),
        (9, "fast"),
        (12, "fast"),
        (14, "slow"),
        (15, "fast"),
    ];
    let expected: Vec<_> = expected.iter().map(|(t, s)| (*t, s.to_string())).collect();
    assert_eq!(seen, expected);
}

#[test]
fn a_domain_takes_inputs_and_commits_only_at_its_edges() {
    let m = module(TWO_CLOCKS);
    let mut sim = Simulator::new(&m);
    let step = |sim: &mut Simulator, en: u64, add: u64| {
        sim.step(&HashMap::from([
            ("fast_en".to_string(), en),
            ("slow_add".to_string(), add),
        ]))
    };
    step(&mut sim, 1, 5); // t=0: both tick; slow takes 5
    step(&mut sim, 1, 9); // t=3: fast only; slow ignores 9
    step(&mut sim, 1, 9); // t=6: fast only
    let out = step(&mut sim, 0, 2); // t=7: slow only; fast ignores en=0
    assert_eq!(
        out["slow_sum_q"], 5,
        "slow committed only its own edge's input"
    );
    assert_eq!(out["fast_count_q"], 3);
    assert_eq!(sim.signal("fast_en"), Some(1), "fast's input held at 1");
    let out = step(&mut sim, 0, 0); // t=9: fast takes en=0
    assert_eq!(out["slow_sum_q"], 7);
    assert_eq!(out["fast_count_q"], 3);
}

#[test]
fn reading_or_writing_another_domains_state_is_rejected() {
    compile_err(
        &two("T { a: self.b, b: self.b }", "T { a: self.a, b: y }"),
        "`tick_fast` (domain `fast`) reads `b`, which belongs to domain `slow`",
    );
    compile_err(
        &two("T { a: x, b: x }", "T { a: self.a, b: y }"),
        "`tick_fast` (domain `fast`) changes `b`, which belongs to domain `slow`",
    );
    // Laundering a crossing through a helper is still a crossing.
    compile_err(
        &format!(
            "fn peek(t: T) -> u8 {{ t.a }}\n{}",
            two("T { a: x, b: self.b }", "T { a: self.a, b: peek(self) }")
        ),
        "`tick_slow` (domain `slow`) reads `a`, which belongs to domain `fast`",
    );
}

#[test]
fn domain_declarations_are_checked() {
    compile_err(
        "struct T { #[domain(fast)] pub a: u8 = 0, pub b: u8 = 0, }
         impl T {
             #[clock(domain = fast, period = 2)]
             fn tf(self) -> T { T { a: 1, b: self.b } }
             #[clock(domain = slow, period = 3)]
             fn ts(self) -> T { T { a: self.a, b: 2 } }
         }",
        "field `b` has no clock domain",
    );
    compile_err(
        &two("T { a: x, b: self.b }", "T { a: self.a, b: y }")
            .replace("#[domain(slow)]", "#[domain(medium)]"),
        "`#[domain(medium)]`: no `#[clock(domain = medium)]` tick exists",
    );
    compile_err(
        &two("T { a: x, b: self.b }", "T { a: self.a, b: y }").replace(", period = 5", ""),
        "with several clock domains, each `#[clock]` needs a period",
    );
    compile_err(
        &two("T { a: x, b: self.b }", "T { a: self.a, b: y }")
            .replace("domain = slow", "domain = fast"),
        "two `#[clock]` roots for domain `fast`",
    );
    // A single-clock design's tags must name its one domain.
    compile_err(
        "struct S { #[domain(other)] pub v: u8 = 0, }
         impl S { #[clock(domain = sys)] fn tick(self) -> S { S { v: 1 } } }",
        "`#[domain(other)]`: no `#[clock(domain = other)]` tick exists",
    );
}

#[test]
fn a_multi_clock_module_has_a_clock_per_domain_and_reset_synchronizers() {
    let sv = verilog::emit_module(&module(TWO_CLOCKS));
    for line in [
        "    input  wire         clk_fast,  // fast clock (period 3)",
        "    input  wire         clk_slow,  // slow clock (period 7)",
        "    input  wire         rst_n,  // active-low asynchronous reset",
        "    always_ff @(posedge clk_fast or negedge rst_n) begin",
        "            rst_fast_n <= rst_fast_meta;",
        "    always_ff @(posedge clk_slow or negedge rst_slow_n) begin",
    ] {
        assert!(sv.contains(line), "missing `{line}`:\n{sv}");
    }
    assert!(!sv.contains("input  wire         clk,"));
}

#[test]
fn faults_and_pin_maps_reject_several_clocks_for_now() {
    let err = compile_str_faults(TWO_CLOCKS, FaultMode::On)
        .expect_err("multi-clock faults")
        .to_string();
    assert!(
        err.contains("`--faults` doesn't support several clock domains yet"),
        "got: {err}"
    );
    let pins = sinter_compiler::pinmap::PinMap::parse("set_io 1 clock fast\n", "t.pins")
        .expect("pin map parses");
    let err = match sinter_compiler::board::emit_board(&module(TWO_CLOCKS), &pins, "board") {
        Ok(_) => panic!("a multi-clock pin map should be rejected"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("pin maps don't support several clock domains yet"),
        "got: {err}"
    );
}
