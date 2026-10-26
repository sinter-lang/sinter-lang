//! Program fuzzer (ROADMAP v0.7): random well-typed programs through the
//! whole oracle.
//!
//! Each seed generates a program ([`gen::program`]) that must compile; its
//! interpreter trace must then match its generated Rust simulator, and, when
//! the tools are present, Yosys must synthesize it and Verilator must lint it
//! (warnings about the design's own logic aside) and co-simulate it bit for bit (DESIGN §7.4).
//!
//! A normal test run checks a fixed set of seeds. For a longer campaign, set
//! `SINTER_FUZZ_PROGRAMS` (how many) and `SINTER_FUZZ_SEED` (the first seed),
//! or run `just fuzz <programs> <seed>`. Every program is saved as
//! `target/tmp/fuzz/seed-<seed>.snt`; to rerun one (or a hand-reduced copy)
//! through the whole oracle, set `SINTER_FUZZ_FILE` to its path.

#[path = "fuzz/gen.rs"]
mod gen;
#[path = "support/oracle.rs"]
mod oracle;

use std::path::PathBuf;

use oracle::{co_simulate, rust_sim_traces, tools, Lint};
use sinter_compiler::compile_str;
use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference};

/// Seeds checked by a normal test run.
const DEFAULT_PROGRAMS: u64 = 40;
/// Of those, how many also go through Yosys and Verilator (the slow part).
const DEFAULT_TOOL_PROGRAMS: u64 = 6;
const CYCLES: usize = 64;

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[test]
fn random_programs_pass_the_oracle() {
    if let Ok(file) = std::env::var("SINTER_FUZZ_FILE") {
        let src = std::fs::read_to_string(&file).expect("read SINTER_FUZZ_FILE");
        let design = compile_str(&src).unwrap_or_else(|e| panic!("{file}: {e}"));
        let module = design.modules.into_iter().next().expect("one module");
        let stimulus = gen_stimulus(&module, CYCLES, env_u64("SINTER_FUZZ_SEED").unwrap_or(1));
        co_simulate(&tools(), "fuzz_file", &module, &[], &stimulus, Lint::Design);
        return;
    }
    let first = env_u64("SINTER_FUZZ_SEED").unwrap_or(1);
    let campaign = env_u64("SINTER_FUZZ_PROGRAMS");
    let count = campaign.unwrap_or(DEFAULT_PROGRAMS);
    // A campaign runs every program through the tools.
    let with_tools = campaign.unwrap_or(DEFAULT_TOOL_PROGRAMS);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fuzz");
    std::fs::create_dir_all(&dir).expect("create fuzz dir");

    let mut cases: Vec<(String, Module, Vec<Vec<u64>>)> = Vec::new();
    for seed in first..first + count {
        let src = gen::program(seed);
        let path = dir.join(format!("seed-{seed}.snt"));
        std::fs::write(&path, &src).expect("write program");
        let module = match compile_str(&src) {
            Ok(design) => design.modules.into_iter().next().expect("one module"),
            Err(e) => panic!(
                "seed {seed}: the generated program doesn't compile ({}):\n{e}",
                path.display()
            ),
        };
        let stimulus = gen_stimulus(&module, CYCLES, seed);
        cases.push((format!("fuzz_{seed}"), module, stimulus));
    }

    let tools = tools();
    for (name, module, stimulus) in cases.iter().take(with_tools as usize) {
        co_simulate(&tools, name, module, &[], stimulus, Lint::Design);
    }

    let runs: Vec<(String, &Module, &[Vec<u64>])> = cases
        .iter()
        .map(|(name, module, stim)| (name.clone(), module, stim.as_slice()))
        .collect();
    let traces = rust_sim_traces(&dir.join("rust-sim"), &runs);
    for ((name, module, stimulus), got) in runs.iter().zip(traces) {
        assert!(
            got == run_reference(module, stimulus),
            "{name}: the generated Rust simulator diverges from the interpreter"
        );
    }
}
