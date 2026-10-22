//! Semantic checking: AST → typed [`ir::Design`].
//!
//! Enforces the load-bearing rules of the minimal synchronous core:
//! - every name resolves and has a width ≤ 64 (MVP limit);
//! - the single-driver rule: each register has exactly one `<=`, each output
//!   exactly one `=`; inputs are never assigned (DESIGN §4.1);
//! - well-typed expressions under the strict same-width discipline;
//! - an acyclic combinational graph, returned topologically sorted (§4.2).

use std::collections::{HashMap, HashSet};

use sinter_syntax::ast::{
    Assign, AssignKind, BinOp, Dir, Expr as AstExpr, Module as AstModule, Pos, Program, Type,
    UnaryOp,
};

use crate::error::{mask, umax, CompileError};
use crate::ir::{self, Design, Expr, ExprKind, BOOL};

/// The widest type the MVP supports (keeps evaluation within `u64`/`i128`).
const MAX_WIDTH: u32 = 64;

/// Check a parsed program, producing the typed IR.
///
/// # Errors
/// Returns the first [`CompileError`] encountered.
pub fn check_program(program: &Program) -> Result<Design, CompileError> {
    let mut modules = Vec::with_capacity(program.modules.len());
    for m in &program.modules {
        modules.push(check_module(m)?);
    }
    Ok(Design { modules })
}

struct Checker<'a> {
    module: &'a str,
    /// Names that may be read in expressions: inputs, registers, and wires.
    readable: HashMap<String, Type>,
    /// Output names and their types (write-only targets).
    outputs: HashMap<String, Type>,
    /// Register names and their types (next-state targets, also readable).
    regs: HashMap<String, Type>,
    /// Wire (`let`) names — used to scope the combinational dependency graph.
    let_names: HashSet<String>,
}

fn check_module(m: &AstModule) -> Result<ir::Module, CompileError> {
    let name = m.name.clone();
    let mut chk = Checker {
        module: &name,
        readable: HashMap::new(),
        outputs: HashMap::new(),
        regs: HashMap::new(),
        let_names: HashSet::new(),
    };

    // --- Collect declarations, checking widths and duplicate names. --------
    let mut declared: HashSet<String> = HashSet::new();
    let declare = |chk: &Checker, declared: &mut HashSet<String>, n: &str, pos: Pos| {
        if !declared.insert(n.to_string()) {
            return Err(CompileError::semantic(
                chk.module,
                pos,
                format!("duplicate declaration of `{n}`"),
            ));
        }
        Ok(())
    };

    for p in &m.ports {
        check_width(&name, p.ty, p.pos)?;
        declare(&chk, &mut declared, &p.name, p.pos)?;
        match p.dir {
            Dir::In => {
                chk.readable.insert(p.name.clone(), p.ty);
            }
            Dir::Out => {
                chk.outputs.insert(p.name.clone(), p.ty);
            }
        }
    }
    for r in &m.regs {
        check_width(&name, r.ty, r.pos)?;
        declare(&chk, &mut declared, &r.name, r.pos)?;
        chk.readable.insert(r.name.clone(), r.ty);
        chk.regs.insert(r.name.clone(), r.ty);
    }
    for l in &m.lets {
        if let Some(ty) = l.ty {
            check_width(&name, ty, l.pos)?;
        }
        declare(&chk, &mut declared, &l.name, l.pos)?;
        chk.let_names.insert(l.name.clone());
    }

    // --- Type-check `let` bindings; record their (possibly inferred) types. -
    let mut lets = Vec::new();
    for l in &m.lets {
        let value = chk.check_expr(&l.value, l.ty, l.pos)?;
        if let Some(decl) = l.ty {
            check_width(&name, decl, l.pos)?;
        }
        let ty = value.ty;
        chk.readable.insert(l.name.clone(), ty);
        lets.push(ir::Let {
            name: l.name.clone(),
            ty,
            value,
        });
    }

    // --- Registers: constant-fold reset, type-check next-state. -------------
    let mut regs = Vec::new();
    for r in &m.regs {
        let init = chk.check_expr(&r.init, Some(r.ty), r.pos)?;
        let reset = chk.const_eval(&init, r.pos)?;
        regs.push(ir::Reg {
            name: r.name.clone(),
            ty: r.ty,
            frac_bits: 0,
            reset,
        });
    }

    // --- Assignments: classify, enforce the single-driver rule. ------------
    let mut reg_updates: HashMap<String, Expr> = HashMap::new();
    let mut out_drives: HashMap<String, Expr> = HashMap::new();
    for a in &m.assigns {
        chk.check_assign(a, &declared, &mut reg_updates, &mut out_drives)?;
    }

    // Every register must be driven exactly once; every output too.
    for r in &m.regs {
        if !reg_updates.contains_key(&r.name) {
            return Err(CompileError::semantic(
                &name,
                r.pos,
                format!("register `{}` has no next-state assignment (`<=`)", r.name),
            ));
        }
    }
    for p in m.ports.iter().filter(|p| p.dir == Dir::Out) {
        if !out_drives.contains_key(&p.name) {
            return Err(CompileError::semantic(
                &name,
                p.pos,
                format!("output `{}` is never driven (`=`)", p.name),
            ));
        }
    }

    // Wires are checked declare-before-use (a `let` value can only reference
    // names already in scope), so source order is a valid evaluation order and
    // a combinational cycle is unrepresentable — DESIGN §4.2 holds structurally.
    let ordered_lets = lets;

    let ports = m
        .ports
        .iter()
        .map(|p| ir::Port {
            dir: p.dir,
            name: p.name.clone(),
            ty: p.ty,
            frac_bits: 0,
        })
        .collect();

    // Preserve source order of registers / outputs for stable emission.
    let reg_updates = m
        .regs
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                reg_updates.remove(&r.name).expect("checked"),
            )
        })
        .collect();
    let out_drives = m
        .ports
        .iter()
        .filter(|p| p.dir == Dir::Out)
        .map(|p| (p.name.clone(), out_drives.remove(&p.name).expect("checked")))
        .collect();

    Ok(ir::Module {
        name,
        clock_domain: m.clock_domain.clone(),
        ports,
        regs,
        lets: ordered_lets,
        out_drives,
        reg_updates,
        // The legacy `module` surface has no memory primitive.
        mems: Vec::new(),
        mem_reads: Vec::new(),
        mem_writes: Vec::new(),
        // The legacy `module` surface has no `assert!` (functional surface only).
        asserts: Vec::new(),
        instances: Vec::new(),
    })
}

fn check_width(module: &str, ty: Type, pos: Pos) -> Result<(), CompileError> {
    if ty.width() > MAX_WIDTH {
        return Err(CompileError::semantic(
            module,
            pos,
            format!("type `{ty}` exceeds the current {MAX_WIDTH}-bit width limit"),
        ));
    }
    Ok(())
}

impl Checker<'_> {
    fn check_assign(
        &self,
        a: &Assign,
        declared: &HashSet<String>,
        reg_updates: &mut HashMap<String, Expr>,
        out_drives: &mut HashMap<String, Expr>,
    ) -> Result<(), CompileError> {
        if !declared.contains(&a.target) {
            return Err(CompileError::semantic(
                self.module,
                a.pos,
                format!("assignment to undeclared name `{}`", a.target),
            ));
        }
        match a.kind {
            AssignKind::NextState => {
                let Some(&ty) = self.regs.get(&a.target) else {
                    return Err(CompileError::semantic(
                        self.module,
                        a.pos,
                        format!("`<=` target `{}` is not a register", a.target),
                    ));
                };
                let value = self.check_expr(&a.value, Some(ty), a.pos)?;
                if reg_updates.insert(a.target.clone(), value).is_some() {
                    return Err(CompileError::semantic(
                        self.module,
                        a.pos,
                        format!("register `{}` is driven more than once", a.target),
                    ));
                }
            }
            AssignKind::Drive => {
                let Some(&ty) = self.outputs.get(&a.target) else {
                    return Err(CompileError::semantic(
                        self.module,
                        a.pos,
                        format!("`=` target `{}` is not an output port", a.target),
                    ));
                };
                let value = self.check_expr(&a.value, Some(ty), a.pos)?;
                if out_drives.insert(a.target.clone(), value).is_some() {
                    return Err(CompileError::semantic(
                        self.module,
                        a.pos,
                        format!("output `{}` is driven more than once", a.target),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Type-check `expr` against an optional expected type.
    fn check_expr(
        &self,
        expr: &AstExpr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        match expr {
            AstExpr::Literal(v) => self.check_literal(*v, expected, pos),
            // `include_hex!` is a functional-surface `rom` initializer, expanded to
            // an array before checking; it is not part of the legacy `module` path.
            AstExpr::IncludeHex(_) => Err(CompileError::semantic(
                "module",
                pos,
                "`include_hex!` is only valid as a functional-surface `rom` initializer",
            )),
            AstExpr::Ident(name) => self.check_ident(name, expected, pos),
            AstExpr::Unary { op, expr: inner } => self.check_unary(*op, inner, expected, pos),
            AstExpr::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, expected, pos),
            AstExpr::If { cond, then, els } => self.check_if(cond, then, els, expected, pos),
            // Functional-surface expressions are never produced by `module`
            // parsing, so they are unreachable here. Lowering them is a later
            // milestone (DESIGN §2.1). Listed explicitly (rather than `_`) so
            // adding a new legacy variant is a compile error, not silent.
            AstExpr::Path(_)
            | AstExpr::Field { .. }
            | AstExpr::Call { .. }
            | AstExpr::MethodCall { .. }
            | AstExpr::Index { .. }
            | AstExpr::Slice { .. }
            | AstExpr::StructLit { .. }
            | AstExpr::Tuple(_)
            | AstExpr::Array(_)
            | AstExpr::Some(_)
            | AstExpr::None
            | AstExpr::Try(_)
            | AstExpr::Cast { .. }
            | AstExpr::Match { .. }
            | AstExpr::Block(_) => Err(CompileError::semantic(
                self.module,
                pos,
                "this expression form is not supported in a `module` body",
            )),
        }
    }

    fn check_literal(
        &self,
        v: u128,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        let ty = expected.ok_or_else(|| {
            CompileError::semantic(
                self.module,
                pos,
                format!("cannot infer a type for literal `{v}`; add an annotation or operand type"),
            )
        })?;
        let w = ty.width();
        let v64 = u64::try_from(v).map_err(|_| {
            CompileError::semantic(self.module, pos, format!("literal `{v}` is too large"))
        })?;
        let fits = if ty.is_signed() {
            // Non-negative literals: the magnitude must fit in `width - 1` bits.
            w >= 1 && i128::from(v64) < (1i128 << (w - 1))
        } else {
            v64 <= umax(w)
        };
        if !fits {
            return Err(CompileError::semantic(
                self.module,
                pos,
                format!("literal `{v}` does not fit in `{ty}`"),
            ));
        }
        Ok(Expr {
            kind: ExprKind::Literal(mask(v64, w)),
            ty,
        })
    }

    fn check_ident(
        &self,
        name: &str,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        if self.outputs.contains_key(name) && !self.readable.contains_key(name) {
            return Err(CompileError::semantic(
                self.module,
                pos,
                format!("cannot read output port `{name}`"),
            ));
        }
        let ty = *self.readable.get(name).ok_or_else(|| {
            CompileError::semantic(self.module, pos, format!("unknown name `{name}`"))
        })?;
        if let Some(e) = expected {
            self.expect(e, ty, pos)?;
        }
        Ok(Expr {
            kind: ExprKind::Ident(name.to_string()),
            ty,
        })
    }

    fn check_unary(
        &self,
        op: UnaryOp,
        inner: &AstExpr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        match op {
            UnaryOp::Not => {
                if let Some(e) = expected {
                    self.expect(e, BOOL, pos)?;
                }
                let e = self.check_expr(inner, Some(BOOL), pos)?;
                Ok(Expr {
                    kind: ExprKind::Unary {
                        op,
                        expr: Box::new(e),
                    },
                    ty: BOOL,
                })
            }
            UnaryOp::BitNot => {
                let e = self.check_expr(inner, expected, pos)?;
                let ty = e.ty;
                Ok(Expr {
                    kind: ExprKind::Unary {
                        op,
                        expr: Box::new(e),
                    },
                    ty,
                })
            }
            UnaryOp::Neg => {
                let e = self.check_expr(inner, expected, pos)?;
                if !e.ty.is_signed() {
                    return Err(CompileError::semantic(
                        self.module,
                        pos,
                        format!("negation requires a signed operand, found `{}`", e.ty),
                    ));
                }
                let ty = e.ty;
                Ok(Expr {
                    kind: ExprKind::Unary {
                        op,
                        expr: Box::new(e),
                    },
                    ty,
                })
            }
        }
    }

    fn check_binary(
        &self,
        op: BinOp,
        lhs: &AstExpr,
        rhs: &AstExpr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        if op.is_comparison() {
            if let Some(e) = expected {
                self.expect(e, BOOL, pos)?;
            }
            // Operand type is inferred from a concrete side, never from `bool`.
            let opty = self
                .synth_concrete(lhs)
                .or_else(|| self.synth_concrete(rhs));
            let l = self.check_expr(lhs, opty, pos)?;
            let r = self.check_expr(rhs, Some(l.ty), pos)?;
            self.expect(l.ty, r.ty, pos)?;
            Ok(Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
                ty: BOOL,
            })
        } else {
            let opty = expected
                .or_else(|| self.synth_concrete(lhs))
                .or_else(|| self.synth_concrete(rhs));
            let l = self.check_expr(lhs, opty, pos)?;
            let r = self.check_expr(rhs, Some(l.ty), pos)?;
            self.expect(l.ty, r.ty, pos)?;
            let ty = l.ty;
            Ok(Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
                ty,
            })
        }
    }

    fn check_if(
        &self,
        cond: &AstExpr,
        then: &AstExpr,
        els: &AstExpr,
        expected: Option<Type>,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        let c = self.check_expr(cond, Some(BOOL), pos)?;
        let t = self.check_expr(then, expected, pos)?;
        let e = self.check_expr(els, Some(t.ty), pos)?;
        self.expect(t.ty, e.ty, pos)?;
        let ty = t.ty;
        Ok(Expr {
            kind: ExprKind::If {
                cond: Box::new(c),
                then: Box::new(t),
                els: Box::new(e),
            },
            ty,
        })
    }

    /// Best-effort synthesis of a concrete (non-literal) type, used to choose
    /// the operand type of arithmetic/comparison nodes. Returns `None` when the
    /// expression is entirely literal.
    fn synth_concrete(&self, expr: &AstExpr) -> Option<Type> {
        match expr {
            AstExpr::Literal(_) | AstExpr::IncludeHex(_) => None,
            AstExpr::Ident(name) => self.readable.get(name).copied(),
            AstExpr::Unary { op, expr } => match op {
                UnaryOp::Not => Some(BOOL),
                UnaryOp::BitNot | UnaryOp::Neg => self.synth_concrete(expr),
            },
            AstExpr::Binary { op, lhs, rhs } => {
                if op.is_comparison() {
                    Some(BOOL)
                } else {
                    self.synth_concrete(lhs)
                        .or_else(|| self.synth_concrete(rhs))
                }
            }
            AstExpr::If { then, els, .. } => self
                .synth_concrete(then)
                .or_else(|| self.synth_concrete(els)),
            // Functional-surface expressions never appear in a module body.
            // Listed explicitly so a new legacy variant fails to compile here.
            AstExpr::Path(_)
            | AstExpr::Field { .. }
            | AstExpr::Call { .. }
            | AstExpr::MethodCall { .. }
            | AstExpr::Index { .. }
            | AstExpr::Slice { .. }
            | AstExpr::StructLit { .. }
            | AstExpr::Tuple(_)
            | AstExpr::Array(_)
            | AstExpr::Some(_)
            | AstExpr::None
            | AstExpr::Try(_)
            | AstExpr::Cast { .. }
            | AstExpr::Match { .. }
            | AstExpr::Block(_) => None,
        }
    }

    fn expect(&self, expected: Type, found: Type, pos: Pos) -> Result<(), CompileError> {
        if expected != found {
            return Err(CompileError::semantic(
                self.module,
                pos,
                format!("type mismatch: expected `{expected}`, found `{found}`"),
            ));
        }
        Ok(())
    }

    /// Evaluate a constant expression (used for register reset values). Errors
    /// if the expression references any signal.
    fn const_eval(&self, expr: &Expr, pos: Pos) -> Result<u64, CompileError> {
        match &expr.kind {
            ExprKind::Literal(v) => Ok(*v),
            ExprKind::Ident(name) => Err(CompileError::semantic(
                self.module,
                pos,
                format!("reset value must be constant; `{name}` is not"),
            )),
            ExprKind::Unary { op, expr: inner } => {
                let v = self.const_eval(inner, pos)?;
                Ok(crate::eval::apply_unary(*op, v, inner.ty))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.const_eval(lhs, pos)?;
                let r = self.const_eval(rhs, pos)?;
                Ok(crate::eval::apply_binary(
                    *op, l, lhs.ty, r, rhs.ty, expr.ty,
                ))
            }
            ExprKind::If { cond, then, els } => {
                let c = self.const_eval(cond, pos)?;
                if c != 0 {
                    self.const_eval(then, pos)
                } else {
                    self.const_eval(els, pos)
                }
            }
            ExprKind::Cast { expr: inner, mode } => {
                let v = self.const_eval(inner, pos)?;
                Ok(crate::eval::apply_cast(v, inner.ty, expr.ty, *mode))
            }
            ExprKind::Shl {
                expr: inner,
                amount,
            } => {
                let v = self.const_eval(inner, pos)?;
                Ok(crate::eval::apply_shl(v, expr.ty.width(), *amount))
            }
            ExprKind::Shr {
                expr: inner,
                amount,
            } => {
                let v = self.const_eval(inner, pos)?;
                Ok(crate::eval::apply_shr(
                    v,
                    inner.ty,
                    expr.ty.width(),
                    *amount,
                ))
            }
            ExprKind::VarShift { .. } => Err(CompileError::semantic(
                self.module,
                pos,
                "a variable (runtime-amount) shift is not a constant expression".to_string(),
            )),
        }
    }
}
