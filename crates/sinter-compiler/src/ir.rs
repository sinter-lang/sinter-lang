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
    /// Register next-state: `(register name, next value)`.
    pub reg_updates: Vec<(String, Expr)>,
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
    /// emits one.
    Cast {
        /// The operand being converted (carries its own source type).
        expr: Box<Expr>,
        /// How out-of-range / fractional values are handled.
        mode: CastMode,
    },
}
