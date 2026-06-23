//! Abstract syntax tree for Sinter (`.snt`).
//!
//! The AST is intentionally small: it mirrors the minimal synchronous core in
//! `DESIGN.md` (clocked modules with `uN`/`iN` registers, combinational
//! next-state, single-driver + acyclicity). It carries source positions on
//! statements so later passes can produce located diagnostics.

/// A source position (1-based line and column) for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pos {
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub col: usize,
}

/// A whole compilation unit: zero or more modules.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    /// The modules declared in the unit, in source order.
    pub modules: Vec<Module>,
}

/// A hardware module: an interface plus its registers and combinational logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// The module name (also the emitted Verilog module / filename).
    pub name: String,
    /// The clock domain this module belongs to (defaults to `sys`).
    pub clock_domain: String,
    /// Interface ports, in declaration order.
    pub ports: Vec<Port>,
    /// Registers (persistent state), in declaration order.
    pub regs: Vec<Reg>,
    /// Combinational wire bindings, in declaration order.
    pub lets: Vec<LetBinding>,
    /// Assignments: register next-state (`<=`) and output drives (`=`).
    pub assigns: Vec<Assign>,
    /// Source position of the `module` keyword.
    pub pos: Pos,
}

/// A scalar Sinter type: arbitrary-width unsigned or signed integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// `uN`: unsigned integer of `N` bits.
    Uint(u32),
    /// `iN`: signed (two's complement) integer of `N` bits.
    Int(u32),
}

impl Type {
    /// The bit width of the type.
    #[must_use]
    pub fn width(self) -> u32 {
        match self {
            Type::Uint(w) | Type::Int(w) => w,
        }
    }

    /// Whether the type is signed.
    #[must_use]
    pub fn is_signed(self) -> bool {
        matches!(self, Type::Int(_))
    }
}

impl core::fmt::Display for Type {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Type::Uint(w) => write!(f, "u{w}"),
            Type::Int(w) => write!(f, "i{w}"),
        }
    }
}

/// Port direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// An input read each cycle.
    In,
    /// An output driven combinationally each cycle.
    Out,
}

/// An interface port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// Direction.
    pub dir: Dir,
    /// Port name.
    pub name: String,
    /// Declared type.
    pub ty: Type,
    /// Source position.
    pub pos: Pos,
}

/// A register declaration with an explicit reset value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reg {
    /// Register name.
    pub name: String,
    /// Declared type (its width bounds the stored value).
    pub ty: Type,
    /// Reset / initial value expression.
    pub init: Expr,
    /// Source position.
    pub pos: Pos,
}

/// A combinational wire binding (`let`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetBinding {
    /// Wire name.
    pub name: String,
    /// Optional declared type; otherwise inferred.
    pub ty: Option<Type>,
    /// The driving expression.
    pub value: Expr,
    /// Source position.
    pub pos: Pos,
}

/// The kind of an assignment statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignKind {
    /// `<=`: schedules a register's next-state value.
    NextState,
    /// `=`: drives an output port combinationally.
    Drive,
}

/// An assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assign {
    /// The name being assigned (a register for `<=`, an output for `=`).
    pub target: String,
    /// Assignment kind.
    pub kind: AssignKind,
    /// The right-hand side expression.
    pub value: Expr,
    /// Source position.
    pub pos: Pos,
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Logical / reduction not (`!`) — operates on a 1-bit value.
    Not,
    /// Bitwise complement (`~`).
    BitNot,
    /// Arithmetic negation (`-`).
    Neg,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `&`
    And,
    /// `|`
    Or,
    /// `^`
    Xor,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
}

impl BinOp {
    /// Whether the operator yields a 1-bit boolean result.
    #[must_use]
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
        )
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// An untyped integer literal; its width is resolved from context.
    Literal(u128),
    /// A reference to a port, register, or wire.
    Ident(String),
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        expr: Box<Expr>,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// An `if cond { a } else { b }` value-producing conditional.
    If {
        /// The 1-bit condition.
        cond: Box<Expr>,
        /// Value when the condition is true.
        then: Box<Expr>,
        /// Value when the condition is false.
        els: Box<Expr>,
    },
}
