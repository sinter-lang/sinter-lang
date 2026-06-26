//! Functional surface (DESIGN §2.1) — **F2: elaboration to the IR**.
//!
//! Lowers a type-checked functional design to the same flat [`ir::Module`] the
//! legacy `module` surface produces, so the simulator, Verilog backend, and
//! conformance oracle are reused unchanged. The transition root's state struct
//! becomes the register set; its body's tail struct literal becomes the
//! per-register next-state; its `let`s become combinational wires; and every
//! leaf register is exposed as an observation output (`<field>_q`) so the
//! oracle diffs the full state each cycle.
//!
//! Scope of this first increment (the rest is F3+): **scalar state and inputs**,
//! a `-> Self` transition root, and `wrap` casts. Enums/`match`, sub-state
//! method calls, free-function calls, nested structs, arrays, tuples, and
//! `(Self, Outputs)` returns are reported as not-yet-supported.

use std::collections::HashMap;

use sinter_syntax::ast::{
    BinOp, Expr, Func, Item, Param, Pattern, Pos, Program, Stmt, Type, TypeRef, UnaryOp,
};

use crate::error::{mask, CompileError};
use crate::ir;

/// Elaborate the functional surface of `program` into an [`ir::Module`].
///
/// Assumes `program` has already passed `functional::check_functional`.
///
/// # Errors
/// Returns a [`CompileError`] if the design uses a construct this phase does not
/// yet lower.
pub fn elaborate(program: &Program) -> Result<Option<ir::Module>, CompileError> {
    let Some((state_ty, tick, domain)) = find_clock_root(program)? else {
        // A functional library with no transition root lowers to nothing.
        return Ok(None);
    };
    let mut el = Elaborator {
        ctx: state_ty.to_string(),
        consts: const_values(program)?,
        names: HashMap::new(),
    };
    el.build(program, state_ty, tick, &domain).map(Some)
}

/// Locate the unique `#[clock]` transition root: its state type, method, and
/// clock-domain name. `Ok(None)` when there is none.
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
                            Some(sinter_syntax::ast::AttrValue::Ident(d)) => Some(d.clone()),
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

struct Elaborator {
    /// Error-message context (the state type name).
    ctx: String,
    /// Constant values by name, with their declared type.
    consts: HashMap<String, (u64, Type)>,
    /// Readable scalar names: registers, inputs, and `let` wires, to their type.
    names: HashMap<String, Type>,
}

impl Elaborator {
    fn build(
        &mut self,
        program: &Program,
        state_ty: &str,
        tick: &Func,
        domain: &str,
    ) -> Result<ir::Module, CompileError> {
        let fields = struct_fields(program, state_ty, &self.ctx)?;

        // Registers: one per (scalar) state field, reset = its default value.
        let mut regs = Vec::new();
        for (name, ty, default, pos) in &fields {
            let reset = match default {
                Some(d) => self.const_eval(d, *ty, *pos)?,
                None => {
                    return Err(err(
                        &self.ctx,
                        *pos,
                        format!("state field `{name}` needs a reset value (`= …`)"),
                    ))
                }
            };
            self.names.insert(name.clone(), *ty);
            regs.push(ir::Reg {
                name: name.clone(),
                ty: *ty,
                reset,
            });
        }

        // Inputs: the transition root's parameters after `self`.
        let mut ports = Vec::new();
        for p in &tick.params {
            match p {
                Param::SelfRef { .. } => {}
                Param::Typed { name, ty, pos } => {
                    let ty = scalar(ty, &self.ctx, *pos)?;
                    self.names.insert(name.clone(), ty);
                    ports.push(ir::Port {
                        dir: sinter_syntax::ast::Dir::In,
                        name: name.clone(),
                        ty,
                    });
                }
            }
        }

        // The body: `let`s become combinational wires (declare-before-use, so
        // source order is a valid evaluation order), the tail struct literal
        // becomes the per-register next-state.
        let mut lets = Vec::new();
        for stmt in &tick.body.stmts {
            let Stmt::Let {
                pattern,
                ty,
                value,
                pos,
            } = stmt;
            let Pattern::Binding(name) = pattern else {
                return Err(err(
                    &self.ctx,
                    *pos,
                    "F2 lowers only simple `let name = …` bindings (tuple destructuring is F3)",
                ));
            };
            let want = match ty {
                Some(t) => Some(scalar(t, &self.ctx, *pos)?),
                None => None,
            };
            let value = self.lower(value, want, *pos)?;
            let ty = value.ty;
            self.names.insert(name.clone(), ty);
            lets.push(ir::Let {
                name: name.clone(),
                ty,
                value,
            });
        }

        let reg_updates =
            self.lower_next_state(&tick.body.tail, state_ty, &fields, tick.body.pos)?;

        // Observation outputs: expose every register as `<field>_q`.
        let mut out_drives = Vec::new();
        for r in &regs {
            let name = format!("{}_q", r.name);
            ports.push(ir::Port {
                dir: sinter_syntax::ast::Dir::Out,
                name: name.clone(),
                ty: r.ty,
            });
            out_drives.push((
                name,
                ir::Expr {
                    kind: ir::ExprKind::Ident(r.name.clone()),
                    ty: r.ty,
                },
            ));
        }

        Ok(ir::Module {
            name: state_ty.to_string(),
            clock_domain: domain.to_string(),
            ports,
            regs,
            lets,
            out_drives,
            reg_updates,
        })
    }

    /// Lower the tail `Self { field: expr, … }` to register next-state values,
    /// in register (field declaration) order.
    fn lower_next_state(
        &self,
        tail: &Expr,
        state_ty: &str,
        fields: &[(String, Type, Option<Expr>, Pos)],
        pos: Pos,
    ) -> Result<Vec<(String, ir::Expr)>, CompileError> {
        let Expr::StructLit {
            name,
            fields: inits,
        } = tail
        else {
            return Err(err(
                &self.ctx,
                pos,
                "the transition root's body must end in a `Self { … }` struct literal",
            ));
        };
        if name != state_ty {
            return Err(err(
                &self.ctx,
                pos,
                format!("transition root returns `{name}`, expected `{state_ty}`"),
            ));
        }
        let mut updates = Vec::new();
        for (fname, fty, _, fpos) in fields {
            let init = inits.iter().find(|fi| &fi.name == fname).ok_or_else(|| {
                err(
                    &self.ctx,
                    *fpos,
                    format!("missing next-state for field `{fname}`"),
                )
            })?;
            let value = self.lower(&init.value, Some(*fty), *fpos)?;
            updates.push((fname.clone(), value));
        }
        Ok(updates)
    }

    /// Lower a functional expression to a typed IR expression.
    fn lower(&self, e: &Expr, expected: Option<Type>, pos: Pos) -> Result<ir::Expr, CompileError> {
        match e {
            Expr::Literal(v) => {
                let ty = expected
                    .ok_or_else(|| err(&self.ctx, pos, format!("cannot infer type of `{v}`")))?;
                let v = u64::try_from(*v)
                    .map_err(|_| err(&self.ctx, pos, format!("literal `{v}` is too large")))?;
                Ok(typed(ir::ExprKind::Literal(mask(v, ty.width())), ty))
            }
            Expr::Ident(name) => {
                if let Some(ty) = self.names.get(name) {
                    Ok(typed(ir::ExprKind::Ident(name.clone()), *ty))
                } else if let Some((v, ty)) = self.consts.get(name) {
                    Ok(typed(ir::ExprKind::Literal(*v), *ty))
                } else {
                    Err(err(&self.ctx, pos, format!("unknown name `{name}`")))
                }
            }
            // `self.field` reads the corresponding register.
            Expr::Field { base, field } => match base.as_ref() {
                Expr::Ident(s) if s == "self" => {
                    let ty =
                        self.names.get(field).copied().ok_or_else(|| {
                            err(&self.ctx, pos, format!("unknown field `{field}`"))
                        })?;
                    Ok(typed(ir::ExprKind::Ident(field.clone()), ty))
                }
                _ => Err(unsupported(
                    &self.ctx,
                    pos,
                    "field access on a non-`self` value",
                )),
            },
            Expr::Unary { op, expr: inner } => self.lower_unary(*op, inner, expected, pos),
            Expr::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs, expected, pos),
            Expr::If { cond, then, els } => {
                let cond = self.lower(cond, Some(Type::Uint(1)), pos)?;
                let then = self.lower(then, expected, pos)?;
                let ty = then.ty;
                let els = self.lower(els, Some(ty), pos)?;
                Ok(typed(
                    ir::ExprKind::If {
                        cond: Box::new(cond),
                        then: Box::new(then),
                        els: Box::new(els),
                    },
                    ty,
                ))
            }
            Expr::Cast {
                expr: inner,
                ty,
                mode,
            } => {
                if !matches!(mode, sinter_syntax::ast::CastMode::Wrap) {
                    return Err(unsupported(
                        &self.ctx,
                        pos,
                        "only `wrap` casts are lowered today (sat/trunc/round are F4)",
                    ));
                }
                let target = scalar(ty, &self.ctx, pos)?;
                let inner = self.lower(inner, None, pos)?;
                Ok(typed(
                    ir::ExprKind::Cast {
                        expr: Box::new(inner),
                        mode: *mode,
                    },
                    target,
                ))
            }
            Expr::Path(_) => Err(unsupported(&self.ctx, pos, "enum values / paths (F3)")),
            Expr::Match { .. } => Err(unsupported(&self.ctx, pos, "`match` (F3)")),
            Expr::MethodCall { .. } => Err(unsupported(&self.ctx, pos, "method calls (F3)")),
            Expr::Call { .. } => Err(unsupported(&self.ctx, pos, "function calls (F3)")),
            Expr::StructLit { .. } => {
                Err(unsupported(&self.ctx, pos, "nested struct literals (F3)"))
            }
            Expr::Tuple(_) => Err(unsupported(&self.ctx, pos, "tuples (F3)")),
            Expr::Array(_) => Err(unsupported(&self.ctx, pos, "arrays (F3)")),
            Expr::Index { .. } => Err(unsupported(&self.ctx, pos, "indexing (F3)")),
            // `if`/`else` branches are blocks. A block whose value is just its
            // tail (no `let`s) lowers to that tail; block-local bindings in
            // expression position are F3.
            Expr::Block(b) if b.stmts.is_empty() => self.lower(&b.tail, expected, pos),
            Expr::Block(_) => Err(unsupported(
                &self.ctx,
                pos,
                "blocks with `let` bindings in expression position (F3)",
            )),
        }
    }

    fn lower_unary(
        &self,
        op: UnaryOp,
        inner: &Expr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<ir::Expr, CompileError> {
        match op {
            UnaryOp::Not => {
                let e = self.lower(inner, Some(Type::Uint(1)), pos)?;
                Ok(typed(unary(op, e), Type::Uint(1)))
            }
            UnaryOp::BitNot | UnaryOp::Neg => {
                let e = self.lower(inner, expected, pos)?;
                let ty = e.ty;
                Ok(typed(unary(op, e), ty))
            }
        }
    }

    fn lower_binary(
        &self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<ir::Expr, CompileError> {
        if op.is_comparison() {
            let opty = self.synth(lhs).or_else(|| self.synth(rhs));
            let l = self.lower(lhs, opty, pos)?;
            let lty = l.ty;
            let r = self.lower(rhs, Some(lty), pos)?;
            return Ok(typed(binary(op, l, r), Type::Uint(1)));
        }
        match op {
            BinOp::And | BinOp::Or | BinOp::Xor => {
                let opty = expected.or_else(|| self.synth(lhs));
                let l = self.lower(lhs, opty, pos)?;
                let ty = l.ty;
                let r = self.lower(rhs, Some(ty), pos)?;
                Ok(typed(binary(op, l, r), ty))
            }
            BinOp::Add | BinOp::Sub => {
                let opty = self.synth(lhs).or_else(|| self.synth(rhs));
                let l = self.lower(lhs, opty, pos)?;
                let lty = l.ty;
                let r = self.lower(rhs, Some(lty), pos)?;
                // Width inference (DESIGN §6.2): widen the result by one bit.
                let w = (lty.width().min(63)) + 1;
                let ty = if lty.is_signed() {
                    Type::Int(w)
                } else {
                    Type::Uint(w)
                };
                Ok(typed(binary(op, l, r), ty))
            }
            _ => Err(unsupported(&self.ctx, pos, "this operator")),
        }
    }

    /// Best-effort type synthesis for operand-type inference (no errors).
    fn synth(&self, e: &Expr) -> Option<Type> {
        match e {
            Expr::Ident(name) => self
                .names
                .get(name)
                .copied()
                .or_else(|| self.consts.get(name).map(|(_, t)| *t)),
            Expr::Field { base, field } => match base.as_ref() {
                Expr::Ident(s) if s == "self" => self.names.get(field).copied(),
                _ => None,
            },
            Expr::Unary { op, expr } => match op {
                UnaryOp::Not => Some(Type::Uint(1)),
                UnaryOp::BitNot | UnaryOp::Neg => self.synth(expr),
            },
            Expr::Binary { op, lhs, rhs } => {
                if op.is_comparison() {
                    Some(Type::Uint(1))
                } else {
                    self.synth(lhs).or_else(|| self.synth(rhs))
                }
            }
            Expr::Cast { ty, .. } => scalar(ty, &self.ctx, Pos::default()).ok(),
            _ => None,
        }
    }

    /// Constant-fold an expression to a value of `expected` type. F2 supports
    /// literals and references to other constants.
    fn const_eval(&self, e: &Expr, expected: Type, pos: Pos) -> Result<u64, CompileError> {
        match e {
            Expr::Literal(v) => {
                let v = u64::try_from(*v)
                    .map_err(|_| err(&self.ctx, pos, format!("literal `{v}` is too large")))?;
                Ok(mask(v, expected.width()))
            }
            Expr::Ident(name) => self
                .consts
                .get(name)
                .map(|(v, _)| mask(*v, expected.width()))
                .ok_or_else(|| err(&self.ctx, pos, format!("`{name}` is not a constant"))),
            _ => Err(unsupported(
                &self.ctx,
                pos,
                "reset values beyond literals and `const` references (F3/F4)",
            )),
        }
    }
}

// --- helpers ---------------------------------------------------------------

/// The (name, type, default, pos) of each field of a named scalar struct.
#[allow(clippy::type_complexity)]
fn struct_fields(
    program: &Program,
    name: &str,
    ctx: &str,
) -> Result<Vec<(String, Type, Option<Expr>, Pos)>, CompileError> {
    for item in &program.items {
        if let Item::Struct(s) = item {
            if s.name == name {
                let mut out = Vec::new();
                for f in &s.fields {
                    let ty = scalar(&f.ty, ctx, f.pos)?;
                    out.push((f.name.clone(), ty, f.default.clone(), f.pos));
                }
                return Ok(out);
            }
        }
    }
    Err(err(
        ctx,
        Pos::default(),
        format!("unknown state struct `{name}`"),
    ))
}

/// Build the table of constant values (literals and references to earlier
/// constants), with their declared scalar type.
fn const_values(program: &Program) -> Result<HashMap<String, (u64, Type)>, CompileError> {
    let mut consts: HashMap<String, (u64, Type)> = HashMap::new();
    for item in &program.items {
        if let Item::Const(c) = item {
            let ty = scalar(&c.ty, &c.name, c.pos)?;
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
                        "const values beyond literals and references (F3/F4)",
                    ))
                }
            };
            consts.insert(c.name.clone(), (mask(v, ty.width()), ty));
        }
    }
    Ok(consts)
}

/// Resolve a [`TypeRef`] that must be a scalar `uN`/`iN`.
fn scalar(t: &TypeRef, ctx: &str, pos: Pos) -> Result<Type, CompileError> {
    match t {
        TypeRef::Scalar(ty) => Ok(*ty),
        TypeRef::Fixed { .. } => Err(unsupported(ctx, pos, "fixed-point types (F4)")),
        _ => Err(unsupported(ctx, pos, "non-scalar state/values (F3)")),
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

fn err(ctx: &str, pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, pos, msg)
}

fn unsupported(ctx: &str, pos: Pos, what: &str) -> CompileError {
    CompileError::semantic(ctx, pos, format!("F2 does not yet lower {what}"))
}
