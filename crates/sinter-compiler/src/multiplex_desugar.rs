//! Multiplex-function elaboration (DESIGN §3.2 / §3.3, milestone **M1**).
//!
//! A `multiplex fn` is the **time-folded** dual of a `pipeline fn` (see
//! [`crate::pipeline_desugar`], whose liveness analysis and helpers this pass
//! reuses). Where a pipeline runs every stage every cycle (a depth-`N` register
//! chain, one result/cycle), a multiplex folds the `yield`-segments onto **one
//! reused register per value** plus a small **phase counter**, processing one
//! datum every `N+1` cycles (`N` = number of `yield`s; latency `N`, static).
//! Exactly one phase is active per cycle, so a scarce resource can be reused.
//!
//! Like the pipeline pass it is a **source-to-source desugaring** run before
//! `check_functional`/`elaborate`: for each `multiplex fn M` it synthesizes a
//! `struct M__mux { ph, <v>_r, … }` + an `impl M__mux { fn advance(self, in…,
//! in_valid: u1) -> (M__mux, Out, u1) }`, and rewrites an owning field `inst: M`
//! to `inst: M__mux`.
//!
//! ## Operator sharing (the one-DSP win, §3.2)
//! Each phase may use **at most one** `*`. The desugar **hoists** the per-phase
//! multiply operands into phase-muxes and emits a single shared
//! `let shared = (lhs-mux) * (rhs-mux);` (each operand reads its latch for a later
//! phase, its live wire for its own phase). Because the elaborator inlines `let`s
//! by value, that single shared multiply is then made one *physical* multiplier by
//! the IR-level [`crate::ir_cse::share_muls`] pass (run after `elaborate`), which
//! hoists each distinct IR multiply into one wire. More than one multiply in a
//! phase is rejected.

use std::collections::HashSet;

use sinter_syntax::ast::{
    BinOp, Block, Expr, FieldInit, FnColor, Func, ImplItem, Item, Param, Pos, Program, Stmt,
    StructItem, Type, TypeRef, Visibility,
};

use crate::error::CompileError;
use crate::pipeline_desugar::{local_name, self_field, sem, zero_field, Plan};

/// Suffix of the synthesized instance struct (`<multiplex>__mux`).
const MUX_SUFFIX: &str = "__mux";
/// The synthesized phase-counter register / the shared-multiply local / the
/// input-valid parameter. Reserved as `multiplex fn` value names.
const PH: &str = "ph";
const SHARED: &str = "shared";
const IN_VALID: &str = "in_valid";

/// Desugar every `multiplex fn` in `program` into a synthesized instance struct +
/// `advance` method. A strict no-op (clone) when there is no `multiplex fn`.
///
/// # Errors
/// Returns a [`CompileError`] on an M1 restriction violation (see the module
/// docs) or a synthesized-name collision.
pub fn desugar_multiplexes(program: &Program) -> Result<Program, CompileError> {
    let muxes: Vec<&Func> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Func(f) if f.color == FnColor::Multiplex => Some(f),
            _ => None,
        })
        .collect();
    if muxes.is_empty() {
        return Ok(program.clone());
    }

    let mut type_names: HashSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Struct(s) => Some(s.name.clone()),
            Item::Enum(e) => Some(e.name.clone()),
            _ => None,
        })
        .collect();
    let mux_names: HashSet<String> = muxes.iter().map(|f| f.name.clone()).collect();

    let mut seen = HashSet::new();
    for f in &muxes {
        if !seen.insert(&f.name) {
            return Err(sem(
                &f.name,
                f.pos,
                format!("duplicate function `{}`", f.name),
            ));
        }
    }

    let mut items = program.items.clone();
    for f in &muxes {
        reject_reserved_names(f)?;
        let plan = Plan::build(f, "multiplex")?;
        reject_multi_mul(f, &plan)?;
        let struct_name = format!("{}{MUX_SUFFIX}", f.name);
        if !type_names.insert(struct_name.clone()) {
            return Err(sem(
                &f.name,
                f.pos,
                format!(
                    "synthesized multiplex struct `{struct_name}` collides with an existing type; \
                     rename the `multiplex fn`"
                ),
            ));
        }
        items.push(Item::Struct(synth_struct(&plan, &struct_name)));
        items.push(Item::Impl(synth_impl(&plan, &struct_name)));
    }

    // Rewrite owning fields `inst: <multiplex>` -> `inst: <multiplex>__mux`.
    for it in &mut items {
        if let Item::Struct(s) = it {
            for field in &mut s.fields {
                rewrite_mux_field(&mut field.ty, &mux_names, &s.name, field.pos)?;
            }
        }
    }

    Ok(Program {
        modules: program.modules.clone(),
        items,
    })
}

/// Reject a `multiplex fn` value named like a synthesized field/local (`ph` /
/// `shared` / `in_valid`) — a param, a top-level `let`, *or* a nested binder
/// (a block-local `let` or a `match`-arm pattern). The synthesized `advance`
/// injects `self.ph` / `shared` / `in_valid` inline, so a user binding of any of
/// those names would either be hijacked into the wrong read or shadow the
/// injected one; we reject all of them uniformly, before [`Plan::build`].
fn reject_reserved_names(f: &Func) -> Result<(), CompileError> {
    let ctx = &f.name;
    for p in &f.params {
        if let Param::Typed { name, pos, .. } = p {
            reject_reserved_name(name, ctx, *pos)?;
        }
    }
    reject_reserved_in_block(&f.body, ctx)
}

/// One reserved-name check (a param/binder named `ph` / `shared` / `in_valid`).
fn reject_reserved_name(name: &str, ctx: &str, pos: Pos) -> Result<(), CompileError> {
    if [PH, SHARED, IN_VALID].contains(&name) {
        return Err(sem(
            ctx,
            pos,
            format!("`{name}` is a reserved multiplex name"),
        ));
    }
    Ok(())
}

/// Walk a block's statements (top-level + nested) rejecting reserved binders.
fn reject_reserved_in_block(b: &Block, ctx: &str) -> Result<(), CompileError> {
    for stmt in &b.stmts {
        match stmt {
            Stmt::Let {
                pattern,
                value,
                pos,
                ..
            } => {
                reject_reserved_in_pattern(pattern, ctx, *pos)?;
                reject_reserved_in_expr(value, ctx)?;
            }
            Stmt::Yield { value, .. } => {
                if let Some(v) = value {
                    reject_reserved_in_expr(v, ctx)?;
                }
            }
        }
    }
    reject_reserved_in_expr(&b.tail, ctx)
}

/// Reject reserved names bound by a pattern (a binding or tuple components).
fn reject_reserved_in_pattern(
    pat: &sinter_syntax::ast::Pattern,
    ctx: &str,
    pos: Pos,
) -> Result<(), CompileError> {
    use sinter_syntax::ast::Pattern;
    match pat {
        Pattern::Binding(n) => reject_reserved_name(n, ctx, pos),
        Pattern::Tuple(ps) => ps
            .iter()
            .try_for_each(|p| reject_reserved_in_pattern(p, ctx, pos)),
        Pattern::Some(inner) => reject_reserved_in_pattern(inner, ctx, pos),
        Pattern::Wildcard | Pattern::None | Pattern::Literal(_) | Pattern::Path(_) => Ok(()),
    }
}

/// Recurse into an expression for nested binders (block-local `let`s and
/// `match`-arm patterns) that reuse a reserved multiplex name.
fn reject_reserved_in_expr(e: &Expr, ctx: &str) -> Result<(), CompileError> {
    match e {
        Expr::Block(b) => reject_reserved_in_block(b, ctx),
        Expr::Match { scrutinee, arms } => {
            reject_reserved_in_expr(scrutinee, ctx)?;
            for arm in arms {
                reject_reserved_in_pattern(&arm.pattern, ctx, Pos::default())?;
                reject_reserved_in_expr(&arm.body, ctx)?;
            }
            Ok(())
        }
        Expr::Unary { expr, .. } | Expr::Cast { expr, .. } => reject_reserved_in_expr(expr, ctx),
        Expr::Binary { lhs, rhs, .. } => {
            reject_reserved_in_expr(lhs, ctx)?;
            reject_reserved_in_expr(rhs, ctx)
        }
        Expr::If { cond, then, els } => {
            reject_reserved_in_expr(cond, ctx)?;
            reject_reserved_in_expr(then, ctx)?;
            reject_reserved_in_expr(els, ctx)
        }
        Expr::Field { base, .. } => reject_reserved_in_expr(base, ctx),
        Expr::Call { func, args } => {
            reject_reserved_in_expr(func, ctx)?;
            args.iter()
                .try_for_each(|a| reject_reserved_in_expr(a, ctx))
        }
        Expr::MethodCall { receiver, args, .. } => {
            reject_reserved_in_expr(receiver, ctx)?;
            args.iter()
                .try_for_each(|a| reject_reserved_in_expr(a, ctx))
        }
        Expr::Index { base, index } => {
            reject_reserved_in_expr(base, ctx)?;
            reject_reserved_in_expr(index, ctx)
        }
        Expr::StructLit { fields, .. } => fields
            .iter()
            .try_for_each(|fi| reject_reserved_in_expr(&fi.value, ctx)),
        Expr::Tuple(items) | Expr::Array(items) => items
            .iter()
            .try_for_each(|i| reject_reserved_in_expr(i, ctx)),
        Expr::Some(inner) | Expr::Try(inner) => reject_reserved_in_expr(inner, ctx),
        Expr::Literal(_) | Expr::Ident(_) | Expr::Path(_) | Expr::IncludeHex(_) | Expr::None => {
            Ok(())
        }
    }
}

/// Reject a phase (a `yield`-delimited segment, the tail included) that uses more
/// than one `*` — M1 shares exactly one multiplier, so at most one multiply per
/// phase (no intra-phase sharing).
fn reject_multi_mul(f: &Func, plan: &Plan) -> Result<(), CompileError> {
    for k in 0..=plan.latency {
        let mut n = 0;
        for local in plan.locals.iter().filter(|l| l.group == k) {
            n += count_muls(&local.rhs);
        }
        if k == plan.latency {
            n += count_muls(&plan.tail);
        }
        if n > 1 {
            return Err(sem(
                &f.name,
                f.pos,
                format!(
                    "multiplex phase {k} uses {n} multiplies; M1 allows at most one `*` per phase \
                     (one shared multiplier, no intra-phase sharing)"
                ),
            ));
        }
    }
    Ok(())
}

/// The synthesized instance struct: the phase counter + ONE register per value
/// live across any phase boundary, each defaulting to `0`.
fn synth_struct(plan: &Plan, struct_name: &str) -> StructItem {
    let mut fields = vec![zero_field(
        PH,
        TypeRef::Scalar(Type::Uint(ph_bits(plan.latency))),
    )];
    for v in plan.value_order() {
        if (0..plan.latency).any(|k| plan.live_across(&v, k)) {
            let ty = plan.carried_ty.get(&v).cloned().expect("carried type set");
            fields.push(zero_field(&reg_name(&v), ty));
        }
    }
    StructItem {
        attrs: Vec::new(),
        vis: Visibility::Private,
        name: struct_name.to_string(),
        fields,
        pos: Pos::default(),
    }
}

/// The synthesized `impl <struct_name> { fn advance(...) -> (Self, Out, u1) }`.
fn synth_impl(plan: &Plan, struct_name: &str) -> ImplItem {
    let n = plan.latency; // last phase index; phases 0..=n
    let kbits = ph_bits(n);

    let mut adv_params = vec![Param::SelfRef {
        pos: Pos::default(),
    }];
    for (name, ty) in &plan.params {
        adv_params.push(Param::Typed {
            name: name.clone(),
            ty: ty.clone(),
            pos: Pos::default(),
        });
    }
    adv_params.push(Param::Typed {
        name: IN_VALID.to_string(),
        ty: TypeRef::Scalar(Type::Uint(1)),
        pos: Pos::default(),
    });

    // Rewrite every segment's logic + the tail, collecting the (phase, lhs, rhs)
    // operands of each phase's multiply.
    let mut muls: Vec<(usize, Expr, Expr)> = Vec::new();
    let mut local_lets = Vec::new();
    for local in &plan.locals {
        local_lets.push(Stmt::Let {
            pattern: sinter_syntax::ast::Pattern::Binding(local_name(&local.name, local.group)),
            ty: None,
            value: mux_rewrite(plan, &local.rhs, local.group, &mut muls),
            pos: Pos::default(),
        });
    }
    let payload = mux_rewrite(plan, &plan.tail, n, &mut muls);

    // The single shared multiplier, operands phase-muxed (one Binary{Mul}).
    let mut stmts = Vec::new();
    if !muls.is_empty() {
        let lhs: Vec<(usize, Expr)> = muls.iter().map(|(s, l, _)| (*s, l.clone())).collect();
        let rhs: Vec<(usize, Expr)> = muls.iter().map(|(s, _, r)| (*s, r.clone())).collect();
        stmts.push(Stmt::Let {
            pattern: sinter_syntax::ast::Pattern::Binding(SHARED.to_string()),
            ty: None,
            value: Expr::Binary {
                op: BinOp::Mul,
                lhs: Box::new(phase_mux(&lhs)),
                rhs: Box::new(phase_mux(&rhs)),
            },
            pos: Pos::default(),
        });
    }
    stmts.extend(local_lets);

    // Next state: `ph` counter wrap + each register's write-when-active/hold.
    let mut sfields = vec![FieldInit {
        name: PH.to_string(),
        value: next_ph(kbits, n),
    }];
    for v in plan.value_order() {
        if (0..plan.latency).any(|k| plan.live_across(&v, k)) {
            sfields.push(FieldInit {
                name: reg_name(&v),
                value: mux_next_state(plan, &v),
            });
        }
    }
    let next_struct = Expr::StructLit {
        name: struct_name.to_string(),
        fields: sfields,
    };
    let valid = eq_ph(n); // valid at the final phase
    let tail = Expr::Tuple(vec![next_struct, payload, valid]);

    let ret = TypeRef::Tuple(vec![
        TypeRef::Named(struct_name.to_string()),
        plan.out_ty.clone(),
        TypeRef::Scalar(Type::Uint(1)),
    ]);
    let advance = Func {
        attrs: Vec::new(),
        vis: Visibility::Public,
        color: FnColor::Comb,
        name: "advance".to_string(),
        params: adv_params,
        ret,
        body: Block {
            stmts,
            tail: Box::new(tail),
            pos: Pos::default(),
        },
        pos: Pos::default(),
    };
    ImplItem {
        type_name: struct_name.to_string(),
        methods: vec![advance],
        pos: Pos::default(),
    }
}

/// Each register's next-state: write the freshly-computed value when its
/// producing phase is active, otherwise hold (a guarded hold on the single
/// reused register).
fn mux_next_state(plan: &Plan, v: &str) -> Expr {
    let def = plan.def_group.get(v).copied().unwrap_or(0);
    let regty = plan.carried_ty.get(v).cloned().expect("carried type set");
    let val = if plan.is_param.contains(v) {
        Expr::Ident(v.to_string())
    } else {
        Expr::Ident(local_name(v, def))
    };
    Expr::If {
        cond: Box::new(eq_ph(def)),
        // Pin the width so both `if` arms unify to the register width.
        then: Box::new(Expr::Cast {
            expr: Box::new(val),
            ty: regty,
            mode: sinter_syntax::ast::CastMode::Wrap,
        }),
        els: Box::new(self_field(&reg_name(v))),
    }
}

/// Rewrite an expression in phase `seg`: a value from an earlier phase reads its
/// single latch (`self.<v>_r`); a same-phase param reads its live `advance`
/// parameter, a same-phase local its per-phase name; and THE phase's multiply is
/// recorded (operands, for the shared mux) and replaced by `Ident("shared")`.
fn mux_rewrite(plan: &Plan, e: &Expr, seg: usize, muls: &mut Vec<(usize, Expr, Expr)>) -> Expr {
    match e {
        Expr::Binary {
            op: BinOp::Mul,
            lhs,
            rhs,
        } => {
            let l = mux_rewrite(plan, lhs, seg, muls);
            let r = mux_rewrite(plan, rhs, seg, muls);
            muls.push((seg, l, r));
            Expr::Ident(SHARED.to_string())
        }
        Expr::Ident(x) => match plan.def_group.get(x) {
            Some(&dg) if dg == seg => {
                if plan.is_param.contains(x) {
                    Expr::Ident(x.clone())
                } else {
                    Expr::Ident(local_name(x, seg))
                }
            }
            Some(_) => self_field(&reg_name(x)),
            None => Expr::Ident(x.clone()),
        },
        Expr::Some(inner) => Expr::Some(Box::new(mux_rewrite(plan, inner, seg, muls))),
        Expr::Try(inner) => Expr::Try(Box::new(mux_rewrite(plan, inner, seg, muls))),
        Expr::Literal(_) | Expr::Path(_) | Expr::IncludeHex(_) | Expr::None => e.clone(),
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(mux_rewrite(plan, expr, seg, muls)),
        },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(mux_rewrite(plan, lhs, seg, muls)),
            rhs: Box::new(mux_rewrite(plan, rhs, seg, muls)),
        },
        Expr::If { cond, then, els } => Expr::If {
            cond: Box::new(mux_rewrite(plan, cond, seg, muls)),
            then: Box::new(mux_rewrite(plan, then, seg, muls)),
            els: Box::new(mux_rewrite(plan, els, seg, muls)),
        },
        Expr::Field { base, field } => Expr::Field {
            base: Box::new(mux_rewrite(plan, base, seg, muls)),
            field: field.clone(),
        },
        Expr::Call { func, args } => Expr::Call {
            func: Box::new(mux_rewrite(plan, func, seg, muls)),
            args: args
                .iter()
                .map(|a| mux_rewrite(plan, a, seg, muls))
                .collect(),
        },
        Expr::MethodCall {
            receiver,
            method,
            args,
        } => Expr::MethodCall {
            receiver: Box::new(mux_rewrite(plan, receiver, seg, muls)),
            method: method.clone(),
            args: args
                .iter()
                .map(|a| mux_rewrite(plan, a, seg, muls))
                .collect(),
        },
        Expr::Index { base, index } => Expr::Index {
            base: Box::new(mux_rewrite(plan, base, seg, muls)),
            index: Box::new(mux_rewrite(plan, index, seg, muls)),
        },
        Expr::StructLit { name, fields } => Expr::StructLit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|fi| FieldInit {
                    name: fi.name.clone(),
                    value: mux_rewrite(plan, &fi.value, seg, muls),
                })
                .collect(),
        },
        Expr::Tuple(items) => Expr::Tuple(
            items
                .iter()
                .map(|i| mux_rewrite(plan, i, seg, muls))
                .collect(),
        ),
        Expr::Array(items) => Expr::Array(
            items
                .iter()
                .map(|i| mux_rewrite(plan, i, seg, muls))
                .collect(),
        ),
        Expr::Cast { expr, ty, mode } => Expr::Cast {
            expr: Box::new(mux_rewrite(plan, expr, seg, muls)),
            ty: ty.clone(),
            mode: *mode,
        },
        Expr::Match { scrutinee, arms } => Expr::Match {
            scrutinee: Box::new(mux_rewrite(plan, scrutinee, seg, muls)),
            arms: arms
                .iter()
                .map(|arm| sinter_syntax::ast::MatchArm {
                    pattern: arm.pattern.clone(),
                    body: mux_rewrite(plan, &arm.body, seg, muls),
                })
                .collect(),
        },
        Expr::Block(b) => Expr::Block(Block {
            stmts: b
                .stmts
                .iter()
                .map(|s| match s {
                    Stmt::Let {
                        pattern,
                        ty,
                        value,
                        pos,
                    } => Stmt::Let {
                        pattern: pattern.clone(),
                        ty: ty.clone(),
                        value: mux_rewrite(plan, value, seg, muls),
                        pos: *pos,
                    },
                    Stmt::Yield { value, pos } => Stmt::Yield {
                        value: value.clone(),
                        pos: *pos,
                    },
                })
                .collect(),
            tail: Box::new(mux_rewrite(plan, &b.tail, seg, muls)),
            pos: b.pos,
        }),
    }
}

/// Build a phase-mux `if ph == s0 { op0 } else if ph == s1 { … } else { op_last }`
/// over `(phase, operand)` items (≥1; a phase with no multiply reuses a real
/// operand via the catch-all, harmless since it is muxed away).
fn phase_mux(items: &[(usize, Expr)]) -> Expr {
    let mut rev = items.iter().rev();
    let (_, last) = rev.next().expect("at least one multiplying phase");
    let mut acc = last.clone();
    for (seg, op) in rev {
        acc = Expr::If {
            cond: Box::new(eq_ph(*seg)),
            then: Box::new(op.clone()),
            els: Box::new(acc),
        };
    }
    acc
}

/// The phase-counter next-state: wrap to 0 at the last phase, else increment.
/// The `ph` struct field supplies the expected `uK` type, so the bare `0` arm
/// needs no cast; only the widening `ph + 1` arm is cast back to the counter
/// width (so the `if`-mux unifies to `uK`).
fn next_ph(kbits: u32, last: usize) -> Expr {
    Expr::If {
        cond: Box::new(eq_ph(last)),
        then: Box::new(Expr::Literal(0)),
        els: Box::new(Expr::Cast {
            expr: Box::new(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(self_field(PH)),
                rhs: Box::new(Expr::Literal(1)),
            }),
            ty: TypeRef::Scalar(Type::Uint(kbits)),
            mode: sinter_syntax::ast::CastMode::Wrap,
        }),
    }
}

/// `self.ph == k` (a `u1`).
fn eq_ph(k: usize) -> Expr {
    Expr::Binary {
        op: BinOp::Eq,
        lhs: Box::new(self_field(PH)),
        rhs: Box::new(Expr::Literal(k as u128)),
    }
}

/// The single latch register name for value `v`.
fn reg_name(v: &str) -> String {
    format!("{v}_r")
}

/// Counter width: bits to hold phases `0..=N` (`N+1` values), at least 1.
fn ph_bits(latency: usize) -> u32 {
    let phases = latency + 1;
    let mut b = 1u32;
    while (1usize << b) < phases {
        b += 1;
    }
    b
}

/// Count every `*` reachable in an expression (including nested in casts /
/// `if` / `match` arms / blocks).
fn count_muls(e: &Expr) -> usize {
    match e {
        Expr::Binary {
            op: BinOp::Mul,
            lhs,
            rhs,
        } => 1 + count_muls(lhs) + count_muls(rhs),
        Expr::Binary { lhs, rhs, .. } => count_muls(lhs) + count_muls(rhs),
        Expr::Unary { expr, .. } | Expr::Cast { expr, .. } => count_muls(expr),
        Expr::If { cond, then, els } => count_muls(cond) + count_muls(then) + count_muls(els),
        Expr::Field { base, .. } => count_muls(base),
        Expr::Call { func, args } => count_muls(func) + args.iter().map(count_muls).sum::<usize>(),
        Expr::MethodCall { receiver, args, .. } => {
            count_muls(receiver) + args.iter().map(count_muls).sum::<usize>()
        }
        Expr::Index { base, index } => count_muls(base) + count_muls(index),
        Expr::StructLit { fields, .. } => fields.iter().map(|fi| count_muls(&fi.value)).sum(),
        Expr::Tuple(items) | Expr::Array(items) => items.iter().map(count_muls).sum(),
        Expr::Match { scrutinee, arms } => {
            count_muls(scrutinee) + arms.iter().map(|a| count_muls(&a.body)).sum::<usize>()
        }
        Expr::Block(b) => {
            b.stmts
                .iter()
                .map(|s| match s {
                    Stmt::Let { value, .. } => count_muls(value),
                    Stmt::Yield { .. } => 0,
                })
                .sum::<usize>()
                + count_muls(&b.tail)
        }
        Expr::Some(inner) | Expr::Try(inner) => count_muls(inner),
        Expr::Literal(_) | Expr::Ident(_) | Expr::Path(_) | Expr::IncludeHex(_) | Expr::None => 0,
    }
}

/// Rewrite an owning struct field `inst: M` to `inst: M__mux` (a multiplex
/// instance must be a plain field; nested in an array/tuple is rejected).
fn rewrite_mux_field(
    ty: &mut TypeRef,
    mux_names: &HashSet<String>,
    ctx: &str,
    pos: Pos,
) -> Result<(), CompileError> {
    match ty {
        TypeRef::Named(n) if mux_names.contains(n) => {
            *n = format!("{n}{MUX_SUFFIX}");
            Ok(())
        }
        TypeRef::Array { elem, .. } => reject_nested_mux(elem, mux_names, ctx, pos),
        TypeRef::Tuple(ts) => ts
            .iter()
            .try_for_each(|t| reject_nested_mux(t, mux_names, ctx, pos)),
        _ => Ok(()),
    }
}

/// Reject a multiplex instance nested inside an array/tuple field (M1 holds a
/// multiplex only in a plain field).
fn reject_nested_mux(
    ty: &TypeRef,
    mux_names: &HashSet<String>,
    ctx: &str,
    pos: Pos,
) -> Result<(), CompileError> {
    match ty {
        TypeRef::Named(n) if mux_names.contains(n) => Err(sem(
            ctx,
            pos,
            format!(
                "a multiplex instance (`{n}`) must be held in a plain field, not nested in an \
                 array or tuple"
            ),
        )),
        TypeRef::Array { elem, .. } => reject_nested_mux(elem, mux_names, ctx, pos),
        TypeRef::Tuple(ts) => ts
            .iter()
            .try_for_each(|t| reject_nested_mux(t, mux_names, ctx, pos)),
        _ => Ok(()),
    }
}
