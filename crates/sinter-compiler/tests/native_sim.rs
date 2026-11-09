//! The compiled simulator behind `sinter sim` (`native_sim.rs`): the
//! generated crate's runner, built by cargo and streamed, must match the
//! interpreter step for step — the edge time, the ticking domains, the
//! outputs, every probed input, register, and wire, and the assert failures.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sinter_compiler::native_sim::{self, Step};
use sinter_compiler::sim::{gen_stimulus, Simulator};
use sinter_compiler::{compile_project, compile_str, rust_sim, Module};

fn example(rel: &str) -> Module {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(rel);
    let design = compile_project(&path).expect("example compiles");
    design.modules.into_iter().next().expect("a module")
}

fn workdir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("native-sim")
        .join(name)
}

/// One step as the interpreter sees it, in [`Step`]'s layout.
struct Expected {
    time: u64,
    ticking: Vec<bool>,
    outputs: Vec<u64>,
    probe: Vec<u64>,
}

fn interpret(module: &Module, rows: &[Vec<u64>], resets: &[bool]) -> Vec<Expected> {
    let mut sim = Simulator::new(module);
    let inputs: Vec<&str> = module.inputs().map(|p| p.name.as_str()).collect();
    let names: Vec<&str> = inputs
        .iter()
        .copied()
        .chain(module.regs.iter().map(|r| r.name.as_str()))
        .chain(module.lets.iter().map(|l| l.name.as_str()))
        .collect();
    rows.iter()
        .enumerate()
        .map(|(k, row)| {
            let map: HashMap<String, u64> = inputs
                .iter()
                .map(|n| n.to_string())
                .zip(row.iter().copied())
                .collect();
            let out = if resets.get(k).copied().unwrap_or(false) {
                sim.step_in_reset(&map)
            } else {
                sim.step(&map)
            };
            Expected {
                time: sim.time(),
                ticking: sim.ticking().to_vec(),
                outputs: module
                    .outputs()
                    .map(|p| out.get(&p.name).copied().unwrap_or(0))
                    .collect(),
                probe: names.iter().map(|n| sim.signal(n).unwrap_or(0)).collect(),
            }
        })
        .collect()
}

fn check(name: &str, module: &Module, rows: &[Vec<u64>], resets: &[bool]) {
    let dir = workdir(name);
    let expected = interpret(module, rows, resets);
    for probe in [false, true] {
        let exe = native_sim::build(&dir, module, probe).expect("build the simulator");
        let mut seen = 0;
        let failures = native_sim::run(&exe, &dir, module, rows, resets, probe, |s: &Step| {
            let e = &expected[s.index];
            assert_eq!(s.index, seen, "{name}: steps arrive in order");
            seen += 1;
            assert_eq!(s.time, e.time, "{name} step {}: time", s.index);
            assert_eq!(s.ticking, e.ticking.as_slice(), "{name} step {}", s.index);
            assert_eq!(s.outputs, e.outputs.as_slice(), "{name} step {}", s.index);
            if probe {
                assert_eq!(s.probe.len(), rust_sim::probe_len(module));
                for (k, (got, want)) in s.probe.iter().zip(&e.probe).enumerate() {
                    assert_eq!(got, want, "{name} step {}: probe word {k}", s.index);
                }
            } else {
                assert!(s.probe.is_empty());
            }
            Ok(())
        })
        .expect("run the simulator");
        assert_eq!(seen, rows.len(), "{name}: every step");
        let mut sim = Simulator::new(module);
        for (k, row) in rows.iter().enumerate() {
            let map: HashMap<String, u64> = module
                .inputs()
                .map(|p| p.name.clone())
                .zip(row.iter().copied())
                .collect();
            if resets.get(k).copied().unwrap_or(false) {
                sim.step_in_reset(&map);
            } else {
                sim.step(&map);
            }
        }
        assert_eq!(failures, sim.assert_failures(), "{name}: assert failures");
    }
}

#[test]
fn compiled_simulator_matches_the_interpreter() {
    for (name, rel, cycles) in [
        ("pipe", "rv32/pipe.snt", 400),
        ("two_clocks", "fn_two_clocks.snt", 60),
        ("async_fifo", "fn_async_fifo.snt", 200),
        ("mem2", "fn_mem2.snt", 100),
        ("hier", "fn_hier.snt", 60),
    ] {
        let module = example(rel);
        let rows = gen_stimulus(&module, cycles, 0x5eed);
        check(name, &module, &rows, &[]);
    }

    // A reset schedule (RST1), with `#[no_reset]` state.
    let module = example("fn_no_reset.snt");
    let rows = gen_stimulus(&module, 40, 7);
    let resets: Vec<bool> = (0..40).map(|k| k % 13 == 3 || k == 4).collect();
    check("no_reset", &module, &rows, &resets);
}

#[test]
fn compiled_simulator_reports_assert_failures() {
    let design = compile_str(
        "struct C { pub value: u4 = 0, }
         impl C {
             #[clock]
             fn tick(self, en: u1) -> C {
                 assert!(self.value != 5, \"value hit five\");
                 C { value: if en == 1 { (self.value + 1) as u4 wrap } else { self.value } }
             }
         }",
    )
    .expect("compiles");
    let module = &design.modules[0];
    let rows: Vec<Vec<u64>> = (0..50).map(|_| vec![1]).collect();
    check("asserts", module, &rows, &[]);
    let dir = workdir("asserts");
    let exe = native_sim::build(&dir, module, false).expect("build");
    let failures = native_sim::run(&exe, &dir, module, &rows, &[], false, |_| Ok(())).expect("run");
    let cycles: Vec<usize> = failures.iter().map(|f| f.cycle).collect();
    assert_eq!(cycles, [5, 21, 37]);
    assert_eq!(failures[0].msg, "value hit five");
}

/// A failing callback stops the run with its own error.
#[test]
fn a_failing_callback_stops_the_run() {
    let module = example("fn_counter.snt");
    let dir = workdir("stop");
    let exe = native_sim::build(&dir, &module, false).expect("build");
    let rows = gen_stimulus(&module, 100_000, 1);
    let err = native_sim::run(&exe, &dir, &module, &rows, &[], false, |s| {
        if s.index == 10 {
            Err(std::io::Error::other("enough"))
        } else {
            Ok(())
        }
    })
    .expect_err("the callback stops it");
    assert!(err.to_string().contains("enough"), "{err}");
}
