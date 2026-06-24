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

/// A running cycle-accurate model of one [`Module`].
pub struct Simulator<'a> {
    module: &'a Module,
    /// Current register values, keyed by register name.
    state: HashMap<String, u64>,
}

impl<'a> Simulator<'a> {
    /// Create a simulator with registers at their reset values.
    #[must_use]
    pub fn new(module: &'a Module) -> Self {
        let state = module
            .regs
            .iter()
            .map(|r| (r.name.clone(), r.reset))
            .collect();
        Simulator { module, state }
    }

    /// Restore all registers to their reset values.
    pub fn reset(&mut self) {
        for r in &self.module.regs {
            self.state.insert(r.name.clone(), r.reset);
        }
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
        self.state = next;

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
