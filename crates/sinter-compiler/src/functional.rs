//! Functional surface (DESIGN §2.1) front end — **F1: resolution + type
//! checking**.
//!
//! This pass validates a parsed functional design (state `struct`s, `enum`s,
//! `impl` transition methods, free `fn`s, `const`s) without lowering it. It
//! builds the symbol tables, resolves every name, type-checks each function
//! body with width-inferred arithmetic (DESIGN §6.2) and mode-named casts
//! (§6.3), and validates the unique `#[clock]` transition root.
//!
//! Deliberately deferred to later phases:
//! - lowering to `ir::Module` (F2);
//! - the single-step rule and combinational-acyclicity proof (F2 — they need
//!   the elaborated dataflow graph);
//! - the precise fixed-point fit/arithmetic rules (F4);
//! - multiple clock domains and CDC (later milestone).

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use sinter_syntax::ast::{
    BinOp, Block, CastMode, Expr, FnColor, Func, Item, Param, Pattern, Pos, Program, Stmt, Type,
    TypeRef, UnaryOp, Visibility,
};

use crate::error::CompileError;

/// The widest scalar the current backends support (`u64`-bounded evaluation).
const MAX_WIDTH: u32 = 64;

/// A resolved functional type. Unlike [`TypeRef`], named types are resolved to
/// struct/enum kinds and array lengths are evaluated to constants.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FType {
    /// `uN`.
    Uint(u32),
    /// `iN`.
    Int(u32),
    /// Fixed-point `uI.F` / `iI.F` (DESIGN §6.1).
    Fixed {
        signed: bool,
        int_bits: u32,
        frac_bits: u32,
    },
    /// A named struct type.
    Struct(String),
    /// A named enum type.
    Enum(String),
    /// An `Option<T>` (DESIGN §6.5) — erases to `(is_some: u1, payload: T)`.
    Option(Box<FType>),
    /// An array `[T; N]`.
    Array(Box<FType>, u64),
    /// A tuple `(T, U, …)`.
    Tuple(Vec<FType>),
    /// A ported memory (DESIGN §6.6/§6.9): `mem<T, N>` (1R1W, `writable`) or
    /// `rom<T, N>` (read-only, 1R0W). A read-only memory differs in type — its
    /// `next()` takes no write arguments and its contents are a compile-time
    /// initializer — so the writability is part of the type.
    Mem {
        /// Element (word) type.
        elem: Box<FType>,
        /// Depth in words.
        depth: u64,
        /// Whether the memory has a write port (`mem`) or is read-only (`rom`).
        writable: bool,
    },
}

impl FType {
    fn is_scalar(&self) -> bool {
        matches!(self, FType::Uint(_) | FType::Int(_) | FType::Fixed { .. })
    }

    /// The total bit width of a scalar type, or `None` for aggregates.
    fn scalar_width(&self) -> Option<u32> {
        match self {
            FType::Uint(w) | FType::Int(w) => Some(*w),
            FType::Fixed {
                int_bits,
                frac_bits,
                ..
            } => Some(int_bits + frac_bits),
            _ => None,
        }
    }

    fn is_signed(&self) -> bool {
        matches!(self, FType::Int(_) | FType::Fixed { signed: true, .. })
    }
}

impl fmt::Display for FType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FType::Uint(w) => write!(f, "u{w}"),
            FType::Int(w) => write!(f, "i{w}"),
            FType::Fixed {
                signed,
                int_bits,
                frac_bits,
            } => write!(
                f,
                "{}{int_bits}.{frac_bits}",
                if *signed { 'i' } else { 'u' }
            ),
            FType::Struct(n) | FType::Enum(n) => write!(f, "{n}"),
            FType::Mem {
                elem,
                depth,
                writable,
            } => write!(
                f,
                "{}<{elem}, {depth}>",
                if *writable { "mem" } else { "rom" }
            ),
            FType::Option(t) => write!(f, "Option<{t}>"),
            FType::Array(t, n) => write!(f, "[{t}; {n}]"),
            FType::Tuple(ts) => {
                write!(f, "(")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                write!(f, ")")
            }
        }
    }
}

/// A struct's fields, in declaration order: `(name, type, visibility)`.
struct StructInfo {
    fields: Vec<(String, FType, Visibility)>,
}

impl StructInfo {
    fn field(&self, name: &str) -> Option<&FType> {
        self.fields
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, t, _)| t)
    }

    fn field_vis(&self, name: &str) -> Option<Visibility> {
        self.fields
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, _, v)| *v)
    }
}

/// An enum's variants and bit-encoding.
struct EnumInfo {
    variants: Vec<String>,
}

/// A function or method signature (parameter types after any `self`).
struct FnSig {
    /// Whether the function takes a `self` receiver.
    has_self: bool,
    /// Parameter types (excluding `self`), in order.
    params: Vec<FType>,
    /// Return type.
    ret: FType,
    /// Visibility (DESIGN §10). Enforced for method calls; free-function calls
    /// are already gated by item visibility at name resolution (S2).
    vis: Visibility,
    /// Function color (DESIGN §3). A non-`Comb` function cannot be called
    /// directly — a pipeline is instantiated and advanced (PL3).
    color: FnColor,
}

/// The program's resolved symbol tables.
struct Tables {
    structs: HashMap<String, StructInfo>,
    enums: HashMap<String, EnumInfo>,
    /// Methods keyed by `(type_name, method_name)`.
    methods: HashMap<(String, String), FnSig>,
    funcs: HashMap<String, FnSig>,
    consts: HashMap<String, FType>,
}

/// Resolve and type-check the functional surface of `program`. A no-op when the
/// program has no functional items.
///
/// # Errors
/// Returns the first [`CompileError`] encountered.
pub fn check_functional(program: &Program) -> Result<(), CompileError> {
    if program.items.is_empty() {
        return Ok(());
    }
    // The module system parses today (S1) but is not yet resolved or lowered;
    // reject `mod` / `use` in the single-file compile path so they are never
    // silently dropped. Multi-file resolution is milestone S2/S3 (DESIGN §9.2).
    for item in &program.items {
        let (kind, pos) = match item {
            Item::Mod(m) => ("mod", m.pos),
            Item::Use(u) => ("use", u.pos),
            _ => continue,
        };
        return Err(sem(
            "module",
            pos,
            format!("`{kind}` is parsed but not yet lowered (milestone S2/S3)"),
        ));
    }
    let tables = build_tables(program)?;
    let checker = Checker::new(&tables);

    // Type-check every function body. Free functions may be combinational or a
    // staged color (DESIGN §3); methods must be combinational.
    for item in &program.items {
        match item {
            Item::Func(f) => match f.color {
                FnColor::Comb => checker.check_fn(f, None)?,
                FnColor::Pipeline => checker.check_pipeline(f)?,
                FnColor::Multiplex => checker.check_multiplex(f)?,
            },
            Item::Impl(im) => {
                for m in &im.methods {
                    if m.color != FnColor::Comb {
                        return Err(sem(
                            &m.name,
                            m.pos,
                            "a staged (`pipeline` / `multiplex`) function cannot be a method; \
                             declare it as a free `fn`"
                                .to_string(),
                        ));
                    }
                    checker.check_fn(m, Some(&im.type_name))?;
                }
            }
            _ => {}
        }
    }

    checker.check_clock_root(program)?;
    Ok(())
}

// --- Symbol-table construction --------------------------------------------

fn build_tables(program: &Program) -> Result<Tables, CompileError> {
    let mut structs = HashMap::new();
    let mut enums = HashMap::new();
    let mut consts: HashMap<String, FType> = HashMap::new();

    // Pass 1: collect type *names* so field/return types can reference them.
    let mut struct_names = Vec::new();
    let mut enum_names = Vec::new();
    for item in &program.items {
        match item {
            Item::Struct(s) => struct_names.push((s.name.clone(), s.pos)),
            Item::Enum(e) => enum_names.push((e.name.clone(), e.pos)),
            _ => {}
        }
    }
    let mut declared: HashMap<String, Pos> = HashMap::new();
    for (name, pos) in struct_names.iter().chain(enum_names.iter()) {
        if declared.insert(name.clone(), *pos).is_some() {
            return Err(sem(name, *pos, format!("duplicate type `{name}`")));
        }
    }
    let known: KnownTypes = KnownTypes {
        structs: struct_names.iter().map(|(n, _)| n.clone()).collect(),
        enums: enum_names.iter().map(|(n, _)| n.clone()).collect(),
    };

    // Pass 2: resolve struct fields, enum variants, and consts.
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                let mut fields = Vec::new();
                let mut seen = HashMap::new();
                for fld in &s.fields {
                    if seen.insert(fld.name.clone(), fld.pos).is_some() {
                        return Err(sem(
                            &s.name,
                            fld.pos,
                            format!("duplicate field `{}` in struct `{}`", fld.name, s.name),
                        ));
                    }
                    let ty = resolve_type(&known, &fld.ty, &s.name, fld.pos)?;
                    if is_mem_bearing(&ty) && !matches!(ty, FType::Mem { .. }) {
                        return Err(sem(
                            &s.name,
                            fld.pos,
                            "a memory must be a plain struct field, not nested in an array or \
                             tuple (MEM1)",
                        ));
                    }
                    fields.push((fld.name.clone(), ty, fld.vis));
                }
                structs.insert(s.name.clone(), StructInfo { fields });
            }
            Item::Enum(e) => {
                validate_encoding(e)?;
                let mut variants = Vec::new();
                let mut seen = HashMap::new();
                for v in &e.variants {
                    if seen.insert(v.name.clone(), v.pos).is_some() {
                        return Err(sem(
                            &e.name,
                            v.pos,
                            format!("duplicate variant `{}` in enum `{}`", v.name, e.name),
                        ));
                    }
                    variants.push(v.name.clone());
                }
                enums.insert(e.name.clone(), EnumInfo { variants });
            }
            Item::Const(c) => {
                let ty = resolve_type(&known, &c.ty, &c.name, c.pos)?;
                reject_mem_value(&ty, &c.name, c.pos)?;
                if consts.insert(c.name.clone(), ty).is_some() {
                    return Err(sem(&c.name, c.pos, format!("duplicate const `{}`", c.name)));
                }
            }
            _ => {}
        }
    }

    // Pass 3: resolve function and method signatures.
    let mut methods = HashMap::new();
    let mut funcs = HashMap::new();
    for item in &program.items {
        match item {
            Item::Func(f) => {
                let sig = resolve_sig(&known, f)?;
                if sig.has_self {
                    return Err(sem(
                        &f.name,
                        f.pos,
                        format!("free function `{}` cannot take `self`", f.name),
                    ));
                }
                if funcs.insert(f.name.clone(), sig).is_some() {
                    return Err(sem(
                        &f.name,
                        f.pos,
                        format!("duplicate function `{}`", f.name),
                    ));
                }
            }
            Item::Impl(im) => {
                if !known.structs.contains(&im.type_name) && !known.enums.contains(&im.type_name) {
                    return Err(sem(
                        &im.type_name,
                        im.pos,
                        format!("`impl` on unknown type `{}`", im.type_name),
                    ));
                }
                for m in &im.methods {
                    let sig = resolve_sig(&known, m)?;
                    let key = (im.type_name.clone(), m.name.clone());
                    if methods.insert(key, sig).is_some() {
                        return Err(sem(
                            &im.type_name,
                            m.pos,
                            format!("duplicate method `{}::{}`", im.type_name, m.name),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let tables = Tables {
        structs,
        enums,
        methods,
        funcs,
        consts,
    };
    // Re-check const values and struct-field reset values against their declared
    // types now that all tables exist (a value may reference another const). This
    // is what makes an out-of-range initializer (e.g. `u4 = 99`) a compile error
    // rather than a silently-masked reset (DESIGN §6.10).
    let checker = Checker::new(&tables);
    let env = HashMap::new();
    for item in &program.items {
        match item {
            Item::Const(c) => {
                checker.enter_module(&c.name);
                let want = tables
                    .consts
                    .get(&c.name)
                    .cloned()
                    .unwrap_or(FType::Uint(MAX_WIDTH));
                checker.check_expr(&c.value, &env, Some(&want), &c.name, c.pos)?;
            }
            Item::Struct(s) => {
                checker.enter_module(&s.name);
                for f in &s.fields {
                    let want = resolve_type(&checker.known(), &f.ty, &s.name, f.pos)?;
                    // A `mem` is zero-initialized (`= 0`); a `rom` carries its
                    // compile-time contents (an array literal or `include_hex!`). A
                    // `mem`/`rom` value does not "fit" via the normal literal check,
                    // so it is validated here, not by `check_expr`.
                    if let FType::Mem {
                        elem,
                        depth,
                        writable,
                    } = &want
                    {
                        checker.check_mem_field_default(
                            f.default.as_ref(),
                            elem,
                            *depth,
                            *writable,
                            &env,
                            &s.name,
                            f.pos,
                        )?;
                        continue;
                    }
                    if let Some(default) = &f.default {
                        checker.check_expr(default, &env, Some(&want), &s.name, f.pos)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(tables)
}

/// The module path (canonical-name prefix) of a canonical item name. Root items
/// (no `::`) live in the root module, returned as the empty string.
fn module_of(canonical: &str) -> &str {
    canonical.rsplit_once("::").map_or("", |(m, _)| m)
}

/// Whether an accessing module `acc` can reach a *private* item of module `def`:
/// `acc` is `def` or a descendant (DESIGN §10). A root-module (`""`) item is
/// reachable from everywhere, since every module descends the root.
fn mod_reaches(acc: &str, def: &str) -> bool {
    def.is_empty() || acc == def || acc.starts_with(&format!("{def}::"))
}

/// The set of declared type names, used while resolving [`TypeRef`]s.
struct KnownTypes {
    structs: std::collections::HashSet<String>,
    enums: std::collections::HashSet<String>,
}

fn resolve_sig(known: &KnownTypes, f: &Func) -> Result<FnSig, CompileError> {
    let mut has_self = false;
    let mut params = Vec::new();
    for p in &f.params {
        match p {
            Param::SelfRef { .. } => has_self = true,
            Param::Typed { ty, pos, .. } => {
                let pt = resolve_type(known, ty, &f.name, *pos)?;
                reject_mem_value(&pt, &f.name, *pos)?;
                params.push(pt);
            }
        }
    }
    let ret = resolve_type(known, &f.ret, &f.name, f.pos)?;
    reject_mem_value(&ret, &f.name, f.pos)?;
    Ok(FnSig {
        has_self,
        params,
        ret,
        vis: f.vis,
        color: f.color,
    })
}

/// Whether a type *is* or *contains* (in an array/tuple, but not through a named
/// struct) a memory. A memory is opaque, single-instance state; it may only be a
/// plain struct field, never a function parameter / return or an array/tuple
/// element (MEM1, DESIGN §9.6).
fn is_mem_bearing(t: &FType) -> bool {
    match t {
        FType::Mem { .. } => true,
        FType::Array(e, _) => is_mem_bearing(e),
        FType::Tuple(ts) => ts.iter().any(is_mem_bearing),
        _ => false,
    }
}

/// Reject a memory used as a function parameter or return type.
fn reject_mem_value(t: &FType, ctx: &str, pos: Pos) -> Result<(), CompileError> {
    if is_mem_bearing(t) {
        return Err(sem(
            ctx,
            pos,
            "a memory (`mem<…>`) can only be a struct field, not a function \
             parameter or return value (MEM1)",
        ));
    }
    Ok(())
}

fn resolve_type(
    known: &KnownTypes,
    tref: &TypeRef,
    ctx: &str,
    pos: Pos,
) -> Result<FType, CompileError> {
    let ty = match tref {
        TypeRef::Scalar(Type::Uint(w)) => FType::Uint(*w),
        TypeRef::Scalar(Type::Int(w)) => FType::Int(*w),
        TypeRef::Fixed {
            signed,
            int_bits,
            frac_bits,
        } => FType::Fixed {
            signed: *signed,
            int_bits: *int_bits,
            frac_bits: *frac_bits,
        },
        TypeRef::Named(n) => {
            if known.structs.contains(n) {
                FType::Struct(n.clone())
            } else if known.enums.contains(n) {
                FType::Enum(n.clone())
            } else {
                return Err(sem(ctx, pos, format!("unknown type `{n}`")));
            }
        }
        TypeRef::Option(t) => FType::Option(Box::new(resolve_type(known, t, ctx, pos)?)),
        TypeRef::Array { elem, len } => {
            let et = resolve_type(known, elem, ctx, pos)?;
            let n = const_len(len, "array length", ctx, pos)?;
            FType::Array(Box::new(et), n)
        }
        TypeRef::Tuple(ts) => FType::Tuple(
            ts.iter()
                .map(|t| resolve_type(known, t, ctx, pos))
                .collect::<Result<_, _>>()?,
        ),
        TypeRef::Mem {
            elem,
            depth,
            read_ports,
            write_ports,
        } => {
            // `mem` is 1R1W; `rom` is 1R0W. Anything else (multi-port) is deferred.
            if *read_ports != 1 || *write_ports > 1 {
                return Err(sem(
                    ctx,
                    pos,
                    "only a 1-read / 1-write (1R1W) `mem` or 1-read / 0-write (1R0W) `rom` is \
                     supported; multi-port memories are deferred (DESIGN §9.6/§9.7)",
                ));
            }
            let et = resolve_type(known, elem, ctx, pos)?;
            if !et.is_scalar() {
                return Err(sem(
                    ctx,
                    pos,
                    format!(
                        "a memory element must be a scalar (`uN`/`iN`/fixed-point), not `{et}`"
                    ),
                ));
            }
            let n = const_len(depth, "memory depth", ctx, pos)?;
            if n < 2 || !n.is_power_of_two() {
                return Err(sem(
                    ctx,
                    pos,
                    format!("memory depth `{n}` must be a power of two ≥ 2"),
                ));
            }
            FType::Mem {
                elem: Box::new(et),
                depth: n,
                writable: *write_ports == 1,
            }
        }
        TypeRef::Fifo { .. } => {
            // `fifo<T, N>` is lowered by the `fifo` desugar (DESIGN §9.8, CHAN1)
            // into a plain struct + impl before type-checking, so a surviving
            // `Fifo` type here is a compiler bug (a missed desugar).
            return Err(sem(
                ctx,
                pos,
                "internal error: `fifo<T, N>` should have been desugared before type-checking",
            ));
        }
    };
    if let Some(w) = ty.scalar_width() {
        if w == 0 {
            return Err(sem(ctx, pos, format!("type `{ty}` has zero width")));
        }
        if w > MAX_WIDTH {
            return Err(sem(
                ctx,
                pos,
                format!("type `{ty}` exceeds the current {MAX_WIDTH}-bit limit"),
            ));
        }
    }
    Ok(ty)
}

/// Evaluate an array length: a literal (F1 supports literals only here).
fn const_len(expr: &Expr, noun: &str, ctx: &str, pos: Pos) -> Result<u64, CompileError> {
    match expr {
        Expr::Literal(v) => {
            u64::try_from(*v).map_err(|_| sem(ctx, pos, format!("{noun} `{v}` is too large")))
        }
        _ => Err(sem(ctx, pos, format!("{noun} must be a literal"))),
    }
}

fn validate_encoding(e: &sinter_syntax::ast::EnumItem) -> Result<(), CompileError> {
    for attr in &e.attrs {
        if attr.name != "encoding" {
            continue;
        }
        for arg in &attr.args {
            if !matches!(arg.key.as_str(), "binary" | "onehot" | "gray" | "johnson") {
                return Err(sem(
                    &e.name,
                    attr.pos,
                    format!("unknown enum encoding `{}`", arg.key),
                ));
            }
        }
    }
    Ok(())
}

// --- Type checking ---------------------------------------------------------

struct Checker<'a> {
    tables: &'a Tables,
    /// The module path of the code currently being checked (the *accessing*
    /// module), derived from the enclosing item's canonical name. Used to
    /// enforce struct-field visibility (DESIGN §10). Set per item/function.
    acc_mod: RefCell<String>,
    /// Whether the function currently being checked returns an `Option<T>`. The
    /// `?` operator (DESIGN §6.4) is valid only in such a body. Set per function.
    in_option: RefCell<bool>,
}

/// Lexical environment: local/parameter/`self` names to their types.
type Env = HashMap<String, FType>;

impl<'a> Checker<'a> {
    fn new(tables: &'a Tables) -> Self {
        Checker {
            tables,
            acc_mod: RefCell::new(String::new()),
            in_option: RefCell::new(false),
        }
    }

    /// Set the accessing module for subsequent checks (the module owning the
    /// item about to be checked).
    fn enter_module(&self, canonical_item: &str) {
        *self.acc_mod.borrow_mut() = module_of(canonical_item).to_string();
    }

    /// Reject a read/construction of a private field from a module that cannot
    /// reach it (DESIGN §10 field visibility).
    fn check_field_access(
        &self,
        struct_name: &str,
        field: &str,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        let Some(vis) = self
            .tables
            .structs
            .get(struct_name)
            .and_then(|si| si.field_vis(field))
        else {
            return Ok(());
        };
        let acc = self.acc_mod.borrow();
        if vis == Visibility::Private && !mod_reaches(acc.as_str(), module_of(struct_name)) {
            return Err(sem(
                ctx,
                pos,
                format!("field `{field}` of struct `{struct_name}` is private"),
            ));
        }
        Ok(())
    }
}

impl Checker<'_> {
    fn check_fn(&self, f: &Func, self_ty: Option<&str>) -> Result<(), CompileError> {
        // The accessing module is where this code lives: a method's `self` type
        // module, or a free function's own module (both via canonical name).
        self.enter_module(self_ty.unwrap_or(&f.name));
        let mut env = Env::new();
        for p in &f.params {
            match p {
                Param::SelfRef { pos } => {
                    let Some(ty) = self_ty else {
                        return Err(sem(&f.name, *pos, "`self` outside an `impl`".to_string()));
                    };
                    env.insert("self".to_string(), FType::Struct(ty.to_string()));
                }
                Param::Typed { name, ty, pos } => {
                    let known = self.known();
                    env.insert(name.clone(), resolve_type(&known, ty, &f.name, *pos)?);
                }
            }
        }
        let known = self.known();
        let ret = resolve_type(&known, &f.ret, &f.name, f.pos)?;
        // `?` (DESIGN §6.4) is valid only when the enclosing function returns an
        // `Option` (so the propagated validity has somewhere to go).
        *self.in_option.borrow_mut() = matches!(ret, FType::Option(_));
        let got = self.check_block(&f.body, &env, Some(&ret), &f.name)?;
        self.expect(&ret, &got, &f.name, f.body.pos)?;
        Ok(())
    }

    /// Type-check a `pipeline fn` definition (DESIGN §3.1, PL2). The body is a
    /// per-datum dataflow; `yield;` stage boundaries are type-transparent here
    /// (the stage registers are inserted in PL3). The hazard rule (§3.1) holds
    /// structurally: a pipeline is a free function with no `self`, so it can only
    /// touch its inputs, `const`s, and its own locals. `yield` may only appear at
    /// the top level of the body — a `yield` nested in an `if`/`match`/block is
    /// rejected by `check_block` (data-dependent staging is deferred).
    fn check_pipeline(&self, f: &Func) -> Result<(), CompileError> {
        self.enter_module(&f.name);
        // `?` inside a `pipeline fn` (mid-stream bubbles) is a separate, deferred
        // feature; it is not an `Option`-returning combinational body here.
        *self.in_option.borrow_mut() = false;
        let known = self.known();
        let mut local = Env::new();
        for p in &f.params {
            match p {
                Param::SelfRef { pos } => {
                    return Err(sem(
                        &f.name,
                        *pos,
                        "a `pipeline fn` cannot take `self` (it is pure dataflow over its inputs)"
                            .to_string(),
                    ))
                }
                Param::Typed { name, ty, pos } => {
                    local.insert(name.clone(), resolve_type(&known, ty, &f.name, *pos)?);
                }
            }
        }
        let ret = resolve_type(&known, &f.ret, &f.name, f.pos)?;

        let mut stages = 0u32;
        for stmt in &f.body.stmts {
            match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => {
                    let want = match ty {
                        Some(t) => Some(resolve_type(&known, t, &f.name, *pos)?),
                        None => None,
                    };
                    let vt = self.check_expr(value, &local, want.as_ref(), &f.name, *pos)?;
                    self.bind_pattern(pattern, &vt, &mut local, &f.name, *pos)?;
                }
                Stmt::Yield { value, pos } => {
                    if value.is_some() {
                        return Err(sem(
                            &f.name,
                            *pos,
                            "`yield <expr>` taps are not yet supported (deferred); use a bare \
                             `yield;`"
                                .to_string(),
                        ));
                    }
                    stages += 1;
                }
            }
        }
        if stages == 0 {
            return Err(sem(
                &f.name,
                f.body.pos,
                "a `pipeline fn` needs at least one `yield` stage boundary".to_string(),
            ));
        }
        let got = self.check_expr(&f.body.tail, &local, Some(&ret), &f.name, f.body.pos)?;
        self.expect(&ret, &got, &f.name, f.body.pos)?;
        Ok(())
    }

    /// Type-check a `multiplex fn` definition (DESIGN §3.2, M1). Like a
    /// `pipeline fn` the body is a per-datum dataflow with type-transparent
    /// `yield` phase boundaries and no `self`; it differs only in *time-folded*
    /// lowering (one reused register per value + a phase counter, §3.3). The M1
    /// operator-sharing restriction (≤1 `*` per phase) is enforced by
    /// `multiplex_desugar` (which runs first); this pass type-checks the body.
    fn check_multiplex(&self, f: &Func) -> Result<(), CompileError> {
        self.enter_module(&f.name);
        let known = self.known();
        let mut local = Env::new();
        for p in &f.params {
            match p {
                Param::SelfRef { pos } => {
                    return Err(sem(
                        &f.name,
                        *pos,
                        "a `multiplex fn` cannot take `self` (it is pure dataflow over its inputs)"
                            .to_string(),
                    ))
                }
                Param::Typed { name, ty, pos } => {
                    local.insert(name.clone(), resolve_type(&known, ty, &f.name, *pos)?);
                }
            }
        }
        let ret = resolve_type(&known, &f.ret, &f.name, f.pos)?;

        let mut phases = 0u32;
        for stmt in &f.body.stmts {
            match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => {
                    let want = match ty {
                        Some(t) => Some(resolve_type(&known, t, &f.name, *pos)?),
                        None => None,
                    };
                    let vt = self.check_expr(value, &local, want.as_ref(), &f.name, *pos)?;
                    self.bind_pattern(pattern, &vt, &mut local, &f.name, *pos)?;
                }
                Stmt::Yield { value, pos } => {
                    if value.is_some() {
                        return Err(sem(
                            &f.name,
                            *pos,
                            "`yield <expr>` taps are not part of the multiplex model; use a bare \
                             `yield;`"
                                .to_string(),
                        ));
                    }
                    phases += 1;
                }
            }
        }
        if phases == 0 {
            return Err(sem(
                &f.name,
                f.body.pos,
                "a `multiplex fn` needs at least one `yield` phase boundary".to_string(),
            ));
        }
        let got = self.check_expr(&f.body.tail, &local, Some(&ret), &f.name, f.body.pos)?;
        self.expect(&ret, &got, &f.name, f.body.pos)?;
        Ok(())
    }

    fn known(&self) -> KnownTypes {
        KnownTypes {
            structs: self.tables.structs.keys().cloned().collect(),
            enums: self.tables.enums.keys().cloned().collect(),
        }
    }

    fn check_block(
        &self,
        block: &Block,
        env: &Env,
        expected: Option<&FType>,
        ctx: &str,
    ) -> Result<FType, CompileError> {
        let mut local = env.clone();
        for stmt in &block.stmts {
            let (pattern, ty, value, pos) = match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => (pattern, ty, value, *pos),
                // `yield` is a pipeline stage boundary; the pipeline color is
                // rejected up front (PL2/PL3 not lowered), so it cannot appear in
                // a checked (combinational) body.
                Stmt::Yield { pos, .. } => {
                    return Err(sem(
                        ctx,
                        *pos,
                        "`yield` is only valid in a `pipeline fn` (not yet lowered: PL2/PL3)"
                            .to_string(),
                    ))
                }
            };
            let want = match ty {
                Some(t) => Some(resolve_type(&self.known(), t, ctx, pos)?),
                None => None,
            };
            let vt = self.check_expr(value, &local, want.as_ref(), ctx, pos)?;
            self.bind_pattern(pattern, &vt, &mut local, ctx, pos)?;
        }
        self.check_expr(&block.tail, &local, expected, ctx, block.pos)
    }

    #[allow(clippy::too_many_lines)]
    fn check_expr(
        &self,
        expr: &Expr,
        env: &Env,
        expected: Option<&FType>,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let ty = match expr {
            // Expanded to an array before checking (via `compile_program`); reaching
            // here means it appeared outside a `rom` initializer.
            Expr::IncludeHex(_) => {
                return Err(sem(
                    ctx,
                    pos,
                    "`include_hex!` is only valid as a `rom` field initializer",
                ))
            }
            Expr::Literal(v) => {
                let want = expected.ok_or_else(|| {
                    sem(ctx, pos, format!("cannot infer a type for literal `{v}`"))
                })?;
                if !literal_fits(*v, want) {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("literal `{v}` does not fit in `{want}`"),
                    ));
                }
                want.clone()
            }
            Expr::Ident(name) => self.lookup(name, env, ctx, pos)?,
            Expr::Path(segs) => self.check_path(segs, ctx, pos)?,
            Expr::Field { base, field } => {
                let bt = self.check_expr(base, env, None, ctx, pos)?;
                let FType::Struct(s) = &bt else {
                    return Err(sem(ctx, pos, format!("`{bt}` has no fields")));
                };
                let fty = self
                    .tables
                    .structs
                    .get(s)
                    .and_then(|si| si.field(field))
                    .cloned()
                    .ok_or_else(|| sem(ctx, pos, format!("struct `{s}` has no field `{field}`")))?;
                self.check_field_access(s, field, ctx, pos)?;
                fty
            }
            Expr::Index { base, index } => {
                let bt = self.check_expr(base, env, None, ctx, pos)?;
                let FType::Array(elem, n) = &bt else {
                    return Err(sem(ctx, pos, format!("`{bt}` cannot be indexed")));
                };
                // The index is any scalar integer. A bare literal has nothing to
                // infer its width from, so (like a memory address, and since the
                // index is const-folded during elaboration) it adopts the natural
                // index width `u<bits(N)>`; a non-literal index self-infers.
                let hint = matches!(**index, Expr::Literal(_))
                    .then(|| FType::Uint((u64::BITS - n.leading_zeros()).max(1)));
                self.check_expr(index, env, hint.as_ref(), ctx, pos)?;
                (**elem).clone()
            }
            Expr::Call { func, args } => self.check_call(func, args, env, ctx, pos)?,
            Expr::MethodCall {
                receiver,
                method,
                args,
            } => self.check_method(receiver, method, args, env, ctx, pos)?,
            Expr::StructLit { name, fields } => {
                self.check_struct_lit(name, fields, env, ctx, pos)?
            }
            Expr::Tuple(items) => {
                let want = match expected {
                    Some(FType::Tuple(ts)) if ts.len() == items.len() => Some(ts),
                    _ => None,
                };
                let mut parts = Vec::with_capacity(items.len());
                for (i, e) in items.iter().enumerate() {
                    let exp = want.and_then(|ts| ts.get(i));
                    parts.push(self.check_expr(e, env, exp, ctx, pos)?);
                }
                FType::Tuple(parts)
            }
            Expr::Array(items) => self.check_array(items, expected, env, ctx, pos)?,
            Expr::Some(inner) => {
                // `Some(x)` has type `Option<typeof x>`; an expected `Option<T>`
                // hints the payload as `T`.
                let payload_hint = match expected {
                    Some(FType::Option(t)) => Some(t.as_ref()),
                    _ => None,
                };
                let it = self.check_expr(inner, env, payload_hint, ctx, pos)?;
                FType::Option(Box::new(it))
            }
            Expr::None => {
                match expected {
                    Some(FType::Option(t)) => FType::Option(t.clone()),
                    _ => return Err(sem(
                        ctx,
                        pos,
                        "cannot infer the type of `None`; give the surrounding `Option<T>` type",
                    )),
                }
            }
            Expr::Try(inner) => {
                // `?` yields the payload and folds the operand's validity into the
                // enclosing `Option`-returning function's result (DESIGN §6.4).
                if !*self.in_option.borrow() {
                    return Err(sem(
                        ctx,
                        pos,
                        "`?` is only valid in an `Option`-returning function (DESIGN §6.4)",
                    ));
                }
                let it = self.check_expr(inner, env, None, ctx, pos)?;
                let FType::Option(payload) = it else {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("`?` operand must be an `Option`, found `{it}`"),
                    ));
                };
                (*payload).clone()
            }
            Expr::Unary { op, expr: inner } => {
                self.check_unary(*op, inner, env, expected, ctx, pos)?
            }
            Expr::Binary { op, lhs, rhs } => {
                self.check_binary(*op, lhs, rhs, env, expected, ctx, pos)?
            }
            Expr::If { cond, then, els } => {
                self.check_expr(cond, env, Some(&FType::Uint(1)), ctx, pos)?;
                let t = self.check_expr(then, env, expected, ctx, pos)?;
                let e = self.check_expr(els, env, Some(&t), ctx, pos)?;
                self.expect(&t, &e, ctx, pos)?;
                t
            }
            Expr::Match { scrutinee, arms } => {
                self.check_match(scrutinee, arms, env, expected, ctx, pos)?
            }
            Expr::Cast {
                expr: inner,
                ty,
                mode,
            } => {
                let from = self.check_expr(inner, env, None, ctx, pos)?;
                self.check_cast(&from, ty, *mode, ctx, pos)?
            }
            Expr::Block(block) => self.check_block(block, env, expected, ctx)?,
        };
        if let Some(want) = expected {
            // Always confirm the value matches the context. For width-inferred
            // `+`/`-` this is exactly what forces a narrowing cast: `a + b`
            // widens by a bit, so assigning it to the operand width is an error
            // until the cast is written (DESIGN §6.2/§6.3). Operands of `+`/`-`
            // are NOT pushed the expectation (see `check_binary`), so the cast
            // operand keeps `None` and only the cast result is checked here.
            self.expect(want, &ty, ctx, pos)?;
        }
        Ok(ty)
    }

    fn check_path(&self, segs: &[String], ctx: &str, pos: Pos) -> Result<FType, CompileError> {
        let [enum_name, variant] = segs else {
            return Err(sem(
                ctx,
                pos,
                format!("unsupported path `{}`", segs.join("::")),
            ));
        };
        let info = self
            .tables
            .enums
            .get(enum_name)
            .ok_or_else(|| sem(ctx, pos, format!("unknown enum `{enum_name}`")))?;
        if !info.variants.iter().any(|v| v == variant) {
            return Err(sem(
                ctx,
                pos,
                format!("enum `{enum_name}` has no variant `{variant}`"),
            ));
        }
        Ok(FType::Enum(enum_name.clone()))
    }

    fn check_call(
        &self,
        func: &Expr,
        args: &[Expr],
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let Expr::Ident(name) = func else {
            return Err(sem(
                ctx,
                pos,
                "call target must be a function name".to_string(),
            ));
        };
        let sig = self
            .tables
            .funcs
            .get(name)
            .ok_or_else(|| sem(ctx, pos, format!("unknown function `{name}`")))?;
        if sig.color != FnColor::Comb {
            return Err(sem(
                ctx,
                pos,
                format!(
                    "`{name}` is a `pipeline fn` and cannot be called directly; instantiate it \
                     and `advance` it once per cycle (PL3)"
                ),
            ));
        }
        self.check_args(name, &sig.params, args, env, ctx, pos)?;
        Ok(sig.ret.clone())
    }

    fn check_method(
        &self,
        receiver: &Expr,
        method: &str,
        args: &[Expr],
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let recv = self.check_expr(receiver, env, None, ctx, pos)?;
        if let FType::Mem {
            elem,
            depth,
            writable,
        } = &recv
        {
            return self.check_mem_method(elem, *depth, *writable, method, args, env, ctx, pos);
        }
        let type_name = match &recv {
            FType::Struct(s) | FType::Enum(s) => s.clone(),
            _ => return Err(sem(ctx, pos, format!("`{recv}` has no methods"))),
        };
        let sig = self
            .tables
            .methods
            .get(&(type_name.clone(), method.to_string()))
            .ok_or_else(|| {
                sem(
                    ctx,
                    pos,
                    format!("type `{type_name}` has no method `{method}`"),
                )
            })?;
        if !sig.has_self {
            return Err(sem(
                ctx,
                pos,
                format!("`{type_name}::{method}` is an associated function, not a method"),
            ));
        }
        // A private method is callable only from its type's module or a
        // descendant (DESIGN §10). The defining module is the type's module —
        // an `impl` is co-located with its type (enforced in resolve).
        if sig.vis == Visibility::Private
            && !mod_reaches(self.acc_mod.borrow().as_str(), module_of(&type_name))
        {
            return Err(sem(
                ctx,
                pos,
                format!("method `{method}` of `{type_name}` is private"),
            ));
        }
        self.check_args(method, &sig.params, args, env, ctx, pos)?;
        Ok(sig.ret.clone())
    }

    /// Type-check the built-in memory methods (DESIGN §6.6/§9.6/§9.7).
    /// `value() -> T` (both `mem` and `rom`); `next(…) -> Self` is the transition —
    /// `next(raddr, we, waddr, wdata)` for a writable `mem`, `next(raddr)` for a
    /// read-only `rom`. The transition returns the memory so it threads into the
    /// tail `Self { m: self.m.next(…) }` (that threading commits it).
    #[allow(clippy::too_many_arguments)]
    fn check_mem_method(
        &self,
        elem: &FType,
        depth: u64,
        writable: bool,
        method: &str,
        args: &[Expr],
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let want_args = |n: usize| -> Result<(), CompileError> {
            if args.len() == n {
                Ok(())
            } else {
                Err(sem(
                    ctx,
                    pos,
                    format!(
                        "memory `{method}` expects {n} argument(s), found {}",
                        args.len()
                    ),
                ))
            }
        };
        // The address is any scalar integer (masked to the depth's address width
        // during elaboration). A bare literal has nothing to infer its width from,
        // so it adopts the natural address width `u<addr_bits>`; a non-literal
        // self-infers any scalar width.
        let addr_bits = depth.trailing_zeros();
        let check_addr = |this: &Self, e: &Expr| -> Result<(), CompileError> {
            let hint = matches!(e, Expr::Literal(_)).then_some(FType::Uint(addr_bits));
            let at = this.check_expr(e, env, hint.as_ref(), ctx, pos)?;
            this.require_scalar(&at, ctx, pos)
        };
        match method {
            // `value() -> T`: the current read-data register (last cycle's read).
            // A pure accessor — reading it does not consume the read port.
            "value" => {
                want_args(0)?;
                Ok(elem.clone())
            }
            // `next(…) -> Self`: the transition, threaded into the tail. A writable
            // `mem` takes `(raddr, we, waddr, wdata)`; a read-only `rom` takes just
            // `(raddr)` — its only mutable state is the read cursor.
            "next" => {
                if writable {
                    want_args(4)?;
                    check_addr(self, &args[0])?;
                    self.check_expr(&args[1], env, Some(&FType::Uint(1)), ctx, pos)?;
                    check_addr(self, &args[2])?;
                    self.check_expr(&args[3], env, Some(elem), ctx, pos)?;
                } else {
                    want_args(1)?;
                    check_addr(self, &args[0])?;
                }
                Ok(FType::Mem {
                    elem: Box::new(elem.clone()),
                    depth,
                    writable,
                })
            }
            _ => Err(sem(
                ctx,
                pos,
                format!("a memory has no method `{method}` (only `value`/`next`)"),
            )),
        }
    }

    /// Check a `mem`/`rom` field's value. Returns `true` if it was consumed as an
    /// *initializer* (a `mem` `= 0`, or a `rom`'s `[..]` contents); `false` if it
    /// is a *threaded* memory value to be type-checked normally. A non-zero `mem`
    /// literal is an error.
    #[allow(clippy::too_many_arguments)]
    fn check_mem_field_value(
        &self,
        value: &Expr,
        elem: &FType,
        depth: u64,
        writable: bool,
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<bool, CompileError> {
        if writable {
            match value {
                Expr::Literal(0) => Ok(true),
                Expr::Literal(_) => Err(sem(
                    ctx,
                    pos,
                    "a `mem` must be zero-initialized (`= 0`); use a `rom` for fixed contents",
                )),
                _ => Ok(false),
            }
        } else {
            match value {
                Expr::Array(items) => {
                    self.check_rom_init(items, elem, depth, env, ctx, pos)?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
    }

    /// Validate a `mem`/`rom` field default: a `mem` needs `= 0`, a `rom` needs its
    /// compile-time contents. A missing or malformed initializer is an error (a
    /// const default cannot be a threaded value).
    #[allow(clippy::too_many_arguments)]
    fn check_mem_field_default(
        &self,
        default: Option<&Expr>,
        elem: &FType,
        depth: u64,
        writable: bool,
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        let Some(value) = default else {
            return Err(sem(
                ctx,
                pos,
                if writable {
                    "a `mem` field needs an explicit `= 0` (zero-init)"
                } else {
                    "a `rom` field needs initial contents (`= [..]` or `= include_hex!(..)`)"
                },
            ));
        };
        if !self.check_mem_field_value(value, elem, depth, writable, env, ctx, pos)? {
            return Err(sem(
                ctx,
                pos,
                if writable {
                    "a `mem` must be zero-initialized (`= 0`)"
                } else {
                    "a `rom`'s contents must be an array literal `[..]` or `include_hex!(..)`"
                },
            ));
        }
        Ok(())
    }

    /// Validate `rom` contents: exactly `depth` words, each fitting the element
    /// type.
    fn check_rom_init(
        &self,
        items: &[Expr],
        elem: &FType,
        depth: u64,
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        if items.len() as u64 != depth {
            return Err(sem(
                ctx,
                pos,
                format!(
                    "a `rom` of depth {depth} needs {depth} words, but its initializer has {}",
                    items.len()
                ),
            ));
        }
        for it in items {
            self.check_expr(it, env, Some(elem), ctx, pos)?;
        }
        Ok(())
    }

    fn check_args(
        &self,
        callee: &str,
        params: &[FType],
        args: &[Expr],
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        if params.len() != args.len() {
            return Err(sem(
                ctx,
                pos,
                format!(
                    "`{callee}` expects {} argument(s), found {}",
                    params.len(),
                    args.len()
                ),
            ));
        }
        for (p, a) in params.iter().zip(args) {
            self.check_expr(a, env, Some(p), ctx, pos)?;
        }
        Ok(())
    }

    fn check_struct_lit(
        &self,
        name: &str,
        fields: &[sinter_syntax::ast::FieldInit],
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let info = self
            .tables
            .structs
            .get(name)
            .ok_or_else(|| sem(ctx, pos, format!("unknown struct `{name}`")))?;
        let mut seen = std::collections::HashSet::new();
        for fi in fields {
            let Some(fty) = info.field(&fi.name) else {
                return Err(sem(
                    ctx,
                    pos,
                    format!("struct `{name}` has no field `{}`", fi.name),
                ));
            };
            if !seen.insert(fi.name.clone()) {
                return Err(sem(
                    ctx,
                    pos,
                    format!("field `{}` set more than once", fi.name),
                ));
            }
            // Constructing with a private field is only allowed from a module
            // that can reach it (DESIGN §10).
            self.check_field_access(name, &fi.name, ctx, pos)?;
            // A `mem`/`rom` field value may be an *initializer* (a `mem` `= 0`, or a
            // `rom`'s array contents — neither "fits" the type via the literal
            // check) or a *threaded* memory value (e.g. `self.m.next(…)` in the
            // transition tail). The former is validated here; the latter falls
            // through to the normal check, which confirms the same memory type.
            if let FType::Mem {
                elem,
                depth,
                writable,
            } = fty
            {
                if self.check_mem_field_value(&fi.value, elem, *depth, *writable, env, ctx, pos)? {
                    continue;
                }
            }
            self.check_expr(&fi.value, env, Some(fty), ctx, pos)?;
        }
        if seen.len() != info.fields.len() {
            let missing: Vec<&str> = info
                .fields
                .iter()
                .map(|(n, _, _)| n.as_str())
                .filter(|n| !seen.contains(*n))
                .collect();
            return Err(sem(
                ctx,
                pos,
                format!(
                    "struct `{name}` is missing field(s): {}",
                    missing.join(", ")
                ),
            ));
        }
        Ok(FType::Struct(name.to_string()))
    }

    fn check_array(
        &self,
        items: &[Expr],
        expected: Option<&FType>,
        env: &Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let elem_hint = match expected {
            Some(FType::Array(e, _)) => Some((**e).clone()),
            _ => None,
        };
        let mut elem = elem_hint.clone();
        for e in items {
            let t = self.check_expr(e, env, elem.as_ref(), ctx, pos)?;
            match &elem {
                Some(prev) => self.expect(prev, &t, ctx, pos)?,
                None => elem = Some(t),
            }
        }
        let elem = elem.ok_or_else(|| {
            sem(
                ctx,
                pos,
                "cannot infer the element type of an empty array".to_string(),
            )
        })?;
        Ok(FType::Array(Box::new(elem), items.len() as u64))
    }

    fn check_unary(
        &self,
        op: UnaryOp,
        inner: &Expr,
        env: &Env,
        expected: Option<&FType>,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        match op {
            UnaryOp::Not => {
                self.check_expr(inner, env, Some(&FType::Uint(1)), ctx, pos)?;
                Ok(FType::Uint(1))
            }
            UnaryOp::BitNot => {
                let t = self.check_expr(inner, env, expected, ctx, pos)?;
                self.require_scalar(&t, ctx, pos)?;
                Ok(t)
            }
            UnaryOp::Neg => {
                let t = self.check_expr(inner, env, expected, ctx, pos)?;
                if !t.is_signed() {
                    return Err(sem(ctx, pos, format!("cannot negate unsigned `{t}`")));
                }
                Ok(t)
            }
        }
    }

    // Diagnostic context (`ctx`, `pos`) is threaded through the recursive
    // checkers; bundling it would obscure more than it saves here.
    #[allow(clippy::too_many_arguments)]
    fn check_binary(
        &self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        env: &Env,
        expected: Option<&FType>,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        if op.is_comparison() {
            let (l, r) = self.check_operands(lhs, rhs, env, None, ctx, pos)?;
            self.expect(&l, &r, ctx, pos)?;
            // Comparisons are over a scalar or an enum discriminant; reject the
            // aggregates (`Option`, tuple, struct, array) with a located F1 error
            // rather than letting elaboration bail later (`Option` is new in O1).
            if !(l.is_scalar() || matches!(l, FType::Enum(_))) {
                return Err(sem(
                    ctx,
                    pos,
                    format!("cannot compare `{l}`; comparison needs a scalar or enum"),
                ));
            }
            // Result is 1-bit; the caller's `expected` is checked by `check_expr`.
            return Ok(FType::Uint(1));
        }
        match op {
            BinOp::And | BinOp::Or | BinOp::Xor => {
                // Bitwise: same-type operands, result identical.
                let (l, r) = self.check_operands(lhs, rhs, env, expected, ctx, pos)?;
                self.expect(&l, &r, ctx, pos)?;
                self.require_scalar(&l, ctx, pos)?;
                Ok(l)
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                // Width inference (DESIGN §6.2). A bare literal adopts the other
                // operand's type; two concrete operands may differ in integer
                // width (e.g. nested `a*b + c*d`). The result widens, so storing
                // it back needs a cast.
                let (l, r) = self.check_operands(lhs, rhs, env, expected, ctx, pos)?;
                let (ls, li, lf) =
                    fx_parts(&l).ok_or_else(|| sem(ctx, pos, format!("`{l}` is not numeric")))?;
                let (rs, ri, rf) =
                    fx_parts(&r).ok_or_else(|| sem(ctx, pos, format!("`{r}` is not numeric")))?;
                if ls != rs {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("mixed signedness in arithmetic: `{l}` and `{r}`"),
                    ));
                }
                let (ibits, fbits) = if op == BinOp::Mul {
                    // Qa.b * Qc.d -> Q(a+c).(b+d).
                    (li + ri, lf + rf)
                } else {
                    // Q a.f +/- Q c.f -> Q (max(a,c)+1).f (fractional bits aligned).
                    if lf != rf {
                        return Err(sem(
                            ctx,
                            pos,
                            format!("`+`/`-` need matching fractional bits: `{l}` vs `{r}`"),
                        ));
                    }
                    (li.max(ri) + 1, lf)
                };
                if ibits + fbits > MAX_WIDTH {
                    return Err(sem(
                        ctx,
                        pos,
                        format!(
                            "result type `{}` exceeds the {MAX_WIDTH}-bit limit",
                            make_fx(ls, ibits, fbits)
                        ),
                    ));
                }
                Ok(make_fx(ls, ibits, fbits))
            }
            _ => Err(sem(ctx, pos, "unsupported binary operator".to_string())),
        }
    }

    fn check_match(
        &self,
        scrutinee: &Expr,
        arms: &[sinter_syntax::ast::MatchArm],
        env: &Env,
        expected: Option<&FType>,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        let st = self.check_expr(scrutinee, env, None, ctx, pos)?;
        let mut result: Option<FType> = None;
        let mut covered = std::collections::HashSet::new();
        let mut has_wildcard = false;
        for arm in arms {
            let mut local = env.clone();
            self.check_arm_pattern(
                &arm.pattern,
                &st,
                &mut local,
                &mut covered,
                &mut has_wildcard,
                ctx,
                pos,
            )?;
            let exp = expected.or(result.as_ref());
            let bt = self.check_expr(&arm.body, &local, exp, ctx, pos)?;
            match &result {
                Some(prev) => self.expect(prev, &bt, ctx, pos)?,
                None => result = Some(bt),
            }
        }
        // Exhaustiveness: an enum match must cover every variant or have a
        // catch-all; a scalar match must have a catch-all.
        if !has_wildcard {
            match &st {
                FType::Enum(e) => {
                    if let Some(info) = self.tables.enums.get(e) {
                        let missing: Vec<&str> = info
                            .variants
                            .iter()
                            .map(String::as_str)
                            .filter(|v| !covered.contains(*v))
                            .collect();
                        if !missing.is_empty() {
                            return Err(sem(
                                ctx,
                                pos,
                                format!(
                                    "non-exhaustive match on `{e}`: missing {}",
                                    missing.join(", ")
                                ),
                            ));
                        }
                    }
                }
                FType::Option(_) => {
                    let missing: Vec<&str> = ["Some", "None"]
                        .into_iter()
                        .filter(|v| !covered.contains(*v))
                        .collect();
                    if !missing.is_empty() {
                        return Err(sem(
                            ctx,
                            pos,
                            format!(
                                "non-exhaustive match on `{st}`: missing {}",
                                missing.join(", ")
                            ),
                        ));
                    }
                }
                _ => {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("non-exhaustive match on `{st}`: add a `_` arm"),
                    ))
                }
            }
        }
        result.ok_or_else(|| sem(ctx, pos, "match has no arms".to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    fn check_arm_pattern(
        &self,
        pat: &Pattern,
        st: &FType,
        env: &mut Env,
        covered: &mut std::collections::HashSet<String>,
        has_wildcard: &mut bool,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        match pat {
            Pattern::Wildcard => *has_wildcard = true,
            Pattern::Binding(name) => {
                *has_wildcard = true;
                env.insert(name.clone(), st.clone());
            }
            Pattern::Literal(v) => {
                if !literal_fits(*v, st) {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("pattern `{v}` does not fit scrutinee `{st}`"),
                    ));
                }
            }
            Pattern::Path(segs) => {
                let pty = self.check_path(segs, ctx, pos)?;
                self.expect(st, &pty, ctx, pos)?;
                if let [_, variant] = segs.as_slice() {
                    covered.insert(variant.clone());
                }
            }
            Pattern::Tuple(pats) => {
                let FType::Tuple(ts) = st else {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("tuple pattern against non-tuple `{st}`"),
                    ));
                };
                if ts.len() != pats.len() {
                    return Err(sem(ctx, pos, "tuple pattern arity mismatch".to_string()));
                }
                for (p, t) in pats.iter().zip(ts) {
                    self.check_arm_pattern(p, t, env, covered, has_wildcard, ctx, pos)?;
                }
            }
            // `Some(p)` / `None` cover one arm of an `Option` scrutinee — neither is
            // a `_`-style wildcard for the whole match. `Some`'s payload pattern is
            // an irrefutable binding (a refutable inner pattern is deferred).
            Pattern::Some(inner) => {
                let FType::Option(payload) = st else {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("`Some(..)` pattern against non-`Option` `{st}`"),
                    ));
                };
                self.bind_pattern(inner, payload, env, ctx, pos)?;
                covered.insert("Some".to_string());
            }
            Pattern::None => {
                if !matches!(st, FType::Option(_)) {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("`None` pattern against non-`Option` `{st}`"),
                    ));
                }
                covered.insert("None".to_string());
            }
        }
        Ok(())
    }

    fn check_cast(
        &self,
        from: &FType,
        to: &TypeRef,
        _mode: CastMode,
        ctx: &str,
        pos: Pos,
    ) -> Result<FType, CompileError> {
        self.require_scalar(from, ctx, pos)?;
        let to = resolve_type(&self.known(), to, ctx, pos)?;
        if !to.is_scalar() {
            return Err(sem(ctx, pos, format!("cannot cast to non-scalar `{to}`")));
        }
        Ok(to)
    }

    /// Bind a `let` pattern's names to component types of `ty`.
    fn bind_pattern(
        &self,
        pat: &Pattern,
        ty: &FType,
        env: &mut Env,
        ctx: &str,
        pos: Pos,
    ) -> Result<(), CompileError> {
        match pat {
            Pattern::Wildcard => {}
            Pattern::Binding(name) => {
                env.insert(name.clone(), ty.clone());
            }
            Pattern::Tuple(pats) => {
                let FType::Tuple(ts) = ty else {
                    return Err(sem(
                        ctx,
                        pos,
                        format!("tuple pattern binds non-tuple `{ty}`"),
                    ));
                };
                if ts.len() != pats.len() {
                    return Err(sem(ctx, pos, "tuple pattern arity mismatch".to_string()));
                }
                for (p, t) in pats.iter().zip(ts) {
                    self.bind_pattern(p, t, env, ctx, pos)?;
                }
            }
            Pattern::Literal(_) | Pattern::Path(_) | Pattern::Some(_) | Pattern::None => {
                return Err(sem(ctx, pos, "refutable pattern in `let`".to_string()))
            }
        }
        Ok(())
    }

    /// Validate the unique `#[clock]` transition root.
    fn check_clock_root(&self, program: &Program) -> Result<(), CompileError> {
        let mut roots = Vec::new();
        for item in &program.items {
            if let Item::Impl(im) = item {
                for m in &im.methods {
                    if m.attrs.iter().any(|a| a.name == "clock") {
                        roots.push((im.type_name.clone(), m));
                    }
                }
            }
        }
        let [(type_name, m)] = roots.as_slice() else {
            if roots.is_empty() {
                return Ok(()); // a library of helpers with no clock root is fine
            }
            return Err(sem(
                "clock",
                roots[1].1.pos,
                "more than one `#[clock]` root (multiple clock domains are not yet supported)"
                    .to_string(),
            ));
        };
        if !m.params.iter().any(|p| matches!(p, Param::SelfRef { .. })) {
            return Err(sem(
                &m.name,
                m.pos,
                "a `#[clock]` transition root must take `self`".to_string(),
            ));
        }
        // The return must be the state type (`-> Self`) or `(Self, Outputs)`.
        let ok = match &m.ret {
            TypeRef::Named(n) => n == type_name,
            TypeRef::Tuple(ts) => matches!(ts.first(), Some(TypeRef::Named(n)) if n == type_name),
            _ => false,
        };
        if !ok {
            return Err(sem(
                &m.name,
                m.pos,
                format!(
                    "`#[clock]` root `{type_name}::{}` must return `{type_name}` or \
                     `({type_name}, Outputs)`",
                    m.name
                ),
            ));
        }
        Ok(())
    }

    // --- small helpers -----------------------------------------------------

    fn lookup(&self, name: &str, env: &Env, ctx: &str, pos: Pos) -> Result<FType, CompileError> {
        env.get(name)
            .or_else(|| self.tables.consts.get(name))
            .cloned()
            .ok_or_else(|| sem(ctx, pos, format!("unknown name `{name}`")))
    }

    fn expect(&self, want: &FType, got: &FType, ctx: &str, pos: Pos) -> Result<(), CompileError> {
        if want == got {
            Ok(())
        } else {
            Err(sem(
                ctx,
                pos,
                format!("type mismatch: expected `{want}`, found `{got}`"),
            ))
        }
    }

    fn require_scalar(&self, ty: &FType, ctx: &str, pos: Pos) -> Result<(), CompileError> {
        if ty.is_scalar() {
            Ok(())
        } else {
            Err(sem(
                ctx,
                pos,
                format!("expected a scalar value, found `{ty}`"),
            ))
        }
    }

    /// Type-check the two operands of a binary operator. A bare integer literal
    /// adopts the other operand's type; a non-literal operand always self-infers
    /// (passing no expectation), so nested width-inferred arithmetic — e.g.
    /// `a*b + c*d` — is accepted and operands may differ in integer width.
    fn check_operands(
        &self,
        lhs: &Expr,
        rhs: &Expr,
        env: &Env,
        both_literal_hint: Option<&FType>,
        ctx: &str,
        pos: Pos,
    ) -> Result<(FType, FType), CompileError> {
        let lhs_lit = matches!(lhs, Expr::Literal(_));
        let rhs_lit = matches!(rhs, Expr::Literal(_));
        match (lhs_lit, rhs_lit) {
            (true, true) => Ok((
                self.check_expr(lhs, env, both_literal_hint, ctx, pos)?,
                self.check_expr(rhs, env, both_literal_hint, ctx, pos)?,
            )),
            (true, false) => {
                let r = self.check_expr(rhs, env, None, ctx, pos)?;
                let l = self.check_expr(lhs, env, Some(&r), ctx, pos)?;
                Ok((l, r))
            }
            (false, _) => {
                let l = self.check_expr(lhs, env, None, ctx, pos)?;
                let r = self.check_expr(rhs, env, rhs_lit.then_some(&l), ctx, pos)?;
                Ok((l, r))
            }
        }
    }
}

/// The (signed, integer-bits, fractional-bits) of a numeric type. Integers are
/// fixed-point with zero fractional bits.
fn fx_parts(ty: &FType) -> Option<(bool, u32, u32)> {
    match ty {
        FType::Uint(n) => Some((false, *n, 0)),
        FType::Int(n) => Some((true, *n, 0)),
        FType::Fixed {
            signed,
            int_bits,
            frac_bits,
        } => Some((*signed, *int_bits, *frac_bits)),
        _ => None,
    }
}

/// Build a numeric type, collapsing zero fractional bits to a plain integer.
fn make_fx(signed: bool, ibits: u32, fbits: u32) -> FType {
    if fbits == 0 {
        if signed {
            FType::Int(ibits)
        } else {
            FType::Uint(ibits)
        }
    } else {
        FType::Fixed {
            signed,
            int_bits: ibits,
            frac_bits: fbits,
        }
    }
}

/// Whether a raw integer literal fits a numeric type's erased bit width. A
/// fixed-point `iI.F`/`uI.F` literal is the raw stored value, so it fits the
/// `(I+F)`-bit two's-complement / unsigned range — the same check as `iN`/`uN`.
fn literal_fits(v: u128, ty: &FType) -> bool {
    match fx_parts(ty) {
        Some((false, ib, fb)) => {
            let w = ib + fb;
            w >= 128 || v < (1u128 << w)
        }
        Some((true, ib, fb)) => {
            let w = ib + fb;
            w >= 1 && (w > 127 || v < (1u128 << (w - 1)))
        }
        None => false,
    }
}

fn sem(ctx: &str, pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, pos, msg)
}
