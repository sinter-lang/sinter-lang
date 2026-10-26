//! Typed intermediate representation produced by the checker.
//!
//! Every expression node carries its resolved [`Type`]. Both backends — the
//! Rust simulator and the Verilog emitter — consume this single typed IR so
//! that their semantics cannot drift apart (DESIGN §7.4, the correctness
//! oracle requires bit-for-bit agreement).

use sinter_syntax::ast::{BinOp, CastMode, Dir, Type, UnaryOp};

/// The 1-bit unsigned type used for booleans/conditions.
pub const BOOL: Type = Type::Uint(1);

/// A checked compilation unit.
#[derive(Debug, Clone)]
pub struct Design {
    /// The checked modules: the `#[clock]` root's first, then one per
    /// `#[module]` struct it instantiates (directly or through another). The
    /// root is complete for simulation; the others exist for its Verilog.
    pub modules: Vec<Module>,
}

/// A checked module: interface plus registers and combinational logic, with
/// `lets` already in evaluation (topological) order.
#[derive(Debug, Clone)]
pub struct Module {
    /// Module name.
    pub name: String,
    /// Clock domain name.
    pub clock_domain: String,
    /// Every clock domain, in declaration order; `domains[0]` is
    /// `clock_domain`. One entry for a single-clock design (DESIGN §6.7).
    pub domains: Vec<Domain>,
    /// Interface ports in declaration order.
    pub ports: Vec<Port>,
    /// Registers in declaration order.
    pub regs: Vec<Reg>,
    /// Combinational wire bindings, **topologically sorted** so each binding
    /// only references earlier ones.
    pub lets: Vec<Let>,
    /// Output drives: `(output port name, value)`.
    pub out_drives: Vec<(String, Expr)>,
    /// Register next-state: `(register name, next value)`. Excludes the read-data
    /// registers of any [`MemRead`] (their next-state is the memory read).
    pub reg_updates: Vec<(String, Expr)>,
    /// Block-RAM memories (DESIGN §6.6/§6.9), held as opaque indexed arrays — not
    /// SROA-flattened to registers.
    pub mems: Vec<Mem>,
    /// Memory read ports: each drives a read-data register (in `regs`) from the
    /// pre-edge array contents.
    pub mem_reads: Vec<MemRead>,
    /// Memory write ports: each commits to its array on the clock edge.
    pub mem_writes: Vec<MemWrite>,
    /// Sim-only assertions (DESIGN §6.12, FLT0). Evaluated by the simulator each
    /// cycle over the pre-edge environment; a `0` (false) condition is a failure.
    /// The Verilog backend **ignores** this vector entirely, so it emits zero bits
    /// — the compile-out guarantee that keeps the two lowerings byte-identical.
    pub asserts: Vec<Assert>,
    /// Instances of other modules: foreign Verilog (DESIGN §6.15, EXT1) and
    /// `#[module]` structs. Each one's Sinter
    /// reference model is ordinary IR in `regs` / `lets` / `reg_updates`, which
    /// the simulator evaluates as usual. The Verilog backend leaves the model
    /// out and emits an instance of the real module in its place.
    pub instances: Vec<Instance>,
}

impl Module {
    /// The input ports.
    pub fn inputs(&self) -> impl Iterator<Item = &Port> {
        self.ports.iter().filter(|p| p.dir == Dir::In)
    }

    /// The output ports.
    pub fn outputs(&self) -> impl Iterator<Item = &Port> {
        self.ports.iter().filter(|p| p.dir == Dir::Out)
    }
}

/// A clock domain (DESIGN §6.7): its name and integer period, in abstract
/// time units. Every register, port, memory port, and assert belongs to one,
/// by index into [`Module::domains`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    /// The domain's name, e.g. `sys`.
    pub name: String,
    /// The clock period (1 for a single-clock design).
    pub period: u64,
}

impl Module {
    /// Whether the design has more than one clock domain.
    #[must_use]
    pub fn is_multi_clock(&self) -> bool {
        self.domains.len() > 1
    }
}

/// A checked interface port.
#[derive(Debug, Clone)]
pub struct Port {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// Direction.
    pub dir: Dir,
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
    /// Fractional bits if the port carries a fixed-point value (DESIGN §6.1),
    /// else 0. Documentation only: backends treat the port as its raw `ty`.
    pub frac_bits: u32,
}

/// A checked register with a constant-evaluated reset value.
#[derive(Debug, Clone)]
pub struct Reg {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
    /// Fractional bits if the register holds a fixed-point value (DESIGN §6.1),
    /// else 0. Documentation only: backends treat the register as its raw `ty`.
    pub frac_bits: u32,
    /// Reset value (already constant-folded to a masked bit pattern).
    pub reset: u64,
}

/// A block-RAM memory primitive (DESIGN §6.6/§6.9). Held as one indexed array,
/// **never** SROA-split into registers — that distinction (vs the `[T; N]`
/// vector-of-registers) is what lets it lower to BRAM.
#[derive(Debug, Clone)]
pub struct Mem {
    /// Name (the flattened struct-field path, e.g. `data` or `inst_data`).
    pub name: String,
    /// Element (word) type.
    pub elem_ty: Type,
    /// Depth in words (a power of two in MEM1).
    pub depth: usize,
    /// Address width = `log2(depth)` bits; read/write addresses are masked to it.
    pub addr_bits: u32,
    /// Read ports (MEM2, DESIGN §9.6); each has its own [`MemRead`] and
    /// read-data register.
    pub read_ports: u32,
    /// Per-word reset/power-up contents (MEM1: all zero), length `depth`.
    pub reset: Vec<u64>,
}

/// A synchronous memory write port (MEM1: one per memory). `we`/`addr`/`data` are
/// combinational expressions sampled from the pre-edge environment; the write
/// commits on the clock edge. `addr` is already masked to the memory's
/// `addr_bits` (a `Cast` to `uN`), so backends index in range.
#[derive(Debug, Clone)]
pub struct MemWrite {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// Target memory name.
    pub mem: String,
    /// Write-enable (1-bit).
    pub we: Expr,
    /// Write address (masked to `addr_bits`).
    pub addr: Expr,
    /// Write data (element-typed).
    pub data: Expr,
}

/// A synchronous memory read port (MEM1: one per memory). It drives a dedicated
/// read-data register `rdata` (an ordinary [`Reg`], reset 0, excluded from
/// `reg_updates`): on the clock edge `rdata <= mem[addr]` against the *pre-edge*
/// array contents, so a same-address read+write returns the OLD value
/// (`READ_FIRST`, DESIGN §9.6). The read result is `Ident(rdata)` (last cycle's
/// value — the `Static(1)` read latency).
#[derive(Debug, Clone)]
pub struct MemRead {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// The read-data register this read drives.
    pub rdata: String,
    /// Source memory name.
    pub mem: String,
    /// Read address (masked to `addr_bits`).
    pub addr: Expr,
}

/// A sim-only assertion (DESIGN §6.12, FLT0). The condition is a self-contained
/// combinational [`Expr`] over the module's signals — it is stored **only** here,
/// never hoisted into [`Module::lets`], so the Verilog backend (which never reads
/// `asserts`) emits nothing for it. A failing `cond` aborts the reference sim with
/// `msg` and `loc`.
#[derive(Debug, Clone)]
pub struct Assert {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// The 1-bit predicate; a `0` value is a failure.
    pub cond: Expr,
    /// The diagnostic message reported on failure.
    pub msg: String,
    /// The failure report's static backtrace (DESIGN §6.12, FLT1a): the chain of
    /// inlined frames from the `#[clock]` root to the assert site, `" -> "`-joined
    /// and ending in the assert's source line — e.g.
    /// `"Cpu -> Alu::step@88 -> divide@45 (line 41)"`, where each `@N` is the
    /// call-site line of that frame. With no inlining it is just
    /// `"<root type> (line N)"`.
    pub loc: String,
}

/// An instance of another Verilog module: existing Verilog bound with
/// `#[verilog]` (DESIGN §6.15, EXT1), or a `#[module]` struct, which Sinter
/// emits as a module of its own.
///
/// The instance's ports are ordinary wires in [`Module::lets`]: each input
/// wire is driven by the parent's logic, and each output wire is driven, in the
/// simulator, by the reference model's logic and, in Verilog, by the instance.
/// Everything in `model_regs` / `model_lets` / `model_mems` /
/// `model_mem_regs` exists only in the simulator.
#[derive(Debug, Clone)]
pub struct Instance {
    /// The clock domain (index into [`Module::domains`]).
    pub domain: usize,
    /// The Verilog instance name, e.g. `u_uart`.
    pub name: String,
    /// The Verilog module to instantiate, e.g. `uart_tx`.
    pub module: String,
    /// The Sinter struct that models it, e.g. `UartTx`.
    pub model: String,
    /// The module's clock port, connected to `clk` (`None` for a combinational
    /// model).
    pub clk: Option<String>,
    /// The module's active-low reset port, connected to `rst_n` (`None` for a
    /// combinational model or `rst_n = none`).
    pub rst_n: Option<String>,
    /// Input connections: `(Verilog port, wire in lets)`.
    pub inputs: Vec<(String, String)>,
    /// Output connections: `(Verilog port, wire in lets)`.
    pub outputs: Vec<(String, String)>,
    /// The model's state registers (simulator only).
    pub model_regs: Vec<String>,
    /// The model's wires (simulator only): first its next-state wires, one per
    /// entry of `model_regs` in the same order, then its body's own wires.
    pub model_lets: Vec<String>,
    /// The module's Verilog source file, if given (`src = "..."`), resolved
    /// against the `.snt` file's directory.
    pub src: Option<std::path::PathBuf>,
    /// Whether the instantiated module is generated by Sinter from the model
    /// itself (a `#[module]` struct), rather than existing Verilog. Its
    /// definition is another module of the same [`Design`].
    pub generated: bool,
    /// The model's memories (simulator only; a `#[module]` may hold them).
    pub model_mems: Vec<String>,
    /// The read-data registers of `model_mems` (simulator only).
    pub model_mem_regs: Vec<String>,
}

impl Module {
    /// Whether register or wire `name` exists only in a foreign module's
    /// reference model (and so is absent from the emitted Verilog).
    #[must_use]
    pub fn is_model_only(&self, name: &str) -> bool {
        self.instances.iter().any(|i| {
            i.model_regs.iter().any(|n| n == name)
                || i.model_lets.iter().any(|n| n == name)
                || i.model_mems.iter().any(|n| n == name)
                || i.model_mem_regs.iter().any(|n| n == name)
        })
    }

    /// The modules generated for this module's `#[module]` instances, by name
    /// (each once, in instance order).
    #[must_use]
    pub fn generated_children(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for i in self.instances.iter().filter(|i| i.generated) {
            if !out.contains(&i.module.as_str()) {
                out.push(&i.module);
            }
        }
        out
    }

    /// The instance driving wire `name` in Verilog, if it is an instance output.
    #[must_use]
    pub fn instance_output(&self, name: &str) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|i| i.outputs.iter().any(|(_, w)| w == name))
    }
}

/// A checked combinational wire binding.
#[derive(Debug, Clone)]
pub struct Let {
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
    /// Fractional bits if the wire carries a fixed-point value (DESIGN §6.1),
    /// else 0. Documentation only, as for [`Port::frac_bits`].
    pub frac_bits: u32,
    /// Driving expression.
    pub value: Expr,
}

/// A typed expression: an [`ExprKind`] plus its resolved [`Type`].
#[derive(Debug, Clone)]
pub struct Expr {
    /// The expression node.
    pub kind: ExprKind,
    /// The resolved type of the expression's value.
    pub ty: Type,
}

/// The shape of a typed expression.
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// An integer literal (masked to `ty`'s width).
    Literal(u64),
    /// A reference to a port, register, or wire.
    Ident(String),
    /// A unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// A binary operation.
    Binary {
        /// Operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// A conditional (`if c { a } else { b }`).
    If {
        /// 1-bit condition.
        cond: Box<Expr>,
        /// Value when true.
        then: Box<Expr>,
        /// Value when false.
        els: Box<Expr>,
    },
    /// A mode-named cast to the node's own [`Type`] (DESIGN §6.3). `Wrap` resizes (sign/zero-extend when widening, truncate when
    /// narrowing); `Sat` saturates to the node type's range.
    Cast {
        /// The operand being converted (carries its own source type).
        expr: Box<Expr>,
        /// How out-of-range values are handled.
        mode: CastMode,
    },
    /// Left shift by a constant, widening the result by `amount` bits (the node
    /// type is `amount` bits wider than the operand). Used for fixed-point
    /// fractional alignment / precision growth.
    Shl {
        /// The value being shifted.
        expr: Box<Expr>,
        /// The (compile-time constant) shift amount.
        amount: u32,
    },
    /// Right shift by a constant, narrowing the result by `amount` bits
    /// (arithmetic for a signed operand, logical for unsigned). Used for
    /// fixed-point fractional truncation/rounding.
    Shr {
        /// The value being shifted.
        expr: Box<Expr>,
        /// The (compile-time constant) shift amount.
        amount: u32,
    },
    /// Variable (barrel) shift by a runtime `amount`, keeping the operand width
    /// (DESIGN §6.13 / §9.10, BIT2). Left is logical; right is arithmetic for a
    /// signed operand, logical for an unsigned one. A shift of `amount >= width`
    /// yields `0` (or the sign fill for an arithmetic right shift). The node type
    /// is the operand type (same width).
    VarShift {
        /// The value being shifted.
        expr: Box<Expr>,
        /// The runtime shift amount (an unsigned integer).
        amount: Box<Expr>,
        /// Left (`<<`) if true, right (`>>`) if false.
        left: bool,
    },
}
