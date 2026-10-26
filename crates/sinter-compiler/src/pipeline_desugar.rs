//! Pipeline-function elaboration (DESIGN §3.1 / §9.3, milestone **PL3a**).
//!
//! A `pipeline fn` is lowered by a **source-to-source desugaring** that runs as a
//! distinct pass on the already-resolved [`Program`], *before*
//! [`crate::functional::check_functional`] and [`crate::elaborate`]. For each
//!
//! ```text
//! pipeline fn f(in...) -> Out { ...; yield; ...; yield; tail }
//! ```
//!
//! we synthesize two ordinary AST items:
//!
//! 1. a plain `struct f__pipe { ... }` whose fields are one **valid bit** per
//!    stage (`v0..v{L-1}`, `L` = number of `yield`s = latency) plus, for every
//!    value (input param or `let`-local) that is *live across* a `yield`
//!    boundary, a **stage register** `<name>_s{k}` — every field defaulting to
//!    `0`; and
//! 2. an `impl f__pipe { fn advance(self, <in...>) -> (f__pipe, Out, u1) { ... } }`
//!    whose body is the staged register shift, written purely with constructs the
//!    existing lowerer already handles (`let`, struct-literal tail, tuple return,
//!    `self.field`).
//!
//! The desugarer also rewrites the owning struct's field `inst: f` to
//! `inst: f__pipe`. Because the synthesized items are ordinary AST, the existing
//! sub-instance + `(new_state, output)` machinery
//! ([`crate::elaborate`] inlines `advance` exactly like any sub-step method and
//! flattens the instance's fields to leaf registers) lowers them with **no
//! changes to the IR, simulator, or Verilog backend** — so the sim↔Verilog
//! oracle (DESIGN §7.4) holds by construction.
//!
//! ## Validity / reset
//! `Option<Out>` is erased here into a `(payload, valid: u1)` pair. The output
//! `valid` reads the last stage's valid bit (`self.v{L-1}`); since every valid
//! bit resets to `0` and the chain is `L` deep, the output is *invalid* (a bubble)
//! for the first `L` cycles after reset — "None during fill" — then valid.
//!
//! ## Input bubbles (PL3b)
//! `advance` takes a trailing `in_valid: u1`: `v0`'s next state is `in_valid`, so
//! `in_valid = 0` injects a **bubble** that the valid-bit chain carries to the
//! output `L` cycles later (a bubble in ⇒ a bubble out at the right latency); a
//! free-running caller passes `1`. (`Option<In>` is the erased `(payload,
//! in_valid)`; the `Option<T>` sugar and `?`-propagation are a later milestone.)
//!
//! ## Phase-1 restrictions (rejected with a clear error)
//! - a value carried across a `yield` must have an explicit width-fixing cast at
//!   its `let` (so its register type is readable from the source); inputs use
//!   their declared type;
//! - `let` patterns in a `pipeline fn` must be simple bindings (no tuple / `_`
//!   destructuring), and a name may not be redefined across stages;
//! - `yield` may only appear at the top level of the body (no nesting);
//! - the output type is a single value (already enforced upstream).

use std::collections::{HashMap, HashSet};

use sinter_syntax::ast::{
    Block, Expr, ExprKind, FieldInit, FnColor, Func, Item, Param, Pattern, Pos, Program, Stmt,
    StructField, StructItem, Type, TypeRef, Visibility,
};

use crate::error::CompileError;

/// Suffix of the synthesized instance struct (`<pipeline>__pipe`).
const PIPE_SUFFIX: &str = "__pipe";

/// The synthesized trailing `advance` parameter carrying input validity (PL3b):
/// `1` admits a datum this cycle, `0` injects a bubble. Reserved as a pipeline
/// value name.
const IN_VALID: &str = "in_valid";

/// Desugar every `pipeline fn` in `program` into a synthesized instance struct +
/// `advance` method, returning the augmented program. A strict no-op (clone) when
/// the program has no `pipeline fn`.
///
/// # Errors
/// Returns a [`CompileError`] if a pipeline violates a PL3a restriction (see the
/// module docs) or its synthesized struct name collides with an existing type.
pub fn desugar_pipelines(program: &Program) -> Result<Program, CompileError> {
    let pipelines: Vec<&Func> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Func(f) if f.color == FnColor::Pipeline => Some(f),
            _ => None,
        })
        .collect();
    if pipelines.is_empty() {
        return Ok(program.clone());
    }

    // Names already taken by a struct/enum — the synthesized `<f>__pipe` must not
    // collide. (Two pipelines with the same name are caught later as a duplicate
    // function; their identical `__pipe` names are caught here too.)
    let mut type_names: HashSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Struct(s) => Some(s.name.clone()),
            Item::Enum(e) => Some(e.name.clone()),
            _ => None,
        })
        .collect();
    let pipe_names: HashSet<String> = pipelines.iter().map(|f| f.name.clone()).collect();

    // Duplicate pipeline names would otherwise surface as a confusing
    // "`f__pipe` collides" error; report them as the duplicate functions they are
    // (matching `functional::build_tables` and the multi-file resolve path).
    let mut seen = HashSet::new();
    for f in &pipelines {
        if !seen.insert(&f.name) {
            return Err(sem(
                &f.name,
                f.pos,
                format!("duplicate function `{}`", f.name),
            ));
        }
    }

    let mut items = program.items.clone();
    for f in &pipelines {
        let plan = Plan::build(f, "pipeline")?;
        let struct_name = format!("{}{PIPE_SUFFIX}", f.name);
        if !type_names.insert(struct_name.clone()) {
            return Err(sem(
                &f.name,
                f.pos,
                format!(
                    "synthesized pipeline struct `{struct_name}` collides with an existing type; \
                     rename the `pipeline fn`"
                ),
            ));
        }
        items.push(Item::Struct(plan.synth_struct(&struct_name)));
        items.push(Item::Impl(plan.synth_impl(&struct_name)));
    }

    // Rewrite owning fields `inst: <pipeline>` -> `inst: <pipeline>__pipe`. A
    // pipeline instance must be a plain field in PL3a; one nested inside an
    // array/tuple field is rejected (rather than silently left as an unknown type).
    for it in &mut items {
        if let Item::Struct(s) = it {
            for field in &mut s.fields {
                rewrite_field_ty(&mut field.ty, &pipe_names, &s.name, field.pos, true)?;
            }
        }
    }

    Ok(Program { items })
}

/// A single `let`-local of a pipeline body.
pub(crate) struct Local {
    pub(crate) name: String,
    /// 0-based stage the `let` lives in (segment index).
    pub(crate) group: usize,
    /// The bound expression.
    pub(crate) rhs: Expr,
    /// The optional `let` type annotation (a fallback width source for a carried
    /// value when the `let` has no explicit cast).
    pub(crate) ty: Option<TypeRef>,
    /// Source position.
    pub(crate) pos: Pos,
}

/// The analyzed plan for one `pipeline fn`, ready to synthesize from.
pub(crate) struct Plan {
    /// `advance` parameters (the pipeline params, in order): `(name, type)`.
    pub(crate) params: Vec<(String, TypeRef)>,
    /// Latency = number of `yield`s.
    pub(crate) latency: usize,
    /// The output (tail) type.
    pub(crate) out_ty: TypeRef,
    /// The tail expression (output payload, in the final segment `L`).
    pub(crate) tail: Expr,
    /// `let`-locals across all segments, in source order.
    pub(crate) locals: Vec<Local>,
    /// Defining segment of every value (param or local).
    pub(crate) def_group: HashMap<String, usize>,
    /// Whether a name is an input parameter.
    pub(crate) is_param: HashSet<String>,
    /// Highest segment a value is *read* in (the tail counts as segment `L`).
    pub(crate) max_use: HashMap<String, usize>,
    /// The carried register type of each value (param type, or a local's cast).
    pub(crate) carried_ty: HashMap<String, TypeRef>,
}

impl Plan {
    /// Analyze a staged function (`pipeline`/`multiplex`) into a [`Plan`]. `kind`
    /// is the function-color noun used in the structural-restriction diagnostics
    /// (`"pipeline"` or `"multiplex"`), since the same analysis backs both colors.
    pub(crate) fn build(f: &Func, kind: &str) -> Result<Plan, CompileError> {
        let ctx = &f.name;

        // Parameters (a staged fn takes no `self`; PL2 already rejects it).
        let mut params = Vec::new();
        for p in &f.params {
            match p {
                Param::SelfRef { pos } => {
                    return Err(sem(ctx, *pos, format!("a `{kind} fn` cannot take `self`")))
                }
                Param::Typed { name, ty, pos } => {
                    if name == IN_VALID {
                        return Err(sem(
                            ctx,
                            *pos,
                            format!("`{IN_VALID}` is a reserved {kind} parameter name"),
                        ));
                    }
                    params.push((name.clone(), ty.clone()));
                }
            }
        }

        // Split the flat statement list on top-level `yield`s into segments. The
        // tail expression belongs to the final segment (`L`).
        reject_nested_yields(&f.body, ctx)?;
        let mut groups: Vec<Vec<Local>> = vec![Vec::new()];
        for stmt in &f.body.stmts {
            match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => {
                    let Pattern::Binding(name) = pattern else {
                        return Err(sem(
                            ctx,
                            *pos,
                            format!(
                                "a `{kind} fn` `let` must bind a simple name (no tuple / `_` \
                                 destructuring)"
                            ),
                        ));
                    };
                    let group = groups.len() - 1;
                    groups
                        .last_mut()
                        .expect("at least one segment")
                        .push(Local {
                            name: name.clone(),
                            group,
                            rhs: value.clone(),
                            ty: ty.clone(),
                            pos: *pos,
                        });
                }
                Stmt::Yield { value, pos } => {
                    if value.is_some() {
                        return Err(sem(
                            ctx,
                            *pos,
                            format!(
                                "`yield <expr>` taps are not part of the {kind} model (final output \
                                 only); use a bare `yield;`"
                            ),
                        ));
                    }
                    groups.push(Vec::new());
                }
                // FLT0 scopes `assert!` to combinational / `#[clock]` bodies; a
                // staged color splits the body across stage registers (deferred).
                Stmt::Assert { pos, .. } => {
                    return Err(sem(
                        ctx,
                        *pos,
                        format!("`assert!` is not yet supported inside a `{kind} fn` (FLT0)"),
                    ))
                }
            }
        }
        let latency = groups.len() - 1;
        if latency == 0 {
            return Err(sem(
                ctx,
                f.body.pos,
                format!("a `{kind} fn` needs at least one `yield` boundary"),
            ));
        }

        // Value bookkeeping: defining segment, param-ness, and (for locals) order.
        let mut def_group: HashMap<String, usize> = HashMap::new();
        let mut is_param: HashSet<String> = HashSet::new();
        for (name, _) in &params {
            if def_group.insert(name.clone(), 0).is_some() {
                return Err(sem(ctx, f.pos, format!("duplicate parameter `{name}`")));
            }
            is_param.insert(name.clone());
        }
        let mut locals: Vec<Local> = Vec::new();
        for group in &mut groups {
            for local in group.drain(..) {
                if local.name == IN_VALID {
                    return Err(sem(
                        ctx,
                        local.pos,
                        format!("`{IN_VALID}` is a reserved {kind} local name"),
                    ));
                }
                if def_group.insert(local.name.clone(), local.group).is_some() {
                    return Err(sem(
                        ctx,
                        local.pos,
                        format!(
                            "a `{kind} fn` cannot redefine `{}` (redefining a value across `yield` \
                             boundaries is not supported)",
                            local.name
                        ),
                    ));
                }
                locals.push(local);
            }
        }

        // A nested binder (a block-local `let`, or a `match`-arm pattern binding)
        // that shadows a pipeline value would be miscompiled: the rewrite below
        // resolves identifiers by name against `def_group`, so a shadowed name
        // would be hijacked into a stage-register read. PL3a rejects such shadows
        // (consistent with the top-level no-redefine rule above); full lexical
        // scoping is a later refinement.
        for local in &locals {
            reject_shadowing(&local.rhs, &def_group, ctx)?;
        }
        reject_shadowing(&f.body.tail, &def_group, ctx)?;

        // Uses: the highest segment each value is read in (tail counts as `L`).
        let mut max_use: HashMap<String, usize> = HashMap::new();
        let record_use = |id: &str, seg: usize, mu: &mut HashMap<String, usize>| {
            let e = mu.entry(id.to_string()).or_insert(0);
            *e = (*e).max(seg);
        };
        for local in &locals {
            for id in idents(&local.rhs) {
                record_use(&id, local.group, &mut max_use);
            }
        }
        for id in idents(&f.body.tail) {
            record_use(&id, latency, &mut max_use);
        }

        let mut plan = Plan {
            params,
            latency,
            out_ty: f.ret.clone(),
            tail: (*f.body.tail).clone(),
            locals,
            def_group,
            is_param,
            max_use,
            carried_ty: HashMap::new(),
        };

        // Type every value carried across at least one boundary: a param uses its
        // declared type; a `let`-local must carry an explicit width-fixing cast.
        for v in plan.value_order() {
            if (0..plan.latency).any(|k| plan.live_across(&v, k)) {
                let ty = plan.carried_type(&v, ctx)?;
                plan.carried_ty.insert(v, ty);
            }
        }
        Ok(plan)
    }

    /// All values in deterministic order: parameters first, then `let`-locals in
    /// source order. Used to order synthesized fields.
    pub(crate) fn value_order(&self) -> Vec<String> {
        let mut order: Vec<String> = self.params.iter().map(|(n, _)| n.clone()).collect();
        order.extend(self.locals.iter().map(|l| l.name.clone()));
        order
    }

    /// Whether value `v` must be held in a register across boundary `k`: defined
    /// at or before `k` and still read after `k`.
    pub(crate) fn live_across(&self, v: &str, k: usize) -> bool {
        let def = self.def_group.get(v).copied().unwrap_or(0);
        let last = self.max_use.get(v).copied().unwrap_or(0);
        def <= k && last > k
    }

    /// The carried register type of `v`: a param's declared type, or — for a
    /// `let`-local — its explicit cast target or `let` type annotation. One of
    /// those two is required so the register width is readable from the source
    /// without re-running type inference, and the type must be a scalar (a carried
    /// aggregate has no `0` reset literal and is out of scope this milestone).
    pub(crate) fn carried_type(&self, v: &str, ctx: &str) -> Result<TypeRef, CompileError> {
        let (ty, pos) = if let Some((_, ty)) = self.params.iter().find(|(n, _)| n == v) {
            (ty.clone(), Pos::default())
        } else {
            let local = self
                .locals
                .iter()
                .find(|l| l.name == v)
                .expect("carried value is a param or local");
            let ty = match &local.rhs.kind {
                ExprKind::Cast { ty, .. } => ty.clone(),
                _ => local.ty.clone().ok_or_else(|| {
                    sem(
                        ctx,
                        local.pos,
                        format!(
                            "value `{v}` is carried across a `yield` and needs an explicit \
                             width-fixing cast (e.g. `as u16 wrap`) or a `let` type annotation"
                        ),
                    )
                })?,
            };
            (ty, local.pos)
        };
        if !matches!(ty, TypeRef::Scalar(_) | TypeRef::Fixed { .. }) {
            return Err(sem(
                ctx,
                pos,
                format!(
                    "value `{v}` is carried across a `yield` but its type is not a scalar; a \
                     carried value must be a scalar (`uN` / `iN` / fixed-point) in this milestone"
                ),
            ));
        }
        Ok(ty)
    }

    /// The synthesized instance struct: valid bits then carried stage registers,
    /// each defaulting to `0`.
    fn synth_struct(&self, struct_name: &str) -> StructItem {
        let mut fields = Vec::new();
        for k in 0..self.latency {
            fields.push(zero_field(&format!("v{k}"), TypeRef::Scalar(Type::Uint(1))));
        }
        for k in 0..self.latency {
            for v in self.value_order() {
                if self.live_across(&v, k) {
                    let ty = self.carried_ty.get(&v).cloned().expect("carried type set");
                    fields.push(zero_field(&reg_name(&v, k), ty));
                }
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
    fn synth_impl(&self, struct_name: &str) -> sinter_syntax::ast::ImplItem {
        // Params: `self`, the pipeline inputs, then the trailing `in_valid: u1`
        // (PL3b input validity; pass `1` for a free-running caller).
        let mut adv_params = vec![Param::SelfRef {
            pos: Pos::default(),
        }];
        for (name, ty) in &self.params {
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

        // Body: each segment's `let`s, renamed per-segment and with references
        // rewritten to read carried values from `self`.
        let mut stmts = Vec::new();
        for local in &self.locals {
            stmts.push(Stmt::Let {
                pattern: Pattern::Binding(local_name(&local.name, local.group)),
                ty: None,
                value: self.rewrite(&local.rhs, local.group),
                pos: Pos::default(),
            });
        }

        // Tail: `(<struct> { ...next state... }, <payload>, <valid>)`.
        let mut sfields = Vec::new();
        for k in 0..self.latency {
            let value = if k == 0 {
                // Stage 0 admits a datum iff `in_valid` (a bubble when 0); the
                // wavefront's validity then marches down the chain (PL3b).
                Expr::from(ExprKind::Ident(IN_VALID.to_string()))
            } else {
                self_field(&format!("v{}", k - 1))
            };
            sfields.push(FieldInit {
                name: format!("v{k}"),
                value,
            });
        }
        for k in 0..self.latency {
            for v in self.value_order() {
                if self.live_across(&v, k) {
                    sfields.push(FieldInit {
                        name: reg_name(&v, k),
                        value: self.next_state(&v, k),
                    });
                }
            }
        }
        let next_struct = ExprKind::StructLit {
            name: struct_name.to_string(),
            fields: sfields,
            base: None,
        };
        let payload = self.rewrite(&self.tail, self.latency);
        let valid = self_field(&format!("v{}", self.latency - 1));
        let tail = ExprKind::Tuple(vec![Expr::from(next_struct), payload, valid]);

        let ret = TypeRef::Tuple(vec![
            TypeRef::Named(struct_name.to_string()),
            self.out_ty.clone(),
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
                tail: Box::new(Expr::from(tail)),
                pos: Pos::default(),
            },
            pos: Pos::default(),
        };
        sinter_syntax::ast::ImplItem {
            type_name: struct_name.to_string(),
            methods: vec![advance],
            pos: Pos::default(),
        }
    }

    /// The next-state value for carried register `<v>_s{k}`: this cycle's computed
    /// value when `v` is defined in segment `k`, otherwise the previous stage's
    /// register (the shift `<v>_s{k} <= self.<v>_s{k-1}`).
    fn next_state(&self, v: &str, k: usize) -> Expr {
        if self.def_group.get(v).copied().unwrap_or(0) == k {
            if self.is_param.contains(v) {
                Expr::from(ExprKind::Ident(v.to_string())) // a carried input enters at boundary 0
            } else {
                Expr::from(ExprKind::Ident(local_name(v, k)))
            }
        } else {
            self_field(&reg_name(v, k - 1))
        }
    }

    /// Rewrite an expression appearing in segment `seg`: a reference to a value
    /// defined in an earlier segment becomes a read of its carried register
    /// (`self.<name>_s{seg-1}`); a same-segment local takes its per-segment name;
    /// parameters used in segment 0 and everything else (consts, fn names) are
    /// left as-is.
    fn rewrite(&self, e: &Expr, seg: usize) -> Expr {
        // Every rewritten expression keeps the original's position.
        let out = match &e.kind {
            ExprKind::Ident(x) => match self.def_group.get(x) {
                Some(&dg) if dg == seg => {
                    if self.is_param.contains(x) {
                        Expr::from(ExprKind::Ident(x.clone()))
                    } else {
                        Expr::from(ExprKind::Ident(local_name(x, seg)))
                    }
                }
                Some(_) => self_field(&reg_name(x, seg - 1)),
                None => Expr::from(ExprKind::Ident(x.clone())),
            },
            ExprKind::Some(inner) => Expr::from(ExprKind::Some(Box::new(self.rewrite(inner, seg)))),
            ExprKind::Try(inner) => Expr::from(ExprKind::Try(Box::new(self.rewrite(inner, seg)))),
            ExprKind::Literal(_)
            | ExprKind::Path(_)
            | ExprKind::IncludeHex(_)
            | ExprKind::ArrayRepeat { .. }
            | ExprKind::None => e.clone(),
            ExprKind::Unary { op, expr } => Expr::from(ExprKind::Unary {
                op: *op,
                expr: Box::new(self.rewrite(expr, seg)),
            }),
            ExprKind::Binary { op, lhs, rhs } => Expr::from(ExprKind::Binary {
                op: *op,
                lhs: Box::new(self.rewrite(lhs, seg)),
                rhs: Box::new(self.rewrite(rhs, seg)),
            }),
            ExprKind::If { cond, then, els } => Expr::from(ExprKind::If {
                cond: Box::new(self.rewrite(cond, seg)),
                then: Box::new(self.rewrite(then, seg)),
                els: Box::new(self.rewrite(els, seg)),
            }),
            ExprKind::Field { base, field } => Expr::from(ExprKind::Field {
                base: Box::new(self.rewrite(base, seg)),
                field: field.clone(),
            }),
            ExprKind::Call { func, args } => Expr::from(ExprKind::Call {
                func: Box::new(self.rewrite(func, seg)),
                args: args.iter().map(|a| self.rewrite(a, seg)).collect(),
            }),
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            } => Expr::from(ExprKind::MethodCall {
                receiver: Box::new(self.rewrite(receiver, seg)),
                method: method.clone(),
                args: args.iter().map(|a| self.rewrite(a, seg)).collect(),
            }),
            ExprKind::Index { base, index } => Expr::from(ExprKind::Index {
                base: Box::new(self.rewrite(base, seg)),
                index: Box::new(self.rewrite(index, seg)),
            }),
            ExprKind::Slice { base, hi, lo } => Expr::from(ExprKind::Slice {
                base: Box::new(self.rewrite(base, seg)),
                hi: Box::new(self.rewrite(hi, seg)),
                lo: Box::new(self.rewrite(lo, seg)),
            }),
            ExprKind::StructLit { name, fields, .. } => Expr::from(ExprKind::StructLit {
                base: None,
                name: name.clone(),
                fields: fields
                    .iter()
                    .map(|fi| FieldInit {
                        name: fi.name.clone(),
                        value: self.rewrite(&fi.value, seg),
                    })
                    .collect(),
            }),
            ExprKind::Tuple(items) => Expr::from(ExprKind::Tuple(
                items.iter().map(|i| self.rewrite(i, seg)).collect(),
            )),
            ExprKind::Array(items) => Expr::from(ExprKind::Array(
                items.iter().map(|i| self.rewrite(i, seg)).collect(),
            )),
            ExprKind::Cast { expr, ty, mode } => Expr::from(ExprKind::Cast {
                expr: Box::new(self.rewrite(expr, seg)),
                ty: ty.clone(),
                mode: *mode,
            }),
            ExprKind::Match { scrutinee, arms } => Expr::from(ExprKind::Match {
                scrutinee: Box::new(self.rewrite(scrutinee, seg)),
                arms: arms
                    .iter()
                    .map(|arm| sinter_syntax::ast::MatchArm {
                        pattern: arm.pattern.clone(),
                        body: self.rewrite(&arm.body, seg),
                    })
                    .collect(),
            }),
            ExprKind::Block(b) => Expr::from(ExprKind::Block(Block {
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
                            value: self.rewrite(value, seg),
                            pos: *pos,
                        },
                        Stmt::Yield { value, pos } => Stmt::Yield {
                            value: value.clone(),
                            pos: *pos,
                        },
                        // Unreachable: asserts in a staged fn are rejected up
                        // front (stage split + `reject_nested_yields`). Pass
                        // through for exhaustiveness.
                        Stmt::Assert { .. } => s.clone(),
                    })
                    .collect(),
                tail: Box::new(self.rewrite(&b.tail, seg)),
                pos: b.pos,
            })),
        };
        Expr::new(out.kind, e.pos)
    }
}

/// The carried-register field name for value `v` at boundary `k`.
fn reg_name(v: &str, k: usize) -> String {
    format!("{v}_s{k}")
}

/// The per-segment local name used inside `advance` for a `let`-local.
pub(crate) fn local_name(v: &str, seg: usize) -> String {
    format!("{v}__p{seg}")
}

/// A `self.<field>` access expression.
pub(crate) fn self_field(field: &str) -> Expr {
    Expr::from(ExprKind::Field {
        base: Box::new(Expr::from(ExprKind::Ident("self".to_string()))),
        field: field.to_string(),
    })
}

/// A struct field of the given type defaulting to `0`.
pub(crate) fn zero_field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        attrs: Vec::new(),
        vis: Visibility::Private,
        name: name.to_string(),
        ty,
        default: Some(Expr::from(ExprKind::Literal(0))),
        pos: Pos::default(),
    }
}

/// Collect the identifier names *read* by an expression (for use analysis). Field
/// names, struct-literal names, and `::` paths are not identifier reads.
pub(crate) fn idents(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_idents(e, &mut out);
    out
}

fn collect_idents(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Ident(x) => out.push(x.clone()),
        ExprKind::Some(inner) => collect_idents(inner, out),
        ExprKind::Try(inner) => collect_idents(inner, out),
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::IncludeHex(_)
        | ExprKind::ArrayRepeat { .. }
        | ExprKind::None => {}
        ExprKind::Unary { expr, .. } => collect_idents(expr, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_idents(lhs, out);
            collect_idents(rhs, out);
        }
        ExprKind::If { cond, then, els } => {
            collect_idents(cond, out);
            collect_idents(then, out);
            collect_idents(els, out);
        }
        ExprKind::Field { base, .. } => collect_idents(base, out),
        ExprKind::Call { func, args } => {
            collect_idents(func, out);
            for a in args {
                collect_idents(a, out);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            collect_idents(receiver, out);
            for a in args {
                collect_idents(a, out);
            }
        }
        ExprKind::Index { base, index } => {
            collect_idents(base, out);
            collect_idents(index, out);
        }
        ExprKind::Slice { base, hi, lo } => {
            collect_idents(base, out);
            collect_idents(hi, out);
            collect_idents(lo, out);
        }
        ExprKind::StructLit { fields, .. } => {
            for fi in fields {
                collect_idents(&fi.value, out);
            }
        }
        ExprKind::Tuple(items) | ExprKind::Array(items) => {
            for i in items {
                collect_idents(i, out);
            }
        }
        ExprKind::Cast { expr, .. } => collect_idents(expr, out),
        ExprKind::Match { scrutinee, arms } => {
            collect_idents(scrutinee, out);
            for arm in arms {
                collect_idents(&arm.body, out);
            }
        }
        ExprKind::Block(b) => {
            for s in &b.stmts {
                if let Stmt::Let { value, .. } = s {
                    collect_idents(value, out);
                }
            }
            collect_idents(&b.tail, out);
        }
    }
}

/// Reject a `yield` nested below the top level of a `pipeline fn` body (PL3a only
/// splits on top-level `yield`s; data-dependent staging is deferred).
pub(crate) fn reject_nested_yields(body: &Block, ctx: &str) -> Result<(), CompileError> {
    for stmt in &body.stmts {
        if let Stmt::Let { value, .. } = stmt {
            reject_yields_in_expr(value, ctx)?;
        }
    }
    reject_yields_in_expr(&body.tail, ctx)
}

fn reject_yields_in_expr(e: &Expr, ctx: &str) -> Result<(), CompileError> {
    match &e.kind {
        ExprKind::Block(b) => {
            for s in &b.stmts {
                match s {
                    Stmt::Yield { pos, .. } => return Err(sem(
                        ctx,
                        *pos,
                        "`yield` may only appear at the top level of a staged function (nested \
                             staging is not supported)",
                    )),
                    Stmt::Let { value, .. } => reject_yields_in_expr(value, ctx)?,
                    // FLT0 does not support `assert!` in a staged fn; this walk
                    // (via `reject_nested_yields`) reaches nested blocks, so it
                    // catches asserts the top-level stage split does not.
                    Stmt::Assert { pos, .. } => return Err(sem(
                        ctx,
                        *pos,
                        "`assert!` is not yet supported inside a staged (`pipeline`/`multiplex`) \
                         fn (FLT0)",
                    )),
                }
            }
            reject_yields_in_expr(&b.tail, ctx)
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => {
            reject_yields_in_expr(expr, ctx)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            reject_yields_in_expr(lhs, ctx)?;
            reject_yields_in_expr(rhs, ctx)
        }
        ExprKind::If { cond, then, els } => {
            reject_yields_in_expr(cond, ctx)?;
            reject_yields_in_expr(then, ctx)?;
            reject_yields_in_expr(els, ctx)
        }
        ExprKind::Field { base, .. } => reject_yields_in_expr(base, ctx),
        ExprKind::Call { func, args } => {
            reject_yields_in_expr(func, ctx)?;
            args.iter().try_for_each(|a| reject_yields_in_expr(a, ctx))
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            reject_yields_in_expr(receiver, ctx)?;
            args.iter().try_for_each(|a| reject_yields_in_expr(a, ctx))
        }
        ExprKind::Index { base, index } => {
            reject_yields_in_expr(base, ctx)?;
            reject_yields_in_expr(index, ctx)
        }
        ExprKind::Slice { base, hi, lo } => {
            reject_yields_in_expr(base, ctx)?;
            reject_yields_in_expr(hi, ctx)?;
            reject_yields_in_expr(lo, ctx)
        }
        ExprKind::StructLit { fields, .. } => fields
            .iter()
            .try_for_each(|fi| reject_yields_in_expr(&fi.value, ctx)),
        ExprKind::Tuple(items) | ExprKind::Array(items) => {
            items.iter().try_for_each(|i| reject_yields_in_expr(i, ctx))
        }
        ExprKind::Match { scrutinee, arms } => {
            reject_yields_in_expr(scrutinee, ctx)?;
            arms.iter()
                .try_for_each(|arm| reject_yields_in_expr(&arm.body, ctx))
        }
        ExprKind::Some(inner) => reject_yields_in_expr(inner, ctx),
        ExprKind::Try(inner) => reject_yields_in_expr(inner, ctx),
        ExprKind::Literal(_)
        | ExprKind::Ident(_)
        | ExprKind::Path(_)
        | ExprKind::IncludeHex(_)
        | ExprKind::ArrayRepeat { .. }
        | ExprKind::None => Ok(()),
    }
}

/// Reject a nested binder (block-local `let`, `match`-arm pattern) whose name
/// shadows a pipeline value. The reference rewrite resolves identifiers by name
/// against the pipeline's values, so a shadow would be silently miscompiled into a
/// stage-register read; PL3a forbids it (full lexical scoping is a later option).
pub(crate) fn reject_shadowing(
    e: &Expr,
    defs: &HashMap<String, usize>,
    ctx: &str,
) -> Result<(), CompileError> {
    match &e.kind {
        ExprKind::Block(b) => {
            for s in &b.stmts {
                match s {
                    Stmt::Let {
                        pattern,
                        value,
                        pos,
                        ..
                    } => {
                        check_pattern_shadow(pattern, defs, ctx, *pos)?;
                        reject_shadowing(value, defs, ctx)?;
                    }
                    // Nested `yield`s are already rejected by `reject_nested_yields`.
                    Stmt::Yield { .. } => {}
                    // Asserts in a staged fn are likewise already rejected; nothing
                    // to shadow-check here.
                    Stmt::Assert { .. } => {}
                }
            }
            reject_shadowing(&b.tail, defs, ctx)
        }
        ExprKind::Match { scrutinee, arms } => {
            reject_shadowing(scrutinee, defs, ctx)?;
            for arm in arms {
                check_pattern_shadow(&arm.pattern, defs, ctx, Pos::default())?;
                reject_shadowing(&arm.body, defs, ctx)?;
            }
            Ok(())
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => {
            reject_shadowing(expr, defs, ctx)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            reject_shadowing(lhs, defs, ctx)?;
            reject_shadowing(rhs, defs, ctx)
        }
        ExprKind::If { cond, then, els } => {
            reject_shadowing(cond, defs, ctx)?;
            reject_shadowing(then, defs, ctx)?;
            reject_shadowing(els, defs, ctx)
        }
        ExprKind::Field { base, .. } => reject_shadowing(base, defs, ctx),
        ExprKind::Call { func, args } => {
            reject_shadowing(func, defs, ctx)?;
            args.iter().try_for_each(|a| reject_shadowing(a, defs, ctx))
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            reject_shadowing(receiver, defs, ctx)?;
            args.iter().try_for_each(|a| reject_shadowing(a, defs, ctx))
        }
        ExprKind::Index { base, index } => {
            reject_shadowing(base, defs, ctx)?;
            reject_shadowing(index, defs, ctx)
        }
        ExprKind::Slice { base, hi, lo } => {
            reject_shadowing(base, defs, ctx)?;
            reject_shadowing(hi, defs, ctx)?;
            reject_shadowing(lo, defs, ctx)
        }
        ExprKind::StructLit { fields, .. } => fields
            .iter()
            .try_for_each(|fi| reject_shadowing(&fi.value, defs, ctx)),
        ExprKind::Tuple(items) | ExprKind::Array(items) => items
            .iter()
            .try_for_each(|i| reject_shadowing(i, defs, ctx)),
        ExprKind::Some(inner) => reject_shadowing(inner, defs, ctx),
        ExprKind::Try(inner) => reject_shadowing(inner, defs, ctx),
        ExprKind::Literal(_)
        | ExprKind::Ident(_)
        | ExprKind::Path(_)
        | ExprKind::IncludeHex(_)
        | ExprKind::ArrayRepeat { .. }
        | ExprKind::None => Ok(()),
    }
}

/// Reject a pattern that binds a name shadowing a pipeline value.
fn check_pattern_shadow(
    pat: &Pattern,
    defs: &HashMap<String, usize>,
    ctx: &str,
    pos: Pos,
) -> Result<(), CompileError> {
    let mut names = Vec::new();
    pattern_binds(pat, &mut names);
    for n in names {
        if n == IN_VALID {
            return Err(sem(
                ctx,
                pos,
                format!("`{IN_VALID}` is a reserved pipeline name"),
            ));
        }
        if defs.contains_key(&n) {
            return Err(sem(
                ctx,
                pos,
                format!(
                    "a staged function cannot shadow `{n}` with a nested `let` / `match` binding \
                     across `yield` boundaries (not supported)"
                ),
            ));
        }
    }
    Ok(())
}

/// Collect the names a pattern binds (a binding, or the components of a tuple).
fn pattern_binds(pat: &Pattern, out: &mut Vec<String>) {
    match pat {
        Pattern::Binding(n) => out.push(n.clone()),
        Pattern::Tuple(ps) => ps.iter().for_each(|p| pattern_binds(p, out)),
        Pattern::Some(inner) => pattern_binds(inner, out),
        Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path(_) | Pattern::None => {}
    }
}

/// Rewrite a pipeline instance field type `inst: f` to `inst: f__pipe`. A pipeline
/// name nested inside an array/tuple field is rejected (PL3a holds a pipeline only
/// in a plain field).
pub(crate) fn rewrite_field_ty(
    ty: &mut TypeRef,
    pipe_names: &HashSet<String>,
    ctx: &str,
    pos: Pos,
    top_level: bool,
) -> Result<(), CompileError> {
    match ty {
        TypeRef::Named(n) if pipe_names.contains(n) => {
            if top_level {
                *n = format!("{n}{PIPE_SUFFIX}");
                Ok(())
            } else {
                Err(sem(
                    ctx,
                    pos,
                    format!(
                        "a pipeline instance (`{n}`) must be held in a plain field in PL3a, not \
                         nested in an array or tuple"
                    ),
                ))
            }
        }
        TypeRef::Array { elem, .. } => rewrite_field_ty(elem, pipe_names, ctx, pos, false),
        TypeRef::Tuple(ts) => ts
            .iter_mut()
            .try_for_each(|t| rewrite_field_ty(t, pipe_names, ctx, pos, false)),
        _ => Ok(()),
    }
}

pub(crate) fn sem(ctx: &str, pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, pos, msg)
}
