//! Cycle-accurate Rust simulator (the reference model for the oracle).
//!
//! The cycle protocol mirrors what the generated Verilog does under a Verilator
//! testbench (see `verilator.rs`):
//!
//! ```text
//! state S_0 = reset
//! for cycle k:
//!     outputs O_k = comb(inputs I_k, state S_k)   // recorded
//!     state  S_k+1 = next(inputs I_k, state S_k)  // committed on the posedge
//! ```

use std::collections::HashMap;

use crate::error::mask;
use crate::eval::eval_expr;
use crate::ir::Module;

/// A sim-only assertion failure (DESIGN §6.12, FLT0): an `assert!` whose
/// condition read false during a cycle. Recorded rather than panicked so callers
/// (tests, the CLI) decide how to report — a library never aborts the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertFailure {
    /// The 0-based cycle index the assertion fired on.
    pub cycle: usize,
    /// The assertion's diagnostic message.
    pub msg: String,
    /// The assertion's static backtrace — the inlined call path to the site
    /// (DESIGN §6.12, FLT1a), e.g. `"Cpu -> Alu::step@88 -> divide@45 (line 41)"`.
    pub loc: String,
}

/// A running cycle-accurate model of one [`Module`].
pub struct Simulator<'a> {
    module: &'a Module,
    /// Current register values, keyed by register name.
    state: HashMap<String, u64>,
    /// Current memory contents, keyed by memory name (DESIGN §6.6/§9.6).
    mem: HashMap<String, Vec<u64>>,
    /// Cycles advanced so far (the index the next [`Simulator::step`] evaluates).
    cycle: usize,
    /// `assert!` failures observed so far, in cycle order (DESIGN §6.12, FLT0).
    assert_failures: Vec<AssertFailure>,
}

impl<'a> Simulator<'a> {
    /// Create a simulator with registers and memories at their reset values.
    #[must_use]
    pub fn new(module: &'a Module) -> Self {
        let state = module
            .regs
            .iter()
            .map(|r| (r.name.clone(), r.reset))
            .collect();
        let mem = module
            .mems
            .iter()
            .map(|m| (m.name.clone(), m.reset.clone()))
            .collect();
        Simulator {
            module,
            state,
            mem,
            cycle: 0,
            assert_failures: Vec::new(),
        }
    }

    /// Restore all registers and memories to their reset values, and clear the
    /// cycle counter and recorded assertion failures.
    pub fn reset(&mut self) {
        for r in &self.module.regs {
            self.state.insert(r.name.clone(), r.reset);
        }
        for m in &self.module.mems {
            self.mem.insert(m.name.clone(), m.reset.clone());
        }
        self.cycle = 0;
        self.assert_failures.clear();
    }

    /// The `assert!` failures observed so far, in cycle order (DESIGN §6.12,
    /// FLT0). Empty unless a sim-only assertion read false.
    #[must_use]
    pub fn assert_failures(&self) -> &[AssertFailure] {
        &self.assert_failures
    }

    /// Advance one cycle. `inputs` maps input-port names to values (masked to
    /// the port width here). Returns the outputs sampled this cycle.
    pub fn step(&mut self, inputs: &HashMap<String, u64>) -> HashMap<String, u64> {
        let mut env: HashMap<String, u64> = HashMap::new();

        // Inputs (masked to width); missing inputs default to 0.
        for p in self.module.inputs() {
            let v = inputs.get(&p.name).copied().unwrap_or(0);
            env.insert(p.name.clone(), mask(v, p.ty.width()));
        }
        // Current register values.
        for r in &self.module.regs {
            let v = self.state.get(&r.name).copied().unwrap_or(r.reset);
            env.insert(r.name.clone(), v);
        }
        // Combinational wires, already topologically ordered.
        for l in &self.module.lets {
            let v = eval_expr(&l.value, &env);
            env.insert(l.name.clone(), v);
        }

        // Sim-only assertions (DESIGN §6.12, FLT0): evaluate each over the fully
        // populated pre-edge environment (inputs + registers + wires) — the same
        // one `out_drives`/`reg_updates` read. A `0` (false) condition is recorded
        // as a failure; the Verilog backend has no counterpart, so this affects
        // no output bit and the co-simulation stays byte-identical.
        for a in &self.module.asserts {
            if eval_expr(&a.cond, &env) == 0 {
                self.assert_failures.push(AssertFailure {
                    cycle: self.cycle,
                    msg: a.msg.clone(),
                    loc: a.loc.clone(),
                });
            }
        }
        self.cycle += 1;

        // Sample outputs from the current state/inputs.
        let mut outputs = HashMap::new();
        for (name, expr) in &self.module.out_drives {
            outputs.insert(name.clone(), eval_expr(expr, &env));
        }

        // Compute next state from the same environment, then commit atomically.
        let mut next = HashMap::new();
        for (name, expr) in &self.module.reg_updates {
            next.insert(name.clone(), eval_expr(expr, &env));
        }

        // Memory read ports: each read-data register latches the addressed word of
        // the *pre-edge* array — so a same-cycle same-address read sees the OLD
        // value (READ_FIRST), and the result appears one cycle later (`Static(1)`).
        for r in &self.module.mem_reads {
            let idx = eval_expr(&r.addr, &env) as usize;
            let v = self
                .mem
                .get(&r.mem)
                .and_then(|arr| arr.get(idx))
                .copied()
                .unwrap_or(0);
            next.insert(r.rdata.clone(), v);
        }

        // Memory write ports: commit into a scratch clone of the pre-edge array
        // (so reads above are unaffected), then swap it in atomically.
        let mut mem_next = self.mem.clone();
        for w in &self.module.mem_writes {
            if eval_expr(&w.we, &env) != 0 {
                let idx = eval_expr(&w.addr, &env) as usize;
                let data = eval_expr(&w.data, &env);
                if let Some(cell) = mem_next.get_mut(&w.mem).and_then(|arr| arr.get_mut(idx)) {
                    *cell = data;
                }
            }
        }

        self.state = next;
        self.mem = mem_next;

        outputs
    }
}

/// Generate a deterministic input stimulus: `cycles` rows, each holding one
/// value per input port (in declaration order). Uses a fixed LCG so the Rust
/// reference and the Verilator run see identical inputs.
#[must_use]
pub fn gen_stimulus(module: &Module, cycles: usize, seed: u64) -> Vec<Vec<u64>> {
    let inputs: Vec<_> = module.inputs().collect();
    let mut x = seed;
    let mut next = || {
        // SplitMix64-style step; deterministic and dependency-free.
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    (0..cycles)
        .map(|_| inputs.iter().map(|p| mask(next(), p.ty.width())).collect())
        .collect()
}

/// Serialize a stimulus to the binary format the Verilator harness reads:
/// 8 little-endian bytes per value, ports in declaration order, row by row.
#[must_use]
pub fn stimulus_to_bytes(stimulus: &[Vec<u64>]) -> Vec<u8> {
    let mut out = Vec::new();
    for row in stimulus {
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

/// Run the reference model over a stimulus and return the output trace in the
/// same binary format the Verilator harness emits: 8 little-endian bytes per
/// output value, ports in declaration order, one row per cycle.
#[must_use]
pub fn run_reference(module: &Module, stimulus: &[Vec<u64>]) -> Vec<u8> {
    let input_names: Vec<String> = module.inputs().map(|p| p.name.clone()).collect();
    let output_order: Vec<String> = module.outputs().map(|p| p.name.clone()).collect();

    let mut sim = Simulator::new(module);
    let mut trace = Vec::new();
    for row in stimulus {
        let inputs: HashMap<String, u64> = input_names
            .iter()
            .cloned()
            .zip(row.iter().copied())
            .collect();
        let outputs = sim.step(&inputs);
        for name in &output_order {
            let v = outputs.get(name).copied().unwrap_or(0);
            trace.extend_from_slice(&v.to_le_bytes());
        }
    }
    trace
}
