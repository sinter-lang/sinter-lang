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
    /// The checked modules.
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
    /// SROA-flattened to registers. Empty on the legacy `module` path.
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

/// A checked interface port.
#[derive(Debug, Clone)]
pub struct Port {
    /// Direction.
    pub dir: Dir,
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
}

/// A checked register with a constant-evaluated reset value.
#[derive(Debug, Clone)]
pub struct Reg {
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
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
    /// Per-word reset/power-up contents (MEM1: all zero), length `depth`.
    pub reset: Vec<u64>,
}

/// A synchronous memory write port (MEM1: one per memory). `we`/`addr`/`data` are
/// combinational expressions sampled from the pre-edge environment; the write
/// commits on the clock edge. `addr` is already masked to the memory's
/// `addr_bits` (a `Cast` to `uN`), so backends index in range.
#[derive(Debug, Clone)]
pub struct MemWrite {
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
    /// The 1-bit predicate; a `0` value is a failure.
    pub cond: Expr,
    /// The diagnostic message reported on failure.
    pub msg: String,
    /// A best-effort source location for the failure report (FLT0: the module
    /// context + source line; FLT1 will replace this with the full inlined
    /// call/instance path — the "static backtrace").
    pub loc: String,
}

/// A checked combinational wire binding.
#[derive(Debug, Clone)]
pub struct Let {
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Type,
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
    /// A mode-named cast to the node's own [`Type`] (DESIGN §6.3). Produced by
    /// the functional surface's elaboration; the legacy `module` path never
    /// emits one. `Wrap` resizes (sign/zero-extend when widening, truncate when
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
