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

use sinter_syntax::ast::{BinOp, CastMode, Type, UnaryOp};

use crate::error::mask;
use crate::eval::{apply_binary, apply_cast, apply_shl, apply_shr, apply_unary};
use crate::ir::{Expr, ExprKind, Module};

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

/// An IR expression with every name resolved to an environment slot, so the
/// per-cycle evaluation does no string hashing. Each node carries the types
/// its operation needs; the arithmetic itself is `eval.rs`'s, shared with
/// every other evaluator.
enum Node {
    Lit(u64),
    Slot(usize),
    Unary(UnaryOp, Box<Node>, Type),
    Binary(BinOp, Box<Node>, Type, Box<Node>, Type, Type),
    If(Box<Node>, Box<Node>, Box<Node>),
    Cast(Box<Node>, Type, Type, CastMode),
    Shl(Box<Node>, u32, u32),
    Shr(Box<Node>, Type, u32, u32),
    VarShift(Box<Node>, Box<Node>, Type, u32, bool),
}

impl Node {
    /// Resolve `e` against `slots`. Every name is in scope (the checker
    /// guarantees it); an unknown one would read as 0.
    fn compile(e: &Expr, slots: &HashMap<&str, usize>) -> Node {
        let c = |x: &Expr| Box::new(Node::compile(x, slots));
        match &e.kind {
            ExprKind::Literal(v) => Node::Lit(*v),
            ExprKind::Ident(n) => slots
                .get(n.as_str())
                .map_or(Node::Lit(0), |&s| Node::Slot(s)),
            ExprKind::Unary { op, expr } => Node::Unary(*op, c(expr), expr.ty),
            ExprKind::Binary { op, lhs, rhs } => {
                Node::Binary(*op, c(lhs), lhs.ty, c(rhs), rhs.ty, e.ty)
            }
            ExprKind::If { cond, then, els } => Node::If(c(cond), c(then), c(els)),
            ExprKind::Cast { expr, mode } => Node::Cast(c(expr), expr.ty, e.ty, *mode),
            ExprKind::Shl { expr, amount } => Node::Shl(c(expr), e.ty.width(), *amount),
            ExprKind::Shr { expr, amount } => Node::Shr(c(expr), expr.ty, e.ty.width(), *amount),
            ExprKind::VarShift { expr, amount, left } => {
                Node::VarShift(c(expr), c(amount), expr.ty, e.ty.width(), *left)
            }
        }
    }

    fn eval(&self, env: &[u64]) -> u64 {
        match self {
            Node::Lit(v) => *v,
            Node::Slot(s) => env[*s],
            Node::Unary(op, x, ty) => apply_unary(*op, x.eval(env), *ty),
            Node::Binary(op, l, lty, r, rty, ty) => {
                apply_binary(*op, l.eval(env), *lty, r.eval(env), *rty, *ty)
            }
            Node::If(c, t, e) => {
                if c.eval(env) != 0 {
                    t.eval(env)
                } else {
                    e.eval(env)
                }
            }
            Node::Cast(x, from, to, mode) => apply_cast(x.eval(env), *from, *to, *mode),
            Node::Shl(x, to_w, amount) => apply_shl(x.eval(env), *to_w, *amount),
            Node::Shr(x, from, to_w, amount) => apply_shr(x.eval(env), *from, *to_w, *amount),
            // Variable (barrel) shift, same operand width: the const-shift
            // helpers already treat an amount `>= width` as 0 / sign fill.
            Node::VarShift(x, amount, from, w, left) => {
                let v = x.eval(env);
                let sh = u32::try_from(amount.eval(env)).unwrap_or(u32::MAX);
                if *left {
                    apply_shl(v, *w, sh)
                } else {
                    apply_shr(v, *from, *w, sh)
                }
            }
        }
    }
}

/// A memory read port, resolved: `rdata` register index, memory index, address.
struct ReadPort {
    rdata: usize,
    mem: usize,
    addr: Node,
}

/// A memory write port, resolved: memory index, then enable / address / data.
struct WritePort {
    mem: usize,
    we: Node,
    addr: Node,
    data: Node,
}

/// A running cycle-accurate model of one [`Module`].
///
/// The module is compiled once into slot-indexed form. Each cycle fills one
/// environment vector: inputs, then registers, then wires in topological
/// order, against which the outputs and next state are evaluated.
pub struct Simulator<'a> {
    module: &'a Module,
    /// Environment slot of every input, register, and wire, by name.
    slots: HashMap<String, usize>,
    /// The environment: the most recent step's pre-edge values.
    env: Vec<u64>,
    /// Input ports: `(name, slot, width)`.
    inputs: Vec<(String, usize, u32)>,
    /// Register slots, in `module.regs` order.
    reg_slots: Vec<usize>,
    /// Current register values, in `module.regs` order.
    state: Vec<u64>,
    /// Wires: `(slot, value)`, topologically ordered.
    lets: Vec<(usize, Node)>,
    /// Sim-only assert conditions, in `module.asserts` order.
    asserts: Vec<Node>,
    outputs: Vec<(String, Node)>,
    /// Register next-states: `(register index, value)`.
    updates: Vec<(usize, Node)>,
    reads: Vec<ReadPort>,
    writes: Vec<WritePort>,
    /// Current memory contents, in `module.mems` order (DESIGN §6.6/§9.6).
    mem: Vec<Vec<u64>>,
    /// Cycles advanced so far (the index the next [`Simulator::step`] evaluates).
    cycle: usize,
    /// `assert!` failures observed so far, in cycle order (DESIGN §6.12, FLT0).
    assert_failures: Vec<AssertFailure>,
}

impl<'a> Simulator<'a> {
    /// Create a simulator with registers and memories at their reset values.
    #[must_use]
    pub fn new(module: &'a Module) -> Self {
        let names = module
            .inputs()
            .map(|p| p.name.as_str())
            .chain(module.regs.iter().map(|r| r.name.as_str()))
            .chain(module.lets.iter().map(|l| l.name.as_str()));
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for name in names {
            let next = by_name.len();
            by_name.entry(name).or_insert(next);
        }
        let slot = |n: &str| by_name.get(n).copied().unwrap_or(0);
        let reg_index: HashMap<&str, usize> = module
            .regs
            .iter()
            .enumerate()
            .map(|(i, r)| (r.name.as_str(), i))
            .collect();
        let mem_index: HashMap<&str, usize> = module
            .mems
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.as_str(), i))
            .collect();
        let node = |e: &Expr| Node::compile(e, &by_name);

        Simulator {
            module,
            env: vec![0; by_name.len()],
            inputs: module
                .inputs()
                .map(|p| (p.name.clone(), slot(&p.name), p.ty.width()))
                .collect(),
            reg_slots: module.regs.iter().map(|r| slot(&r.name)).collect(),
            state: module.regs.iter().map(|r| r.reset).collect(),
            lets: module
                .lets
                .iter()
                .map(|l| (slot(&l.name), node(&l.value)))
                .collect(),
            asserts: module.asserts.iter().map(|a| node(&a.cond)).collect(),
            outputs: module
                .out_drives
                .iter()
                .map(|(n, e)| (n.clone(), node(e)))
                .collect(),
            updates: module
                .reg_updates
                .iter()
                .filter_map(|(n, e)| reg_index.get(n.as_str()).map(|&i| (i, node(e))))
                .collect(),
            reads: module
                .mem_reads
                .iter()
                .filter_map(|r| {
                    Some(ReadPort {
                        rdata: *reg_index.get(r.rdata.as_str())?,
                        mem: *mem_index.get(r.mem.as_str())?,
                        addr: node(&r.addr),
                    })
                })
                .collect(),
            writes: module
                .mem_writes
                .iter()
                .filter_map(|w| {
                    Some(WritePort {
                        mem: *mem_index.get(w.mem.as_str())?,
                        we: node(&w.we),
                        addr: node(&w.addr),
                        data: node(&w.data),
                    })
                })
                .collect(),
            mem: module.mems.iter().map(|m| m.reset.clone()).collect(),
            slots: by_name
                .into_iter()
                .map(|(n, s)| (n.to_string(), s))
                .collect(),
            cycle: 0,
            assert_failures: Vec::new(),
        }
    }

    /// Restore all registers and memories to their reset values, and clear the
    /// cycle counter and recorded assertion failures.
    pub fn reset(&mut self) {
        self.state = self.module.regs.iter().map(|r| r.reset).collect();
        self.mem = self.module.mems.iter().map(|m| m.reset.clone()).collect();
        self.env.iter_mut().for_each(|v| *v = 0);
        self.cycle = 0;
        self.assert_failures.clear();
    }

    /// The `assert!` failures observed so far, in cycle order (DESIGN §6.12,
    /// FLT0). Empty unless a sim-only assertion read false.
    #[must_use]
    pub fn assert_failures(&self) -> &[AssertFailure] {
        &self.assert_failures
    }

    /// The value an input port, register, or wire held during the most recent
    /// [`Simulator::step`] (registers *before* that step's clock edge). `None`
    /// before the first step or for an unknown name. Outputs are returned by
    /// `step` itself.
    #[must_use]
    pub fn signal(&self, name: &str) -> Option<u64> {
        if self.cycle == 0 {
            return None;
        }
        self.slots.get(name).map(|&s| self.env[s])
    }

    /// Advance one cycle. `inputs` maps input-port names to values (masked to
    /// the port width here; missing inputs read 0). Returns the outputs sampled
    /// this cycle.
    pub fn step(&mut self, inputs: &HashMap<String, u64>) -> HashMap<String, u64> {
        // Inputs, current registers, then wires (already topologically ordered).
        for (name, slot, width) in &self.inputs {
            self.env[*slot] = mask(inputs.get(name).copied().unwrap_or(0), *width);
        }
        for (slot, v) in self.reg_slots.iter().zip(&self.state) {
            self.env[*slot] = *v;
        }
        for (slot, value) in &self.lets {
            let v = value.eval(&self.env);
            self.env[*slot] = v;
        }

        // Sim-only assertions (DESIGN §6.12, FLT0): evaluate each over the fully
        // populated pre-edge environment (inputs + registers + wires) — the same
        // one the outputs and next state read. A `0` (false) condition is
        // recorded as a failure; the Verilog backend has no counterpart, so this
        // affects no output bit and the co-simulation stays byte-identical.
        for (cond, a) in self.asserts.iter().zip(&self.module.asserts) {
            if cond.eval(&self.env) == 0 {
                self.assert_failures.push(AssertFailure {
                    cycle: self.cycle,
                    msg: a.msg.clone(),
                    loc: a.loc.clone(),
                });
            }
        }
        self.cycle += 1;

        // Sample outputs from the current state/inputs.
        let outputs = self
            .outputs
            .iter()
            .map(|(name, e)| (name.clone(), e.eval(&self.env)))
            .collect();

        // Compute next state from the same environment, then commit atomically.
        let mut next = self.state.clone();
        for (i, e) in &self.updates {
            next[*i] = e.eval(&self.env);
        }

        // Memory read ports: each read-data register latches the addressed word of
        // the *pre-edge* array — so a same-cycle same-address read sees the OLD
        // value (READ_FIRST), and the result appears one cycle later (`Static(1)`).
        for r in &self.reads {
            let idx = r.addr.eval(&self.env) as usize;
            next[r.rdata] = self.mem[r.mem].get(idx).copied().unwrap_or(0);
        }

        // Memory write ports: commit after the reads above, so they see the
        // pre-edge array.
        for w in &self.writes {
            if w.we.eval(&self.env) != 0 {
                let idx = w.addr.eval(&self.env) as usize;
                let data = w.data.eval(&self.env);
                if let Some(cell) = self.mem[w.mem].get_mut(idx) {
                    *cell = data;
                }
            }
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
