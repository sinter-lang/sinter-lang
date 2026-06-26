//! Functional surface (DESIGN §2.1) — **F2/F3: elaboration to the IR**.
//!
//! Lowers a type-checked functional design to the same flat [`ir::Module`] the
//! legacy `module` surface produces, so the simulator, Verilog backend, and
//! conformance oracle are reused unchanged.
//!
//! The technique is **symbolic inlining + scalar replacement of aggregates
//! (SROA)**. Every value is a `Val` tree whose leaves are scalar IR
//! expressions over the registers and inputs:
//!
//! - the `#[clock]` transition root's state struct is flattened to leaf scalar
//!   **registers** (`tens.value` → `tens_value`, `seg[0]` → `seg_0`, …);
//! - the body's `let`s, sub-state **method calls**, and **free-function calls**
//!   are inlined (closed over their parameters), so each register's next-state
//!   is one scalar expression in terms of current registers and inputs;
//! - `if` / `match` become per-leaf multiplexers; `enum` values are their
//!   discriminant scalar;
//! - the tail `Self { … }` (or `(Self, Outputs)`) gives each register's
//!   next-state, and every leaf register is exposed as an observation output
//!   (`<field>_q`) so the oracle diffs the full state each cycle.
//!
//! Single-driver and acyclicity hold structurally: the tail struct literal
//! assigns each field exactly once (checked in F1), and reads are always of the
//! *current* register value, so feedback only ever passes through a register.
//!
//! Still deferred: fixed-point arithmetic, `sat`/`trunc`/`round` casts, and
//! *widening* casts (all F4 — `wrap` casts must narrow today); the pin map (F5);
//! multiple clock domains.

use std::collections::HashMap;

use sinter_syntax::ast::{
    AttrValue, BinOp, CastMode, EnumItem, Expr, Func, Item, Param, Pattern, Pos, Program, Stmt,
    StructItem, Type, TypeRef, UnaryOp,
};

use crate::error::{mask, CompileError};
use crate::ir;

/// Guards against accidental unbounded inlining (recursion is not synthesizable).
const MAX_INLINE_DEPTH: usize = 256;

/// All IR scalar widths must fit `eval`'s `u64` (DESIGN §6.2). Mirrors
/// `check::MAX_WIDTH` / `functional::MAX_WIDTH`.
const MAX_WIDTH: u32 = 64;

/// A resolved functional type.
#[derive(Debug, Clone)]
enum Ty {
    Scalar(Type),
    /// A fixed-point type `uI.F` / `iI.F`: it erases to an `(I+F)`-bit integer
    /// in the IR; the binary point (`fbits`) is tracked only here.
    Fixed {
        signed: bool,
        ibits: u32,
        fbits: u32,
    },
    /// An enum: its name and the bit width of its discriminant encoding.
    Enum(String, u32),
    Struct(String),
    Array(Box<Ty>, usize),
    Tuple(Vec<Ty>),
}

impl Ty {
    /// The IR (erased) integer type of a scalar/fixed/enum type.
    fn scalar(&self) -> Option<Type> {
        match self {
            Ty::Scalar(t) => Some(*t),
            Ty::Enum(_, w) => Some(Type::Uint(*w)),
            Ty::Fixed {
                signed,
                ibits,
                fbits,
            } => Some(int_ty(*signed, ibits + fbits)),
            _ => None,
        }
    }

    /// The fractional-bit count of a scalar/fixed type (0 for plain integers).
    fn fbits(&self) -> u32 {
        match self {
            Ty::Fixed { fbits, .. } => *fbits,
            _ => 0,
        }
    }
}

/// The IR integer type of `width` bits with the given signedness.
fn int_ty(signed: bool, width: u32) -> Type {
    if signed {
        Type::Int(width)
    } else {
        Type::Uint(width)
    }
}

/// A scalarized value: leaves are scalar IR expressions.
#[derive(Debug, Clone)]
enum Val {
    /// A scalar (or fixed-point) value: its IR expression plus the fractional
    /// bit count (`0` for a plain integer). The expression's IR width is
    /// `ibits + fbits`.
    Scalar(ir::Expr, u32),
    /// An enum value, carried as its discriminant scalar.
    Enum(String, ir::Expr),
    Struct(String, Vec<(String, Val)>),
    Tuple(Vec<Val>),
    Array(Vec<Val>),
}

/// Symbol tables holding references into the parsed program.
struct Tables<'p> {
    structs: HashMap<String, &'p StructItem>,
    enums: HashMap<String, &'p EnumItem>,
    methods: HashMap<(String, String), &'p Func>,
    funcs: HashMap<String, &'p Func>,
}

/// Lexical environment: names (`self`, parameters, `let`s) to their values.
type Env = HashMap<String, Val>;

/// Elaborate the functional surface of `program` into an [`ir::Module`].
///
/// Assumes `program` has already passed `functional::check_functional`.
///
/// # Errors
/// Returns a [`CompileError`] if the design uses a construct not yet lowered.
pub fn elaborate(program: &Program) -> Result<Option<ir::Module>, CompileError> {
    let tables = build_tables(program);
    let Some((state_ty, tick, domain)) = find_clock_root(program)? else {
        return Ok(None);
    };
    let el = Elaborator {
        ctx: state_ty.to_string(),
        tables,
        consts: const_values(program)?,
    };
    el.build(state_ty, tick, &domain).map(Some)
}

fn build_tables(program: &Program) -> Tables<'_> {
    let mut t = Tables {
        structs: HashMap::new(),
        enums: HashMap::new(),
        methods: HashMap::new(),
        funcs: HashMap::new(),
    };
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                t.structs.insert(s.name.clone(), s);
            }
            Item::Enum(e) => {
                t.enums.insert(e.name.clone(), e);
            }
            Item::Func(f) => {
                t.funcs.insert(f.name.clone(), f);
            }
            Item::Impl(im) => {
                for m in &im.methods {
                    t.methods.insert((im.type_name.clone(), m.name.clone()), m);
                }
            }
            Item::Const(_) => {}
        }
    }
    t
}

fn find_clock_root(program: &Program) -> Result<Option<(&str, &Func, String)>, CompileError> {
    let mut found = None;
    for item in &program.items {
        if let Item::Impl(im) = item {
            for m in &im.methods {
                if let Some(attr) = m.attrs.iter().find(|a| a.name == "clock") {
                    let domain = attr
                        .args
                        .iter()
                        .find(|a| a.key == "domain")
                        .and_then(|a| match &a.value {
                            Some(AttrValue::Ident(d)) => Some(d.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "sys".to_string());
                    if found.is_some() {
                        return Err(err(&im.type_name, m.pos, "more than one `#[clock]` root"));
                    }
                    found = Some((im.type_name.as_str(), m, domain));
                }
            }
        }
    }
    Ok(found)
}

struct Elaborator<'p> {
    ctx: String,
    tables: Tables<'p>,
    consts: HashMap<String, (u64, Type)>,
}

impl Elaborator<'_> {
    fn build(&self, state_ty: &str, tick: &Func, domain: &str) -> Result<ir::Module, CompileError> {
        let state = self.resolve_named(state_ty, tick.pos)?;

        // Registers: the state struct flattened to leaf scalars, reset from the
        // (possibly nested) default values.
        let mut leaves = Vec::new();
        self.flatten_ty(&state, "", &mut leaves);
        let resets = self.state_resets(state_ty, tick.pos)?;
        let mut regs = Vec::new();
        let mut seen = HashMap::new();
        for (name, ty) in &leaves {
            if seen.insert(name.clone(), ()).is_some() {
                return Err(err(
                    &self.ctx,
                    tick.pos,
                    format!("flattened state name `{name}` collides; rename a field"),
                ));
            }
            let reset = *resets
                .get(name)
                .ok_or_else(|| err(&self.ctx, tick.pos, format!("no reset for `{name}`")))?;
            regs.push(ir::Reg {
                name: name.clone(),
                ty: *ty,
                reset,
            });
        }

        // Environment: `self` (current registers) and inputs (flattened params).
        let mut env: Env = HashMap::new();
        env.insert("self".to_string(), self.idents_val(&state, ""));
        let mut ports = Vec::new();
        for p in &tick.params {
            if let Param::Typed { name, ty, pos } = p {
                let pty = self.resolve(ty, *pos)?;
                let mut pleaves = Vec::new();
                self.flatten_ty(&pty, name, &mut pleaves);
                for (pname, pty) in pleaves {
                    // `clk`/`rst_n` are the module's reserved clock/reset ports
                    // (emitted by the Verilog backend); an input flattening to
                    // either would duplicate-declare a port.
                    if pname == "clk" || pname == "rst_n" {
                        return Err(err(
                            &self.ctx,
                            *pos,
                            format!(
                                "input `{pname}` collides with the reserved clock/reset port; \
                                 rename the parameter or field"
                            ),
                        ));
                    }
                    ports.push(ir::Port {
                        dir: sinter_syntax::ast::Dir::In,
                        name: pname,
                        ty: pty,
                    });
                }
                env.insert(name.clone(), self.idents_val(&pty, name));
            }
        }

        // Body: inline `let`s into the environment, then lower the tail.
        for stmt in &tick.body.stmts {
            let Stmt::Let {
                pattern,
                value,
                pos,
                ..
            } = stmt;
            let v = self.lower(&env, value, None, 0, *pos)?;
            self.bind(pattern, v, &mut env, *pos)?;
        }
        let result = self.lower(&env, &tick.body.tail, Some(&state), 0, tick.body.pos)?;

        // The result is `Self` or `(Self, Outputs)`. The first is the next
        // state; the optional second is extra observation outputs.
        let (next_state, extra_out) = match result {
            Val::Struct(..) => (result, None),
            Val::Tuple(mut parts) if parts.len() == 2 => {
                let out = parts.pop();
                let st = parts.pop().expect("two parts");
                (st, out)
            }
            _ => {
                return Err(err(
                    &self.ctx,
                    tick.body.pos,
                    "transition root must return `Self` or `(Self, Outputs)`",
                ))
            }
        };

        // Register next-state from the returned state value.
        let mut next = Vec::new();
        flatten_val(&next_state, "", &mut next);
        let next_by_name: HashMap<_, _> = next.into_iter().collect();
        let mut reg_updates = Vec::new();
        for r in &regs {
            let value = next_by_name.get(&r.name).cloned().ok_or_else(|| {
                err(
                    &self.ctx,
                    tick.body.pos,
                    format!("no next-state for `{}`", r.name),
                )
            })?;
            reg_updates.push((r.name.clone(), value));
        }

        // Observation outputs: every register, plus any explicit outputs.
        let mut out_drives = Vec::new();
        for r in &regs {
            let name = format!("{}_q", r.name);
            ports.push(ir::Port {
                dir: sinter_syntax::ast::Dir::Out,
                name: name.clone(),
                ty: r.ty,
            });
            out_drives.push((name, typed(ir::ExprKind::Ident(r.name.clone()), r.ty)));
        }
        if let Some(out) = extra_out {
            let mut outs = Vec::new();
            flatten_val(&out, "out", &mut outs);
            for (name, value) in outs {
                ports.push(ir::Port {
                    dir: sinter_syntax::ast::Dir::Out,
                    name: name.clone(),
                    ty: value.ty,
                });
                out_drives.push((name, value));
            }
        }

        Ok(ir::Module {
            name: state_ty.to_string(),
            clock_domain: domain.to_string(),
            ports,
            regs,
            lets: Vec::new(),
            out_drives,
            reg_updates,
        })
    }

    // --- expression lowering ----------------------------------------------

    fn lower(
        &self,
        env: &Env,
        e: &Expr,
        expected: Option<&Ty>,
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        if depth > MAX_INLINE_DEPTH {
            return Err(err(&self.ctx, pos, "inlining too deep (recursive call?)"));
        }
        match e {
            Expr::Literal(v) => {
                let ty = expected
                    .ok_or_else(|| err(&self.ctx, pos, format!("cannot infer type of `{v}`")))?;
                let it = ty
                    .scalar()
                    .ok_or_else(|| err(&self.ctx, pos, format!("`{v}` needs a numeric type")))?;
                Ok(Val::Scalar(self.lit(*v, it, pos)?, ty.fbits()))
            }
            Expr::Ident(name) => {
                if let Some(v) = env.get(name) {
                    Ok(v.clone())
                } else if let Some((v, ty)) = self.consts.get(name) {
                    Ok(Val::Scalar(typed(ir::ExprKind::Literal(*v), *ty), 0))
                } else {
                    Err(err(&self.ctx, pos, format!("unknown name `{name}`")))
                }
            }
            Expr::Path(segs) => self.lower_path(segs, pos),
            Expr::Field { base, field } => {
                let b = self.lower(env, base, None, depth, pos)?;
                self.field(&b, field, pos)
            }
            Expr::Index { base, index } => {
                let b = self.lower(env, base, None, depth, pos)?;
                let Val::Array(items) = b else {
                    return Err(err(&self.ctx, pos, "indexing a non-array"));
                };
                let i = self.const_index(index, pos)?;
                items
                    .into_iter()
                    .nth(i)
                    .ok_or_else(|| err(&self.ctx, pos, format!("index {i} out of bounds")))
            }
            Expr::Call { func, args } => self.lower_call(env, func, args, depth, pos),
            Expr::MethodCall {
                receiver,
                method,
                args,
            } => self.lower_method(env, receiver, method, args, depth, pos),
            Expr::StructLit { name, fields } => {
                let sty = self.resolve_named(name, pos)?;
                let mut out = Vec::new();
                for (fname, fty) in self.struct_fields(name, pos)? {
                    let init = fields
                        .iter()
                        .find(|fi| fi.name == fname)
                        .ok_or_else(|| err(&self.ctx, pos, format!("missing field `{fname}`")))?;
                    out.push((
                        fname.clone(),
                        self.lower(env, &init.value, Some(&fty), depth, pos)?,
                    ));
                }
                let _ = sty;
                Ok(Val::Struct(name.clone(), out))
            }
            Expr::Tuple(items) => {
                let hint = match expected {
                    Some(Ty::Tuple(ts)) if ts.len() == items.len() => Some(ts),
                    _ => None,
                };
                let mut out = Vec::new();
                for (i, it) in items.iter().enumerate() {
                    out.push(self.lower(env, it, hint.and_then(|ts| ts.get(i)), depth, pos)?);
                }
                Ok(Val::Tuple(out))
            }
            Expr::Array(items) => {
                let hint = match expected {
                    Some(Ty::Array(et, _)) => Some((**et).clone()),
                    _ => None,
                };
                let mut out = Vec::new();
                for it in items {
                    out.push(self.lower(env, it, hint.as_ref(), depth, pos)?);
                }
                Ok(Val::Array(out))
            }
            Expr::Unary { op, expr: inner } => {
                self.lower_unary(env, *op, inner, expected, depth, pos)
            }
            Expr::Binary { op, lhs, rhs } => {
                self.lower_binary(env, *op, lhs, rhs, expected, depth, pos)
            }
            Expr::If { cond, then, els } => {
                let c = self.lower_scalar(env, cond, Some(Type::Uint(1)), depth, pos)?;
                let t = self.lower(env, then, expected, depth, pos)?;
                let e = self.lower(env, els, expected, depth, pos)?;
                self.mux(&c, &t, &e, pos)
            }
            Expr::Match { scrutinee, arms } => {
                self.lower_match(env, scrutinee, arms, expected, depth, pos)
            }
            Expr::Cast {
                expr: inner,
                ty,
                mode,
            } => self.lower_cast(env, inner, ty, *mode, depth, pos),
            Expr::Block(b) => {
                let mut local = env.clone();
                for stmt in &b.stmts {
                    let Stmt::Let {
                        pattern,
                        value,
                        pos,
                        ..
                    } = stmt;
                    let v = self.lower(&local, value, None, depth, *pos)?;
                    self.bind(pattern, v, &mut local, *pos)?;
                }
                self.lower(&local, &b.tail, expected, depth, b.pos)
            }
        }
    }

    /// Lower an expression that must be scalar, returning its IR expression.
    fn lower_scalar(
        &self,
        env: &Env,
        e: &Expr,
        expected: Option<Type>,
        depth: usize,
        pos: Pos,
    ) -> Result<ir::Expr, CompileError> {
        match self.lower(env, e, expected.map(Ty::Scalar).as_ref(), depth, pos)? {
            Val::Scalar(x, _) | Val::Enum(_, x) => Ok(x),
            _ => Err(err(&self.ctx, pos, "expected a scalar value")),
        }
    }

    fn lower_unary(
        &self,
        env: &Env,
        op: UnaryOp,
        inner: &Expr,
        expected: Option<&Ty>,
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        match op {
            UnaryOp::Not => {
                let v = self.lower_scalar(env, inner, Some(Type::Uint(1)), depth, pos)?;
                Ok(Val::Scalar(typed(unary(op, v), Type::Uint(1)), 0))
            }
            UnaryOp::BitNot | UnaryOp::Neg => {
                let val = self.lower(env, inner, expected, depth, pos)?;
                let (e, f) = scalar_parts(&val, &self.ctx, pos)?;
                let ty = e.ty;
                Ok(Val::Scalar(typed(unary(op, e), ty), f))
            }
        }
    }

    // Threads the env + diagnostic context (depth, pos) like its siblings.
    #[allow(clippy::too_many_arguments)]
    fn lower_binary(
        &self,
        env: &Env,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<&Ty>,
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        if op.is_comparison() {
            let (l, r) = self.lower_operands(env, lhs, rhs, None, depth, pos)?;
            let (le, _) = scalar_parts(&l, &self.ctx, pos)?;
            let (re, _) = scalar_parts(&r, &self.ctx, pos)?;
            return Ok(Val::Scalar(typed(binary(op, le, re), Type::Uint(1)), 0));
        }
        match op {
            BinOp::And | BinOp::Or | BinOp::Xor => {
                let l = self.lower(env, lhs, expected, depth, pos)?;
                let (le, lf) = scalar_parts(&l, &self.ctx, pos)?;
                let lt = val_ty(&l);
                let r = self.lower(env, rhs, Some(&lt), depth, pos)?;
                let (re, _) = scalar_parts(&r, &self.ctx, pos)?;
                let ty = le.ty;
                Ok(Val::Scalar(typed(binary(op, le, re), ty), lf))
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (l, r) = self.lower_operands(env, lhs, rhs, expected, depth, pos)?;
                let (le, lf) = scalar_parts(&l, &self.ctx, pos)?;
                let (re, rf) = scalar_parts(&r, &self.ctx, pos)?;
                let (lty, rty) = (le.ty, re.ty);
                let signed = lty.is_signed();
                let (li, ri) = (lty.width() - lf, rty.width() - rf);
                // Width inference (DESIGN §6.2). F1 has already checked matching
                // signedness, matching fractional bits for +/-, and the limit.
                let (ibits, fbits) = if op == BinOp::Mul {
                    (li + ri, lf + rf)
                } else {
                    (li.max(ri) + 1, lf)
                };
                let ty = int_ty(signed, ibits + fbits);
                Ok(Val::Scalar(typed(binary(op, le, re), ty), fbits))
            }
            _ => Err(err(&self.ctx, pos, "unsupported operator")),
        }
    }

    /// Lower the two operands of an arithmetic op, letting a bare literal adopt
    /// the other operand's type (two concrete operands keep their own types,
    /// which may differ in integer width).
    fn lower_operands(
        &self,
        env: &Env,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<&Ty>,
        depth: usize,
        pos: Pos,
    ) -> Result<(Val, Val), CompileError> {
        let lhs_lit = matches!(lhs, Expr::Literal(_));
        let rhs_lit = matches!(rhs, Expr::Literal(_));
        match (lhs_lit, rhs_lit) {
            (true, true) => Ok((
                self.lower(env, lhs, expected, depth, pos)?,
                self.lower(env, rhs, expected, depth, pos)?,
            )),
            (true, false) => {
                let r = self.lower(env, rhs, None, depth, pos)?;
                let rt = val_ty(&r);
                let l = self.lower(env, lhs, Some(&rt), depth, pos)?;
                Ok((l, r))
            }
            (false, _) => {
                let l = self.lower(env, lhs, None, depth, pos)?;
                let lt = val_ty(&l);
                let r = self.lower(env, rhs, rhs_lit.then_some(&lt), depth, pos)?;
                Ok((l, r))
            }
        }
    }

    /// Lower a mode-named cast (DESIGN §6.3) into IR shift/resize/saturate ops.
    /// Step 1 aligns the fractional bits (shift left to add precision; arithmetic
    /// shift right — rounding when `round` — to drop them). Step 2 adjusts the
    /// integer width (sign/zero-extend when widening; truncate, or saturate when
    /// `sat`, when narrowing).
    fn lower_cast(
        &self,
        env: &Env,
        inner: &Expr,
        ty: &TypeRef,
        mode: CastMode,
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        let target = self.resolve(ty, pos)?;
        let (ts, ti, tf) =
            target_fx(&target).ok_or_else(|| err(&self.ctx, pos, "cast to a non-numeric type"))?;
        let val = self.lower(env, inner, None, depth, pos)?;
        let (mut e, xf) = scalar_parts(&val, &self.ctx, pos)?;
        let xs = e.ty.is_signed();
        let xi = e.ty.width() - xf;
        if xs != ts {
            return Err(unsupported(&self.ctx, pos, "sign-changing casts (F4+)"));
        }
        // Widening the fraction (xf -> tf) builds an `xi + tf`-bit intermediate
        // before the integer step narrows it. `eval` is `u64`-bounded, so reject
        // a cast whose intermediate would overflow 64 bits (it would silently
        // diverge from the Verilog, which has no such limit).
        let intermediate = xi + tf.max(xf);
        if intermediate > MAX_WIDTH {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "cast builds a {intermediate}-bit intermediate, exceeding the \
                     {MAX_WIDTH}-bit limit"
                ),
            ));
        }

        // Step 1: fractional alignment xf -> tf.
        if tf > xf {
            let by = tf - xf;
            e = typed(
                ir::ExprKind::Shl {
                    expr: Box::new(e),
                    amount: by,
                },
                int_ty(xs, xi + tf),
            );
        } else if tf < xf {
            let by = xf - tf;
            if mode == CastMode::Round {
                // Round half up: add 1<<(by-1) at the source width (a same-width
                // add that wraps in the rare near-max case), then shift right.
                let half = typed(
                    ir::ExprKind::Literal(mask(1u64 << (by - 1), e.ty.width())),
                    e.ty,
                );
                let ety = e.ty;
                e = typed(binary(BinOp::Add, e, half), ety);
            }
            e = typed(
                ir::ExprKind::Shr {
                    expr: Box::new(e),
                    amount: by,
                },
                int_ty(xs, xi + tf),
            );
        }

        // Step 2: integer width xi -> ti (at fractional bits tf).
        let target_ty = int_ty(ts, ti + tf);
        if ti != xi {
            let cm = if ti < xi && mode == CastMode::Sat {
                CastMode::Sat
            } else {
                CastMode::Wrap
            };
            e = typed(
                ir::ExprKind::Cast {
                    expr: Box::new(e),
                    mode: cm,
                },
                target_ty,
            );
        }
        Ok(Val::Scalar(e, tf))
    }

    fn lower_path(&self, segs: &[String], pos: Pos) -> Result<Val, CompileError> {
        let [enum_name, variant] = segs else {
            return Err(err(&self.ctx, pos, "unsupported path"));
        };
        let w = self.enum_width(enum_name, pos)?;
        let d = self.variant_discr(enum_name, variant, pos)?;
        Ok(Val::Enum(
            enum_name.clone(),
            typed(ir::ExprKind::Literal(mask(d, w)), Type::Uint(w)),
        ))
    }

    fn lower_call(
        &self,
        env: &Env,
        func: &Expr,
        args: &[Expr],
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        let Expr::Ident(name) = func else {
            return Err(err(&self.ctx, pos, "call target must be a function name"));
        };
        let f = *self
            .tables
            .funcs
            .get(name)
            .ok_or_else(|| err(&self.ctx, pos, format!("unknown function `{name}`")))?;
        let inner = self.bind_params(env, None, &f.params, args, depth, pos)?;
        self.lower_body(&inner, f, depth + 1)
    }

    fn lower_method(
        &self,
        env: &Env,
        receiver: &Expr,
        method: &str,
        args: &[Expr],
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        let recv = self.lower(env, receiver, None, depth, pos)?;
        let type_name = match &recv {
            Val::Struct(s, _) => s.clone(),
            Val::Enum(s, _) => s.clone(),
            _ => return Err(err(&self.ctx, pos, "method call on a scalar")),
        };
        let m = *self
            .tables
            .methods
            .get(&(type_name.clone(), method.to_string()))
            .ok_or_else(|| {
                err(
                    &self.ctx,
                    pos,
                    format!("type `{type_name}` has no method `{method}`"),
                )
            })?;
        let inner = self.bind_params(env, Some(recv), &m.params, args, depth, pos)?;
        self.lower_body(&inner, m, depth + 1)
    }

    /// Build the fresh environment for an inlined function/method body: `self`
    /// (if any), then each parameter bound to its (lowered) argument value.
    fn bind_params(
        &self,
        caller: &Env,
        recv: Option<Val>,
        params: &[Param],
        args: &[Expr],
        depth: usize,
        pos: Pos,
    ) -> Result<Env, CompileError> {
        let mut inner: Env = HashMap::new();
        let mut a = args.iter();
        for p in params {
            match p {
                Param::SelfRef { .. } => {
                    let r = recv
                        .clone()
                        .ok_or_else(|| err(&self.ctx, pos, "`self` without a receiver"))?;
                    inner.insert("self".to_string(), r);
                }
                Param::Typed {
                    name,
                    ty,
                    pos: ppos,
                } => {
                    let arg = a
                        .next()
                        .ok_or_else(|| err(&self.ctx, pos, "too few arguments"))?;
                    let pty = self.resolve(ty, *ppos)?;
                    inner.insert(
                        name.clone(),
                        self.lower(caller, arg, Some(&pty), depth, pos)?,
                    );
                }
            }
        }
        Ok(inner)
    }

    fn lower_body(&self, env: &Env, f: &Func, depth: usize) -> Result<Val, CompileError> {
        let ret = self.resolve(&f.ret, f.pos)?;
        let mut local = env.clone();
        for stmt in &f.body.stmts {
            let Stmt::Let {
                pattern,
                value,
                pos,
                ..
            } = stmt;
            let v = self.lower(&local, value, None, depth, *pos)?;
            self.bind(pattern, v, &mut local, *pos)?;
        }
        self.lower(&local, &f.body.tail, Some(&ret), depth, f.body.pos)
    }

    fn lower_match(
        &self,
        env: &Env,
        scrutinee: &Expr,
        arms: &[sinter_syntax::ast::MatchArm],
        expected: Option<&Ty>,
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        let scrut = self.lower(env, scrutinee, None, depth, pos)?;
        // Build (optional condition, body) for each arm, then fold from the
        // default backward into a chain of muxes.
        let mut cases: Vec<(Option<ir::Expr>, Val)> = Vec::new();
        for arm in arms {
            let mut local = env.clone();
            let cond = self.arm_cond(&scrut, &arm.pattern, &mut local, pos)?;
            let body = self.lower(&local, &arm.body, expected, depth, pos)?;
            cases.push((cond, body));
        }
        let mut acc: Option<Val> = None;
        for (cond, body) in cases.into_iter().rev() {
            acc = Some(match (cond, acc) {
                (None, _) => body,       // wildcard / binding arm
                (Some(_), None) => body, // last arm with no default after it
                (Some(c), Some(rest)) => self.mux(&c, &body, &rest, pos)?,
            });
        }
        acc.ok_or_else(|| err(&self.ctx, pos, "match has no arms"))
    }

    /// The condition selecting an arm (or `None` for a catch-all), binding any
    /// pattern variables into `env`.
    fn arm_cond(
        &self,
        scrut: &Val,
        pat: &Pattern,
        env: &mut Env,
        pos: Pos,
    ) -> Result<Option<ir::Expr>, CompileError> {
        match pat {
            Pattern::Wildcard => Ok(None),
            Pattern::Binding(name) => {
                env.insert(name.clone(), scrut.clone());
                Ok(None)
            }
            Pattern::Literal(v) => {
                let s = scalar_of(scrut, &self.ctx, pos)?;
                let lit = typed(ir::ExprKind::Literal(mask_u128(*v, s.ty.width())), s.ty);
                Ok(Some(typed(
                    binary(BinOp::Eq, s.clone(), lit),
                    Type::Uint(1),
                )))
            }
            Pattern::Path(segs) => {
                let [enum_name, variant] = segs.as_slice() else {
                    return Err(err(&self.ctx, pos, "unsupported pattern path"));
                };
                let s = scalar_of(scrut, &self.ctx, pos)?;
                let w = self.enum_width(enum_name, pos)?;
                let d = self.variant_discr(enum_name, variant, pos)?;
                let lit = typed(ir::ExprKind::Literal(mask(d, w)), Type::Uint(w));
                Ok(Some(typed(
                    binary(BinOp::Eq, s.clone(), lit),
                    Type::Uint(1),
                )))
            }
            Pattern::Tuple(_) => Err(err(&self.ctx, pos, "tuple patterns in match are F3+")),
        }
    }

    /// Multiplex two values of the same shape on a 1-bit condition.
    fn mux(&self, cond: &ir::Expr, t: &Val, e: &Val, pos: Pos) -> Result<Val, CompileError> {
        match (t, e) {
            (Val::Scalar(a, fa), Val::Scalar(b, _)) => Ok(Val::Scalar(mux_expr(cond, a, b), *fa)),
            (Val::Enum(n, a), Val::Enum(_, b)) => Ok(Val::Enum(n.clone(), mux_expr(cond, a, b))),
            (Val::Struct(n, fa), Val::Struct(_, fb)) => {
                let mut out = Vec::new();
                for ((name, va), (_, vb)) in fa.iter().zip(fb) {
                    out.push((name.clone(), self.mux(cond, va, vb, pos)?));
                }
                Ok(Val::Struct(n.clone(), out))
            }
            (Val::Tuple(a), Val::Tuple(b)) => Ok(Val::Tuple(
                a.iter()
                    .zip(b)
                    .map(|(x, y)| self.mux(cond, x, y, pos))
                    .collect::<Result<_, _>>()?,
            )),
            (Val::Array(a), Val::Array(b)) => Ok(Val::Array(
                a.iter()
                    .zip(b)
                    .map(|(x, y)| self.mux(cond, x, y, pos))
                    .collect::<Result<_, _>>()?,
            )),
            _ => Err(err(&self.ctx, pos, "branches have different shapes")),
        }
    }

    fn field(&self, base: &Val, field: &str, pos: Pos) -> Result<Val, CompileError> {
        let Val::Struct(_, fields) = base else {
            return Err(err(&self.ctx, pos, format!("`.{field}` on a non-struct")));
        };
        fields
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| err(&self.ctx, pos, format!("no field `{field}`")))
    }

    // --- binding / patterns -----------------------------------------------

    fn bind(&self, pat: &Pattern, v: Val, env: &mut Env, pos: Pos) -> Result<(), CompileError> {
        match pat {
            Pattern::Wildcard => Ok(()),
            Pattern::Binding(name) => {
                env.insert(name.clone(), v);
                Ok(())
            }
            Pattern::Tuple(pats) => {
                let Val::Tuple(vals) = v else {
                    return Err(err(&self.ctx, pos, "tuple pattern binds a non-tuple"));
                };
                if pats.len() != vals.len() {
                    return Err(err(&self.ctx, pos, "tuple pattern arity mismatch"));
                }
                for (p, val) in pats.iter().zip(vals) {
                    self.bind(p, val, env, pos)?;
                }
                Ok(())
            }
            Pattern::Literal(_) | Pattern::Path(_) => {
                Err(err(&self.ctx, pos, "refutable pattern in `let`"))
            }
        }
    }

    // --- type resolution & struct/enum tables -----------------------------

    fn resolve(&self, t: &TypeRef, pos: Pos) -> Result<Ty, CompileError> {
        match t {
            TypeRef::Scalar(ty) => Ok(Ty::Scalar(*ty)),
            TypeRef::Fixed {
                signed,
                int_bits,
                frac_bits,
            } => Ok(Ty::Fixed {
                signed: *signed,
                ibits: *int_bits,
                fbits: *frac_bits,
            }),
            TypeRef::Named(n) => self.resolve_named(n, pos),
            TypeRef::Array { elem, len } => {
                let et = self.resolve(elem, pos)?;
                let n = self.const_index(len, pos)?;
                Ok(Ty::Array(Box::new(et), n))
            }
            TypeRef::Tuple(ts) => Ok(Ty::Tuple(
                ts.iter()
                    .map(|t| self.resolve(t, pos))
                    .collect::<Result<_, _>>()?,
            )),
        }
    }

    fn resolve_named(&self, name: &str, pos: Pos) -> Result<Ty, CompileError> {
        if self.tables.structs.contains_key(name) {
            Ok(Ty::Struct(name.to_string()))
        } else if self.tables.enums.contains_key(name) {
            Ok(Ty::Enum(name.to_string(), self.enum_width(name, pos)?))
        } else {
            Err(err(&self.ctx, pos, format!("unknown type `{name}`")))
        }
    }

    fn struct_fields(&self, name: &str, pos: Pos) -> Result<Vec<(String, Ty)>, CompileError> {
        let s = *self
            .tables
            .structs
            .get(name)
            .ok_or_else(|| err(&self.ctx, pos, format!("unknown struct `{name}`")))?;
        s.fields
            .iter()
            .map(|f| Ok((f.name.clone(), self.resolve(&f.ty, f.pos)?)))
            .collect()
    }

    fn enum_width(&self, name: &str, pos: Pos) -> Result<u32, CompileError> {
        let e = *self
            .tables
            .enums
            .get(name)
            .ok_or_else(|| err(&self.ctx, pos, format!("unknown enum `{name}`")))?;
        let n = e.variants.len().max(1) as u32;
        if onehot(e) {
            Ok(e.variants.len().max(1) as u32)
        } else {
            // Binary: enough bits for the largest discriminant.
            let max_d = (0..e.variants.len())
                .map(|i| {
                    self.variant_discr(name, &e.variants[i].name, pos)
                        .unwrap_or(i as u64)
                })
                .max()
                .unwrap_or(0);
            Ok(bits_for(max_d).max(1).max(bits_for(u64::from(n) - 1)))
        }
    }

    fn variant_discr(&self, enum_name: &str, variant: &str, pos: Pos) -> Result<u64, CompileError> {
        let e = *self
            .tables
            .enums
            .get(enum_name)
            .ok_or_else(|| err(&self.ctx, pos, format!("unknown enum `{enum_name}`")))?;
        let idx = e
            .variants
            .iter()
            .position(|v| v.name == variant)
            .ok_or_else(|| err(&self.ctx, pos, format!("no variant `{variant}`")))?;
        // Explicit discriminant wins; otherwise binary index or one-hot bit.
        if let Some(d) = &e.variants[idx].discriminant {
            if let Expr::Literal(v) = d {
                return u64::try_from(*v)
                    .map_err(|_| err(&self.ctx, pos, "discriminant too large"));
            }
            return Err(err(&self.ctx, pos, "non-literal discriminant"));
        }
        Ok(if onehot(e) { 1u64 << idx } else { idx as u64 })
    }

    // --- reset / default values -------------------------------------------

    /// Compute the reset value of every leaf register of the state struct.
    fn state_resets(&self, name: &str, pos: Pos) -> Result<HashMap<String, u64>, CompileError> {
        let v = self.default_val(&Ty::Struct(name.to_string()), None, pos)?;
        let mut leaves = Vec::new();
        flatten_val(&v, "", &mut leaves);
        leaves
            .into_iter()
            .map(|(n, e)| match e.kind {
                ir::ExprKind::Literal(x) => Ok((n, x)),
                _ => Err(err(&self.ctx, pos, "reset value is not constant")),
            })
            .collect()
    }

    /// The default value of a type: an explicit default if present, otherwise
    /// the recursively-derived default of a struct (each field's own default).
    fn default_val(&self, ty: &Ty, explicit: Option<&Expr>, pos: Pos) -> Result<Val, CompileError> {
        if let Some(e) = explicit {
            return self.const_val(e, ty, pos);
        }
        match ty {
            Ty::Struct(name) => {
                let s = *self
                    .tables
                    .structs
                    .get(name)
                    .ok_or_else(|| err(&self.ctx, pos, format!("unknown struct `{name}`")))?;
                let mut out = Vec::new();
                for f in &s.fields {
                    let fty = self.resolve(&f.ty, f.pos)?;
                    out.push((
                        f.name.clone(),
                        self.default_val(&fty, f.default.as_ref(), f.pos)?,
                    ));
                }
                Ok(Val::Struct(name.clone(), out))
            }
            _ => Err(err(&self.ctx, pos, "field needs a reset value (`= …`)")),
        }
    }

    /// Constant-fold `e` to a [`Val`] of literal leaves, of type `ty`.
    fn const_val(&self, e: &Expr, ty: &Ty, pos: Pos) -> Result<Val, CompileError> {
        match (e, ty) {
            (Expr::Array(items), Ty::Array(et, n)) => {
                if items.len() != *n {
                    return Err(err(&self.ctx, pos, "array literal length mismatch"));
                }
                Ok(Val::Array(
                    items
                        .iter()
                        .map(|it| self.const_val(it, et, pos))
                        .collect::<Result<_, _>>()?,
                ))
            }
            (Expr::StructLit { name, fields }, Ty::Struct(_)) => {
                let mut out = Vec::new();
                for (fname, fty) in self.struct_fields(name, pos)? {
                    let init = fields
                        .iter()
                        .find(|fi| fi.name == fname)
                        .ok_or_else(|| err(&self.ctx, pos, format!("missing field `{fname}`")))?;
                    out.push((fname.clone(), self.const_val(&init.value, &fty, pos)?));
                }
                Ok(Val::Struct(name.clone(), out))
            }
            (Expr::Path(segs), Ty::Enum(..)) => self.lower_path(segs, pos),
            (_, _) => {
                let t = ty
                    .scalar()
                    .ok_or_else(|| err(&self.ctx, pos, "non-scalar constant"))?;
                let v = self.fold_scalar(e, t, pos)?;
                Ok(Val::Scalar(
                    typed(ir::ExprKind::Literal(mask(v, t.width())), t),
                    ty.fbits(),
                ))
            }
        }
    }

    /// Constant-fold a scalar expression to a value.
    fn fold_scalar(&self, e: &Expr, ty: Type, pos: Pos) -> Result<u64, CompileError> {
        match e {
            Expr::Literal(v) => Ok(mask(self.lit_u64(*v, pos)?, ty.width())),
            Expr::Ident(name) => self
                .consts
                .get(name)
                .map(|(v, _)| mask(*v, ty.width()))
                .ok_or_else(|| err(&self.ctx, pos, format!("`{name}` is not a constant"))),
            _ => Err(unsupported(
                &self.ctx,
                pos,
                "reset values beyond literals, arrays, struct/enum defaults, and `const`s",
            )),
        }
    }

    // --- structured-value builders ----------------------------------------

    /// Flatten a type to its leaf scalar registers (mirrors `idents_val`).
    fn flatten_ty(&self, ty: &Ty, path: &str, out: &mut Vec<(String, Type)>) {
        match ty {
            Ty::Scalar(t) => out.push((path.to_string(), *t)),
            Ty::Fixed {
                signed,
                ibits,
                fbits,
            } => out.push((path.to_string(), int_ty(*signed, ibits + fbits))),
            Ty::Enum(_, w) => out.push((path.to_string(), Type::Uint(*w))),
            Ty::Struct(name) => {
                if let Ok(fields) = self.struct_fields(name, Pos::default()) {
                    for (f, fty) in fields {
                        self.flatten_ty(&fty, &join(path, &f), out);
                    }
                }
            }
            Ty::Array(et, n) => {
                for i in 0..*n {
                    self.flatten_ty(et, &join(path, &i.to_string()), out);
                }
            }
            Ty::Tuple(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    self.flatten_ty(t, &join(path, &i.to_string()), out);
                }
            }
        }
    }

    /// Build the structured value of a type with each leaf an `Ident(path)`.
    fn idents_val(&self, ty: &Ty, path: &str) -> Val {
        match ty {
            Ty::Scalar(t) => Val::Scalar(typed(ir::ExprKind::Ident(path.to_string()), *t), 0),
            Ty::Fixed {
                signed,
                ibits,
                fbits,
            } => Val::Scalar(
                typed(
                    ir::ExprKind::Ident(path.to_string()),
                    int_ty(*signed, ibits + fbits),
                ),
                *fbits,
            ),
            Ty::Enum(n, w) => Val::Enum(
                n.clone(),
                typed(ir::ExprKind::Ident(path.to_string()), Type::Uint(*w)),
            ),
            Ty::Struct(name) => {
                let fields = self.struct_fields(name, Pos::default()).unwrap_or_default();
                Val::Struct(
                    name.clone(),
                    fields
                        .into_iter()
                        .map(|(f, fty)| {
                            let v = self.idents_val(&fty, &join(path, &f));
                            (f, v)
                        })
                        .collect(),
                )
            }
            Ty::Array(et, n) => Val::Array(
                (0..*n)
                    .map(|i| self.idents_val(et, &join(path, &i.to_string())))
                    .collect(),
            ),
            Ty::Tuple(ts) => Val::Tuple(
                ts.iter()
                    .enumerate()
                    .map(|(i, t)| self.idents_val(t, &join(path, &i.to_string())))
                    .collect(),
            ),
        }
    }

    // --- small helpers ----------------------------------------------------

    fn lit(&self, v: u128, ty: Type, pos: Pos) -> Result<ir::Expr, CompileError> {
        Ok(typed(
            ir::ExprKind::Literal(mask(self.lit_u64(v, pos)?, ty.width())),
            ty,
        ))
    }

    fn lit_u64(&self, v: u128, pos: Pos) -> Result<u64, CompileError> {
        u64::try_from(v).map_err(|_| err(&self.ctx, pos, format!("literal `{v}` is too large")))
    }

    fn const_index(&self, e: &Expr, pos: Pos) -> Result<usize, CompileError> {
        match e {
            Expr::Literal(v) => {
                usize::try_from(*v).map_err(|_| err(&self.ctx, pos, "index too large"))
            }
            Expr::Ident(name) => self
                .consts
                .get(name)
                .and_then(|(v, _)| usize::try_from(*v).ok())
                .ok_or_else(|| err(&self.ctx, pos, "index must be a constant")),
            _ => Err(err(&self.ctx, pos, "index/length must be a constant")),
        }
    }
}

// --- free helpers ----------------------------------------------------------

fn flatten_val(v: &Val, path: &str, out: &mut Vec<(String, ir::Expr)>) {
    match v {
        Val::Scalar(e, _) | Val::Enum(_, e) => out.push((path.to_string(), e.clone())),
        Val::Struct(_, fields) => {
            for (f, val) in fields {
                flatten_val(val, &join(path, f), out);
            }
        }
        Val::Array(items) => {
            for (i, val) in items.iter().enumerate() {
                flatten_val(val, &join(path, &i.to_string()), out);
            }
        }
        Val::Tuple(items) => {
            for (i, val) in items.iter().enumerate() {
                flatten_val(val, &join(path, &i.to_string()), out);
            }
        }
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}_{name}")
    }
}

fn scalar_of(v: &Val, ctx: &str, pos: Pos) -> Result<ir::Expr, CompileError> {
    match v {
        Val::Scalar(e, _) | Val::Enum(_, e) => Ok(e.clone()),
        _ => Err(err(ctx, pos, "expected a scalar value")),
    }
}

/// A scalar value's IR expression and fractional-bit count (0 for an enum).
fn scalar_parts(v: &Val, ctx: &str, pos: Pos) -> Result<(ir::Expr, u32), CompileError> {
    match v {
        Val::Scalar(e, f) => Ok((e.clone(), *f)),
        Val::Enum(_, e) => Ok((e.clone(), 0)),
        _ => Err(err(ctx, pos, "expected a scalar value")),
    }
}

/// The resolved type of a (scalar/enum/struct) value, for operand inference.
fn val_ty(v: &Val) -> Ty {
    match v {
        Val::Scalar(e, f) if *f == 0 => Ty::Scalar(e.ty),
        Val::Scalar(e, f) => Ty::Fixed {
            signed: e.ty.is_signed(),
            ibits: e.ty.width() - f,
            fbits: *f,
        },
        Val::Enum(n, e) => Ty::Enum(n.clone(), e.ty.width()),
        Val::Struct(n, _) => Ty::Struct(n.clone()),
        // Aggregates are never arithmetic operands (the checker rejects them).
        Val::Tuple(_) | Val::Array(_) => Ty::Scalar(Type::Uint(1)),
    }
}

/// The (signed, integer-bits, fractional-bits) of a scalar/fixed target type.
fn target_fx(ty: &Ty) -> Option<(bool, u32, u32)> {
    match ty {
        Ty::Scalar(Type::Uint(n)) => Some((false, *n, 0)),
        Ty::Scalar(Type::Int(n)) => Some((true, *n, 0)),
        Ty::Fixed {
            signed,
            ibits,
            fbits,
        } => Some((*signed, *ibits, *fbits)),
        _ => None,
    }
}

fn onehot(e: &EnumItem) -> bool {
    e.attrs
        .iter()
        .any(|a| a.name == "encoding" && a.args.iter().any(|arg| arg.key == "onehot"))
}

fn bits_for(max_value: u64) -> u32 {
    if max_value == 0 {
        0
    } else {
        64 - max_value.leading_zeros()
    }
}

fn mask_u128(v: u128, w: u32) -> u64 {
    mask(u64::try_from(v).unwrap_or(0), w)
}

fn mux_expr(cond: &ir::Expr, t: &ir::Expr, e: &ir::Expr) -> ir::Expr {
    ir::Expr {
        kind: ir::ExprKind::If {
            cond: Box::new(cond.clone()),
            then: Box::new(t.clone()),
            els: Box::new(e.clone()),
        },
        ty: t.ty,
    }
}

fn typed(kind: ir::ExprKind, ty: Type) -> ir::Expr {
    ir::Expr { kind, ty }
}

fn unary(op: UnaryOp, e: ir::Expr) -> ir::ExprKind {
    ir::ExprKind::Unary {
        op,
        expr: Box::new(e),
    }
}

fn binary(op: BinOp, l: ir::Expr, r: ir::Expr) -> ir::ExprKind {
    ir::ExprKind::Binary {
        op,
        lhs: Box::new(l),
        rhs: Box::new(r),
    }
}

fn const_values(program: &Program) -> Result<HashMap<String, (u64, Type)>, CompileError> {
    let mut consts: HashMap<String, (u64, Type)> = HashMap::new();
    for item in &program.items {
        if let Item::Const(c) = item {
            let ty = match &c.ty {
                TypeRef::Scalar(t) => *t,
                _ => return Err(unsupported(&c.name, c.pos, "non-scalar constants (F4)")),
            };
            let v = match &c.value {
                Expr::Literal(v) => u64::try_from(*v)
                    .map_err(|_| err(&c.name, c.pos, format!("literal `{v}` is too large")))?,
                Expr::Ident(name) => consts.get(name).map(|(v, _)| *v).ok_or_else(|| {
                    err(&c.name, c.pos, format!("`{name}` is not a known constant"))
                })?,
                _ => {
                    return Err(unsupported(
                        &c.name,
                        c.pos,
                        "const values beyond literals/refs",
                    ))
                }
            };
            consts.insert(c.name.clone(), (mask(v, ty.width()), ty));
        }
    }
    Ok(consts)
}

fn err(ctx: &str, pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, pos, msg)
}

fn unsupported(ctx: &str, pos: Pos, what: &str) -> CompileError {
    CompileError::semantic(ctx, pos, format!("not yet lowered: {what}"))
}
