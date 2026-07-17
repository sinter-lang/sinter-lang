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

/// Item / field visibility (DESIGN §10). Private unless written `pub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Visible only within the defining module and its descendants (the default).
    #[default]
    Private,
    /// Exported with `pub`.
    Public,
}

/// A whole compilation unit: legacy modules and/or functional-surface items.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    /// The legacy `module` declarations, in source order. The compiler's
    /// current pipeline lowers only these.
    pub modules: Vec<Module>,
    /// The functional-surface items (`const`/`struct`/`enum`/`impl`/`fn`), in
    /// source order. Parsed today; semantic checks and lowering are a later
    /// milestone (DESIGN §2.1). These will eventually subsume `modules`.
    pub items: Vec<Item>,
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
    /// `*`
    Mul,
    /// `<<` — left shift by a constant amount (DESIGN §6.13, BIT1); the result
    /// widens by the shift amount.
    Shl,
    /// `>>` — right shift by a constant amount (DESIGN §6.13, BIT1); the result
    /// narrows by the shift amount (arithmetic for a signed operand).
    Shr,
    /// `@` — bit concatenation (DESIGN §6.13, BIT1); `a @ b` joins `a` (high) and
    /// `b` (low) into a `u{wa+wb}`.
    Concat,
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
    ///
    /// In the legacy module surface `then`/`els` are bare expressions; in the
    /// functional surface they are [`Expr::Block`]s.
    If {
        /// The 1-bit condition.
        cond: Box<Expr>,
        /// Value when the condition is true.
        then: Box<Expr>,
        /// Value when the condition is false.
        els: Box<Expr>,
    },

    // --- Functional-surface expressions (DESIGN §2.1) ----------------------
    /// A `::`-segmented path, e.g. `Cmd::Up` (≥ 2 segments).
    Path(Vec<String>),
    /// `include_hex!("path")` — the contents of a line-delimited hex file, used to
    /// initialize a `rom` (DESIGN §9.7). Expanded to an [`Expr::Array`] at compile
    /// time (relative to the source directory) before checking/elaboration, so no
    /// later pass sees it. Holds the raw path string.
    IncludeHex(String),
    /// Field access `base.field`.
    Field {
        /// The receiver expression.
        base: Box<Expr>,
        /// The field name.
        field: String,
    },
    /// A function call `func(args)`.
    Call {
        /// The callee (an ident or path).
        func: Box<Expr>,
        /// The argument expressions, in order.
        args: Vec<Expr>,
    },
    /// A method call `receiver.method(args)`.
    MethodCall {
        /// The receiver expression.
        receiver: Box<Expr>,
        /// The method name.
        method: String,
        /// The argument expressions, in order.
        args: Vec<Expr>,
    },
    /// An index expression `base[index]`.
    Index {
        /// The indexed expression.
        base: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
    },
    /// A bit-slice `base[hi:lo]` (DESIGN §6.13, BIT1): the contiguous bit field
    /// from `hi` down to `lo` (inclusive) of a scalar, of type `u{hi-lo+1}`.
    /// `hi` and `lo` are compile-time constants.
    Slice {
        /// The value being sliced.
        base: Box<Expr>,
        /// The high (most-significant) bit index, inclusive.
        hi: Box<Expr>,
        /// The low (least-significant) bit index, inclusive.
        lo: Box<Expr>,
    },
    /// A struct literal `Name { field: value, ... }`.
    StructLit {
        /// The struct type name.
        name: String,
        /// The field initializers, in source order.
        fields: Vec<FieldInit>,
    },
    /// A tuple `(a, b, ...)` (≥ 2 elements).
    Tuple(Vec<Expr>),
    /// An array literal `[a, b, ...]`.
    Array(Vec<Expr>),
    /// `Some(x)` — an `Option` carrying a value (DESIGN §6.5).
    Some(Box<Expr>),
    /// `None` — the empty `Option` (DESIGN §6.5).
    None,
    /// Postfix `expr?` — Option/validity propagation (DESIGN §6.4): yields the
    /// payload and folds the operand's validity into the enclosing
    /// `Option`-returning function's result.
    Try(Box<Expr>),
    /// A mode-named narrowing cast `expr as Type mode` (DESIGN §6.3).
    Cast {
        /// The operand being narrowed.
        expr: Box<Expr>,
        /// The target type.
        ty: TypeRef,
        /// How out-of-range values are handled.
        mode: CastMode,
    },
    /// A `match` expression over a scrutinee.
    Match {
        /// The value being matched.
        scrutinee: Box<Expr>,
        /// The arms, in source order.
        arms: Vec<MatchArm>,
    },
    /// A block expression `{ let …; … tail }` whose value is its tail.
    Block(Block),
}

// ===========================================================================
// Functional surface (DESIGN §2.1). Parsed today; semantic checks and lowering
// are a later milestone. The canonical example is `examples/go_board_counter.snt`.
// ===========================================================================

/// A top-level functional-surface item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A `const` binding.
    Const(ConstItem),
    /// A `struct` declaration (state, inputs, or outputs).
    Struct(StructItem),
    /// An `enum` declaration.
    Enum(EnumItem),
    /// An `impl` block grouping transition methods.
    Impl(ImplItem),
    /// A free function.
    Func(Func),
    /// A `mod` declaration — external (`mod foo;`) or inline (`mod foo { … }`)
    /// (DESIGN §10).
    Mod(ModItem),
    /// A `use` import (DESIGN §10).
    Use(UseItem),
}

/// A `mod` declaration (DESIGN §10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModItem {
    /// Visibility of the module itself.
    pub vis: Visibility,
    /// The module name.
    pub name: String,
    /// `Some(items)` for an inline `mod foo { … }`; `None` for an external
    /// `mod foo;` whose body the loader reads from `foo.snt`.
    pub body: Option<Vec<Item>>,
    /// Source position of the `mod` keyword.
    pub pos: Pos,
}

/// A `use` import (DESIGN §10). Name resolution / visibility checking is a later
/// milestone (S2); this records the parsed path only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseItem {
    /// `pub use` re-exports the imported name; otherwise the import is private.
    pub vis: Visibility,
    /// The imported path.
    pub path: UsePath,
    /// Source position of the `use` keyword.
    pub pos: Pos,
}

/// A `use` path: an optional leading qualifier, `::`-separated segments, and an
/// optional trailing `*` glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsePath {
    /// The leading qualifier (`root` / `self` / `super`…), if any.
    pub qualifier: PathQualifier,
    /// The `::`-separated segments after the qualifier (excludes a trailing `*`).
    pub segments: Vec<String>,
    /// Whether the path ends in a `::*` glob.
    pub glob: bool,
}

/// The leading qualifier of a [`UsePath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathQualifier {
    /// No qualifier — a bare path.
    None,
    /// `root::` — the project root module.
    Root,
    /// `self::` — the current module.
    SelfMod,
    /// `super::` … — `N` levels up toward the root.
    Super(usize),
}

/// A `const NAME: TYPE = EXPR;` binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstItem {
    /// Visibility (DESIGN §10).
    pub vis: Visibility,
    /// The constant's name.
    pub name: String,
    /// The declared type.
    pub ty: TypeRef,
    /// The value expression.
    pub value: Expr,
    /// Source position of the `const` keyword.
    pub pos: Pos,
}

/// A `struct` declaration with inline reset values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructItem {
    /// Attributes preceding the struct (e.g. none today).
    pub attrs: Vec<Attr>,
    /// Visibility (DESIGN §10).
    pub vis: Visibility,
    /// The struct name.
    pub name: String,
    /// The fields, in source order.
    pub fields: Vec<StructField>,
    /// Source position of the `struct` keyword.
    pub pos: Pos,
}

/// A field of a [`StructItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructField {
    /// Visibility (DESIGN §10) — fields are private by default.
    pub vis: Visibility,
    /// The field name.
    pub name: String,
    /// The field type.
    pub ty: TypeRef,
    /// The optional inline reset/initial value (`= expr`).
    pub default: Option<Expr>,
    /// Source position.
    pub pos: Pos,
}

/// An `enum` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumItem {
    /// Attributes preceding the enum (e.g. `#[encoding(binary)]`).
    pub attrs: Vec<Attr>,
    /// Visibility (DESIGN §10).
    pub vis: Visibility,
    /// The enum name.
    pub name: String,
    /// The variants, in source order.
    pub variants: Vec<EnumVariant>,
    /// Source position of the `enum` keyword.
    pub pos: Pos,
}

/// A variant of an [`EnumItem`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// The variant name.
    pub name: String,
    /// An optional explicit discriminant (`= expr`), for protocol encodings.
    pub discriminant: Option<Expr>,
    /// Source position.
    pub pos: Pos,
}

/// An `impl Name { ... }` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplItem {
    /// The type the methods are implemented on.
    pub type_name: String,
    /// The methods, in source order.
    pub methods: Vec<Func>,
    /// Source position of the `impl` keyword.
    pub pos: Pos,
}

/// A function color (DESIGN §3): plain combinational, or a staged color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FnColor {
    /// A plain combinational / stateful-step function (the default).
    #[default]
    Comb,
    /// A `pipeline fn` — `yield` marks stage boundaries (DESIGN §3.1).
    Pipeline,
    /// A `multiplex fn` — arms folded in time over a shared resource (§3.2).
    Multiplex,
}

/// A function or method definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Func {
    /// Attributes (e.g. `#[clock(domain = sys)]` on the clock root).
    pub attrs: Vec<Attr>,
    /// Visibility (DESIGN §10).
    pub vis: Visibility,
    /// The function color (DESIGN §3); `Comb` unless `pipeline`/`multiplex`.
    pub color: FnColor,
    /// The function name.
    pub name: String,
    /// The parameters, in order (a leading `self` for methods).
    pub params: Vec<Param>,
    /// The return type.
    pub ret: TypeRef,
    /// The body block.
    pub body: Block,
    /// Source position of the `fn` keyword.
    pub pos: Pos,
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    /// The `self` receiver (by value).
    SelfRef {
        /// Source position.
        pos: Pos,
    },
    /// A typed parameter `name: ty`.
    Typed {
        /// The parameter name.
        name: String,
        /// The parameter type.
        ty: TypeRef,
        /// Source position.
        pos: Pos,
    },
}

/// An attribute `#[name]` or `#[name(args)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// The attribute name (e.g. `clock`, `encoding`).
    pub name: String,
    /// The arguments, in order.
    pub args: Vec<AttrArg>,
    /// Source position of the `#`.
    pub pos: Pos,
}

/// An argument of an [`Attr`]: a bare key, or `key = value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrArg {
    /// The argument key (e.g. `domain`, or the bare scheme name `binary`).
    pub key: String,
    /// The optional value (present for `key = value`).
    pub value: Option<AttrValue>,
}

/// The value of an [`AttrArg`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    /// An identifier value, e.g. the domain `sys`.
    Ident(String),
    /// An integer literal value.
    Literal(u128),
}

/// A type reference in the functional surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A scalar integer type `uN` / `iN` (reuses the legacy [`Type`]).
    Scalar(Type),
    /// A fixed-point type `uI.F` / `iI.F` (DESIGN §6.1). For signed types the
    /// sign bit is the MSB of the `int_bits` field; total width = I + F.
    Fixed {
        /// Whether the type is signed (`i`) or unsigned (`u`).
        signed: bool,
        /// The integer-field bit count `I` (includes the sign bit if signed).
        int_bits: u32,
        /// The fractional-field bit count `F`.
        frac_bits: u32,
    },
    /// A named type (struct or enum), e.g. `Buttons`.
    Named(String),
    /// An `Option<T>` — "maybe a value" (DESIGN §6.5). Erases to a valid bit plus
    /// a `T` payload.
    Option(Box<TypeRef>),
    /// An array `[T; N]` of `N` elements (DESIGN §6.6 vector-of-registers).
    Array {
        /// The element type.
        elem: Box<TypeRef>,
        /// The length expression (a constant).
        len: Box<Expr>,
    },
    /// A tuple `(T, U, ...)`.
    Tuple(Vec<TypeRef>),
    /// A ported memory `mem<T, N>` (DESIGN §6.6/§6.9): an `N`-deep array of `T`
    /// that lowers to block RAM, *distinct* from the `[T; N]` vector-of-registers.
    /// Port count is part of the type; MEM1 fixes it at 1R1W (`read_ports` /
    /// `write_ports` = 1), with the fields present so `mem<T, N, R, W>` is additive
    /// later (DESIGN §9.6).
    Mem {
        /// The element type.
        elem: Box<TypeRef>,
        /// The depth (number of words) — a constant expression.
        depth: Box<Expr>,
        /// Number of read ports (MEM1: always 1).
        read_ports: u32,
        /// Number of write ports (MEM1: always 1).
        write_ports: u32,
    },
    /// A fixed-capacity channel (DESIGN §6.11 / §9.8): the `fifo<T, N>` /
    /// `ring<T, N>` / `flow<T>` family. A single-clock queue held in a struct
    /// field and accessed method-style (`front()` / `next(push, pop)` / …), with
    /// the loss policy chosen by [`ChannelKind`] rather than a call-site flag.
    /// Lowered by a source-to-source desugar (`channel_desugar`) into a plain
    /// `struct` + `impl` before type-checking, so no later stage sees this
    /// variant.
    Channel {
        /// Which member of the family — and so the loss policy and surface.
        kind: ChannelKind,
        /// The element type (a scalar).
        elem: Box<TypeRef>,
        /// The capacity (a power-of-two constant expression). A `flow<T>` is
        /// depth-1 by definition and writes no capacity in source, so the parser
        /// synthesizes the literal `1` for it.
        depth: Box<Expr>,
    },
}

/// Which member of the fixed-capacity channel family a [`TypeRef::Channel`] is
/// (DESIGN §6.11). The loss policy lives in the type — never in a call-site flag
/// — so a lossy queue reads as lossy at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    /// `fifo<T, N>` — lossless. A push into a full queue with no simultaneous pop
    /// is a guarded hold: the word is refused, live data is never overwritten.
    Fifo,
    /// `ring<T, N>` — overwrite-oldest. A push into a full ring drops the oldest
    /// word and always accepts. For trace/telemetry buffers where the newest data
    /// matters most.
    Ring,
    /// `flow<T>` — depth-1, newest-wins. A single valid slot read with `value()`;
    /// there is no pop. A "latest sample" register.
    Flow,
}

/// A block: zero or more statements followed by a tail expression (its value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The statements preceding the tail, in order.
    pub stmts: Vec<Stmt>,
    /// The tail expression whose value is the block's value.
    pub tail: Box<Expr>,
    /// Source position of the opening brace.
    pub pos: Pos,
}

/// A statement inside a [`Block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// A `let pattern [: ty] = value;` binding.
    Let {
        /// The binding pattern.
        pattern: Pattern,
        /// An optional type annotation.
        ty: Option<TypeRef>,
        /// The bound value.
        value: Expr,
        /// Source position of the `let` keyword.
        pos: Pos,
    },
    /// A `yield;` stage boundary in a `pipeline fn` (DESIGN §3.1). The optional
    /// expression taps a value out at that stage (reserved; not yet lowered).
    Yield {
        /// The optional tapped value.
        value: Option<Expr>,
        /// Source position of the `yield` keyword.
        pos: Pos,
    },
}

/// A pattern (in `let` bindings and `match` arms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// The wildcard `_`.
    Wildcard,
    /// A binding name, e.g. `ones` or `_tens_wrap`.
    Binding(String),
    /// A literal pattern, e.g. `0`.
    Literal(u128),
    /// A path pattern (enum variant), e.g. `Cmd::Up` (≥ 2 segments).
    Path(Vec<String>),
    /// A tuple pattern, e.g. `(ones, ones_wrap)`.
    Tuple(Vec<Pattern>),
    /// `Some(p)` — matches a present `Option`, binding the payload via `p`.
    Some(Box<Pattern>),
    /// `None` — matches the empty `Option`.
    None,
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    /// The field name.
    pub name: String,
    /// The value expression.
    pub value: Expr,
}

/// An arm of a [`Expr::Match`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// The pattern to match.
    pub pattern: Pattern,
    /// The arm body expression.
    pub body: Expr,
}

/// The mode of a narrowing [`Expr::Cast`] (DESIGN §6.3); no default is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastMode {
    /// Saturate to the nearest representable value.
    Sat,
    /// Wrap / truncate the high bits.
    Wrap,
    /// Truncate fractional bits.
    Trunc,
    /// Round to the nearest representable value.
    Round,
}
