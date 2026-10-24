//! Channel desugar — the `fifo` / `ring` / `flow` family (DESIGN §6.11 / §9.8,
//! CHAN1–CHAN2).
//!
//! Rewrites each channel-typed struct field into a synthesized `struct` + `impl`
//! *before* type-checking, so the rest of the pipeline sees only ordinary
//! functional-surface state — no new IR node and no backend change (the same
//! source-to-source strategy as `pipeline_desugar` / `multiplex_desugar`). The
//! synthesized items are produced by generating Sinter source and re-parsing it,
//! so the generated queue logic is legible and reuses the existing
//! parser/checker/elaborator unchanged.
//!
//! **Shift-register model (why there is no runtime array index).** The head is
//! always slot 0 (`front()` reads `data[0]`), and each slot's next value compares
//! the *runtime* `count` against a *constant* slot index — so every `data[k]`
//! access uses a literal `k`, and the desugar needs none of the runtime array
//! indexing that DESIGN §9.11 (REG1) added. Good for shallow depth.
//!
//! **Loss policy is the type (DESIGN §6.11)**, and for the queue kinds it reduces
//! to two flags over one shared template: `pushf` (does a word land this cycle?)
//! and `shiftf` (does the queue shift down, retiring slot 0?).
//!
//! - `fifo<T, N>` — **lossless**. `pushf = hp & (space | popf)`: a push into a
//!   full queue with no simultaneous pop is refused (a guarded hold — live data is
//!   never overwritten). `shiftf = popf`: only a real pop retires the head.
//! - `ring<T, N>` — **overwrite-oldest**. `pushf = hp`: a push always lands.
//!   `shiftf = popf | (hp & full)`: pushing into a full ring retires the oldest
//!   word to make room, so `count` saturates at `N` rather than refusing the word.
//! - `flow<T>` — **depth-1 newest-wins**. No queue at all: an `Option<T>` register
//!   that takes the pushed word and otherwise holds, read with `value()`. There is
//!   no pop, so it erases exactly like `Option`.
//!
//! Common to all: a `pop` when empty is inert, a same-cycle push+pop streams
//! through (READ_FIRST), and everything resets empty (all-zero / `None`), so the
//! oracle sees no `X`.
//!
//! **Views and the push token (CHAN3).** A `fifo<T, N>` also gets
//! `sender()` / `receiver()`, returning `Sender<T, N>` / `Receiver<T, N>`
//! snapshot structs, and its `next` takes its push as a `Push<T>`, a struct
//! that only a `Sender`'s `try_push` / `offer` / `no_push` construct, each
//! refusing the word when the queue is full. So a lossless queue can't be
//! overfilled. These types may appear anywhere a type can (signatures, `let`
//! annotations), so every type in the program is rewritten, not only fields.

use std::collections::BTreeMap;

use sinter_syntax::ast::{
    Block, ChannelKind, Expr, Item, MatchArm, Param, Pos, Program, Stmt, Type, TypeRef,
};
use sinter_syntax::parse_program;

use crate::error::CompileError;

const CTX: &str = "channel";

fn sem(pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(CTX, pos, msg)
}

/// How a channel kind spells itself in source (for diagnostics + type names).
fn kind_src(kind: ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Fifo => "fifo",
        ChannelKind::Ring => "ring",
        ChannelKind::Flow => "flow",
        ChannelKind::Sender => "Sender",
        ChannelKind::Receiver => "Receiver",
        ChannelKind::Push => "Push",
    }
}

/// Whether a kind is channel *state* (a queue held in a field) rather than a
/// view or push token, which are plain values.
fn is_state(kind: ChannelKind) -> bool {
    matches!(
        kind,
        ChannelKind::Fifo | ChannelKind::Ring | ChannelKind::Flow
    )
}

/// One channel monomorphization → a synthesized `struct` + `impl`.
struct Mono {
    /// Which member of the family (and so which lowering).
    kind: ChannelKind,
    /// Synthesized struct name (e.g. `Fifo_u8_16`, `Ring_u8_4`, `Flow_u8`).
    name: String,
    /// Element type rendered as source (e.g. `u8`, `i16`, `u4.4`).
    elem_src: String,
    /// Capacity (a power of two; always 1 for `flow`).
    n: usize,
    /// Bit width of the occupancy counter — enough to hold `0..=N`.
    cbits: u32,
}

/// Whether `program` mentions a channel type anywhere (a field, a signature,
/// or a `let` annotation).
pub fn has_channel(program: &Program) -> bool {
    let mut found = false;
    let mut probe = program.clone();
    // The visitor only reads; `Ok` keeps walking.
    let _ = for_each_type(&mut probe, &mut |t| {
        found |= type_has_channel(t);
        Ok(())
    });
    found
}

fn type_has_channel(t: &TypeRef) -> bool {
    match t {
        TypeRef::Channel { .. } => true,
        TypeRef::Array { elem, .. } => type_has_channel(elem),
        TypeRef::Tuple(ts) => ts.iter().any(type_has_channel),
        TypeRef::Option(t) | TypeRef::Mem { elem: t, .. } => type_has_channel(t),
        _ => false,
    }
}

/// The source spelling of each synthesized channel struct name, e.g.
/// `Push_u8` → `Push<u8>`, so diagnostics can show what the user wrote.
pub fn source_names(program: &Program) -> Vec<(String, String)> {
    let mut monos: BTreeMap<String, Mono> = BTreeMap::new();
    let mut probe = program.clone();
    let _ = for_each_type(&mut probe, &mut |t| rewrite_type(t, &mut monos));
    let fifos: Vec<(String, usize)> = monos
        .values()
        .filter(|m| {
            matches!(
                m.kind,
                ChannelKind::Fifo | ChannelKind::Sender | ChannelKind::Receiver
            )
        })
        .map(|m| (m.elem_src.clone(), m.n))
        .collect();
    for (elem_src, n) in fifos {
        for kind in [
            ChannelKind::Sender,
            ChannelKind::Receiver,
            ChannelKind::Push,
        ] {
            let m = mono_for(kind, elem_src.clone(), n);
            monos.entry(m.name.clone()).or_insert(m);
        }
    }
    monos
        .into_values()
        .map(|m| {
            let ks = kind_src(m.kind);
            let src = match m.kind {
                ChannelKind::Flow | ChannelKind::Push => format!("{ks}<{}>", m.elem_src),
                _ => format!("{ks}<{}, {}>", m.elem_src, m.n),
            };
            (m.name, src)
        })
        .collect()
}

/// Lower every channel type to a synthesized `struct` + `impl`.
pub fn desugar_channels(program: &Program) -> Result<Program, CompileError> {
    // 1. Validate channel fields: queue state takes no initializer and must be
    //    held in a plain field.
    for it in &program.items {
        let Item::Struct(s) = it else { continue };
        for f in &s.fields {
            match &f.ty {
                TypeRef::Channel { kind, .. } if is_state(*kind) && f.default.is_some() => {
                    return Err(sem(
                        f.pos,
                        format!(
                            "a `{}` field (`{}`) resets empty and takes no initializer",
                            kind_src(*kind),
                            f.name
                        ),
                    ));
                }
                TypeRef::Channel { .. } => {}
                other => reject_nested_channel(other, f.pos)?,
            }
        }
    }

    // 2. Rewrite every channel type, wherever it appears, to its synthesized
    //    struct name, collecting the distinct monomorphizations. A `fifo`
    //    brings its views and push token with it.
    let mut out = program.clone();
    let mut monos: BTreeMap<String, Mono> = BTreeMap::new();
    for_each_type(&mut out, &mut |t| rewrite_type(t, &mut monos))?;
    if monos.is_empty() {
        return Ok(program.clone());
    }
    let fifos: Vec<(String, usize)> = monos
        .values()
        .filter(|m| {
            matches!(
                m.kind,
                ChannelKind::Fifo | ChannelKind::Sender | ChannelKind::Receiver
            )
        })
        .map(|m| (m.elem_src.clone(), m.n))
        .collect();
    for (elem_src, n) in fifos {
        for kind in [
            ChannelKind::Sender,
            ChannelKind::Receiver,
            ChannelKind::Push,
        ] {
            let m = mono_for(kind, elem_src.clone(), n);
            monos.entry(m.name.clone()).or_insert(m);
        }
    }

    // 3. Generate + parse the synthesized items and append them.
    for m in monos.values() {
        let src = gen_source(m);
        let parsed = parse_program(&src).map_err(|e| {
            sem(
                Pos::default(),
                format!(
                    "internal error: synthesized `{}` source failed to parse: {e}\n{src}",
                    kind_src(m.kind)
                ),
            )
        })?;
        out.items.extend(parsed.items);
    }
    Ok(out)
}

/// Replace `t` (and any channel type inside it) with its synthesized struct
/// name, recording the monomorphization.
fn rewrite_type(t: &mut TypeRef, monos: &mut BTreeMap<String, Mono>) -> Result<(), CompileError> {
    match t {
        TypeRef::Channel { kind, elem, depth } => {
            let m = build_mono(*kind, elem, depth, Pos::default())?;
            *t = TypeRef::Named(m.name.clone());
            monos.entry(m.name.clone()).or_insert(m);
            Ok(())
        }
        TypeRef::Array { elem, .. } => rewrite_type(elem, monos),
        TypeRef::Tuple(ts) => ts.iter_mut().try_for_each(|t| rewrite_type(t, monos)),
        TypeRef::Option(inner) | TypeRef::Mem { elem: inner, .. } => rewrite_type(inner, monos),
        _ => Ok(()),
    }
}

/// Call `f` on every type the program names: struct fields, `const`s,
/// function and method signatures, `let` annotations, and casts.
fn for_each_type(
    program: &mut Program,
    f: &mut dyn FnMut(&mut TypeRef) -> Result<(), CompileError>,
) -> Result<(), CompileError> {
    fn func(
        fun: &mut sinter_syntax::ast::Func,
        f: &mut dyn FnMut(&mut TypeRef) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        for p in &mut fun.params {
            if let Param::Typed { ty, .. } = p {
                f(ty)?;
            }
        }
        f(&mut fun.ret)?;
        block(&mut fun.body, f)
    }
    fn block(
        b: &mut Block,
        f: &mut dyn FnMut(&mut TypeRef) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        for stmt in &mut b.stmts {
            match stmt {
                Stmt::Let { ty, value, .. } => {
                    if let Some(t) = ty {
                        f(t)?;
                    }
                    expr(value, f)?;
                }
                Stmt::Yield { value, .. } => {
                    if let Some(v) = value {
                        expr(v, f)?;
                    }
                }
                Stmt::Assert { cond, .. } => expr(cond, f)?,
            }
        }
        expr(&mut b.tail, f)
    }
    fn expr(
        e: &mut Expr,
        f: &mut dyn FnMut(&mut TypeRef) -> Result<(), CompileError>,
    ) -> Result<(), CompileError> {
        match e {
            Expr::Cast { expr: x, ty, .. } => {
                f(ty)?;
                expr(x, f)
            }
            Expr::Unary { expr: x, .. }
            | Expr::Some(x)
            | Expr::Try(x)
            | Expr::Field { base: x, .. } => expr(x, f),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Index {
                base: lhs,
                index: rhs,
            } => {
                expr(lhs, f)?;
                expr(rhs, f)
            }
            Expr::If { cond, then, els } => {
                expr(cond, f)?;
                expr(then, f)?;
                expr(els, f)
            }
            Expr::Slice { base, hi, lo } => {
                expr(base, f)?;
                expr(hi, f)?;
                expr(lo, f)
            }
            Expr::Call { func: callee, args } => {
                expr(callee, f)?;
                args.iter_mut().try_for_each(|a| expr(a, f))
            }
            Expr::MethodCall { receiver, args, .. } => {
                expr(receiver, f)?;
                args.iter_mut().try_for_each(|a| expr(a, f))
            }
            Expr::StructLit { fields, .. } => {
                fields.iter_mut().try_for_each(|fi| expr(&mut fi.value, f))
            }
            Expr::Tuple(items) | Expr::Array(items) => {
                items.iter_mut().try_for_each(|i| expr(i, f))
            }
            Expr::ArrayRepeat { value, len } => {
                expr(value, f)?;
                expr(len, f)
            }
            Expr::Match { scrutinee, arms } => {
                expr(scrutinee, f)?;
                arms.iter_mut()
                    .try_for_each(|MatchArm { body, .. }| expr(body, f))
            }
            Expr::Block(b) => block(b, f),
            Expr::None
            | Expr::Literal(_)
            | Expr::Ident(_)
            | Expr::Path(_)
            | Expr::IncludeHex(_) => Ok(()),
        }
    }
    for item in &mut program.items {
        match item {
            Item::Struct(s) => {
                for fld in &mut s.fields {
                    f(&mut fld.ty)?;
                }
            }
            Item::Const(c) => f(&mut c.ty)?,
            Item::Func(fun) => func(fun, f)?,
            Item::Impl(im) => {
                for m in &mut im.methods {
                    func(m, f)?;
                }
            }
            Item::Enum(_) | Item::Mod(_) | Item::Use(_) => {}
        }
    }
    Ok(())
}

fn build_mono(
    kind: ChannelKind,
    elem: &TypeRef,
    depth: &Expr,
    pos: Pos,
) -> Result<Mono, CompileError> {
    let elem_src = scalar_src(kind, elem, pos)?;
    let ks = kind_src(kind);
    let n = match depth {
        Expr::Literal(v) => usize::try_from(*v)
            .map_err(|_| sem(pos, format!("`{ks}` capacity `{v}` is too large")))?,
        _ => return Err(sem(pos, format!("a `{ks}` capacity must be a literal"))),
    };
    // A `flow<T>` / `Push<T>` is depth-1 by construction (the parser synthesizes
    // the `1`), so the power-of-two capacity rule applies only to the others.
    if !matches!(kind, ChannelKind::Flow | ChannelKind::Push) && (n < 2 || !n.is_power_of_two()) {
        return Err(sem(
            pos,
            format!("a `{ks}` capacity (`{n}`) must be a power of two ≥ 2"),
        ));
    }
    Ok(mono_for(kind, elem_src, n))
}

/// The monomorphization of `kind` over element `elem_src` and capacity `n`.
fn mono_for(kind: ChannelKind, elem_src: String, n: usize) -> Mono {
    // Bits to hold the occupancy `0..=N` (e.g. N=16 -> 0..=16 needs 5 bits).
    let cbits = u32::BITS - (n as u32).leading_zeros();
    let e = mangle(&elem_src);
    let name = match kind {
        ChannelKind::Fifo => format!("Fifo_{e}_{n}"),
        ChannelKind::Ring => format!("Ring_{e}_{n}"),
        ChannelKind::Flow => format!("Flow_{e}"),
        ChannelKind::Sender => format!("Sender_{e}_{n}"),
        ChannelKind::Receiver => format!("Receiver_{e}_{n}"),
        ChannelKind::Push => format!("Push_{e}"),
    };
    Mono {
        kind,
        name,
        elem_src,
        n,
        cbits,
    }
}

/// Render a scalar element type as Sinter source (a channel holds scalars only).
fn scalar_src(kind: ChannelKind, t: &TypeRef, pos: Pos) -> Result<String, CompileError> {
    match t {
        TypeRef::Scalar(Type::Uint(w)) => Ok(format!("u{w}")),
        TypeRef::Scalar(Type::Int(w)) => Ok(format!("i{w}")),
        TypeRef::Fixed {
            signed,
            int_bits,
            frac_bits,
        } => Ok(format!(
            "{}{int_bits}.{frac_bits}",
            if *signed { "i" } else { "u" }
        )),
        _ => Err(sem(
            pos,
            format!(
                "a `{}` element must be a scalar (`uN`/`iN`/fixed-point)",
                kind_src(kind)
            ),
        )),
    }
}

/// Make a type-source string safe inside an identifier (`u4.4` -> `u4p4`).
fn mangle(elem_src: &str) -> String {
    elem_src.replace('.', "p")
}

/// Reject a channel nested inside an array/tuple/`Option`/`mem` field (a channel
/// is held only in a plain field, mirroring the `mem`/pipeline placement rule).
fn reject_nested_channel(t: &TypeRef, pos: Pos) -> Result<(), CompileError> {
    match t {
        TypeRef::Channel { kind, .. } => Err(sem(
            pos,
            format!(
                "a `{}` must be held in a plain struct field (not nested in an array/tuple/Option)",
                kind_src(*kind)
            ),
        )),
        TypeRef::Array { elem, .. } => reject_nested_channel(elem, pos),
        TypeRef::Tuple(ts) => ts.iter().try_for_each(|t| reject_nested_channel(t, pos)),
        TypeRef::Option(t) | TypeRef::Mem { elem: t, .. } => reject_nested_channel(t, pos),
        _ => Ok(()),
    }
}

/// A queue kind's loss policy, as the source expressions that differ between a
/// `fifo` and a `ring` (DESIGN §6.11). Everything else about the two is shared.
struct QueuePolicy {
    /// A `let` declaring the occupancy flag the expressions below read.
    occupancy_flag: String,
    /// Does an offered word land this cycle?
    pushf: &'static str,
    /// Does the queue shift down, retiring slot 0?
    shiftf: &'static str,
}

/// Generate the `struct` + `impl` source for one monomorphization — the sole
/// place the family's members diverge.
fn gen_source(m: &Mono) -> String {
    let n = m.n;
    match m.kind {
        // Lossless: refuse a word when there is no room (a guarded hold — live
        // data is never overwritten), and retire the head only on a real pop.
        ChannelKind::Fifo => gen_queue_source(
            m,
            &QueuePolicy {
                occupancy_flag: format!(
                    "let space: u1 = if self.count == {n} {{ 0 }} else {{ 1 }};"
                ),
                pushf: "hp & (space | popf)",
                shiftf: "popf",
            },
        ),
        // Overwrite-oldest: always accept, retiring the oldest word to make room
        // when the ring is full — so `count` saturates at `N` instead of refusing.
        ChannelKind::Ring => gen_queue_source(
            m,
            &QueuePolicy {
                occupancy_flag: format!(
                    "let full: u1 = if self.count == {n} {{ 1 }} else {{ 0 }};"
                ),
                pushf: "hp",
                shiftf: "popf | (hp & full)",
            },
        ),
        ChannelKind::Flow => gen_flow_source(m),
        ChannelKind::Sender | ChannelKind::Receiver | ChannelKind::Push => gen_view_source(m),
    }
}

/// Generate a `fifo`'s producer / consumer view or its push token (CHAN3).
fn gen_view_source(m: &Mono) -> String {
    let Mono {
        kind,
        name,
        elem_src: ts,
        n,
        cbits,
    } = m;
    let push = format!("Push_{}", mangle(ts));
    match kind {
        ChannelKind::Sender => format!(
            "\
struct {name} {{
    full: u1 = 0,
    count: u{cbits} = 0,
}}

impl {name} {{
    fn is_full(self) -> u1 {{
        self.full
    }}
    fn len(self) -> u{cbits} {{
        self.count
    }}
    // Push `v` if the queue has room (a full `fifo<_, {n}>` refuses it).
    fn try_push(self, v: {ts}) -> {push} {{
        {push} {{ v: if self.full == 1 {{ None }} else {{ Some(v) }} }}
    }}
    // Push the word in `o`, if any, when the queue has room.
    fn offer(self, o: Option<{ts}>) -> {push} {{
        {push} {{ v: if self.full == 1 {{ None }} else {{ o }} }}
    }}
    fn no_push(self) -> {push} {{
        {push} {{ v: None }}
    }}
}}
"
        ),
        ChannelKind::Receiver => format!(
            "\
struct {name} {{
    head: Option<{ts}>,
    count: u{cbits} = 0,
}}

impl {name} {{
    fn front(self) -> Option<{ts}> {{
        self.head
    }}
    fn is_empty(self) -> u1 {{
        if self.count == 0 {{ 1 }} else {{ 0 }}
    }}
    fn len(self) -> u{cbits} {{
        self.count
    }}
}}
"
        ),
        _ => format!(
            "\
struct {name} {{
    v: Option<{ts}>,
}}
"
        ),
    }
}

/// Generate a `fifo` / `ring` — one shift-register queue whose loss policy is
/// carried entirely by `policy`.
fn gen_queue_source(m: &Mono, policy: &QueuePolicy) -> String {
    let Mono {
        name,
        elem_src: ts,
        n,
        cbits,
        ..
    } = m;
    let n = *n;
    let zeros = vec!["0"; n].join(", ");
    let QueuePolicy {
        occupancy_flag,
        pushf,
        shiftf,
    } = policy;

    // Per-slot next-state (constant slot index `k`, runtime `count` comparisons):
    //   write target: slot `widx` (= count - shiftf) gets the pushed word;
    //   on a shift everything moves down one slot; otherwise each slot holds.
    let mut slots = Vec::with_capacity(n);
    for k in 0..n {
        let shifted = if k + 1 < n {
            format!("self.data[{}]", k + 1)
        } else {
            "0".to_string()
        };
        slots.push(format!(
            "if pushf & (widx == {k}) {{ pv }} else if shiftf == 1 {{ {shifted} }} \
             else {{ self.data[{k}] }}"
        ));
    }
    let data_next = slots.join(",\n            ");

    // A lossless `fifo` takes its push as a `Push<T>` token, which only its
    // `Sender` view makes (CHAN3); a lossy `ring` takes a plain `Option<T>`.
    let e = mangle(ts);
    let (push_param, push_unwrap, views) = if m.kind == ChannelKind::Fifo {
        (
            format!("push_tok: Push_{e}"),
            format!("let push: Option<{ts}> = push_tok.v;"),
            format!(
                "\
    fn sender(self) -> Sender_{e}_{n} {{
        Sender_{e}_{n} {{ full: self.is_full(), count: self.count }}
    }}
    fn receiver(self) -> Receiver_{e}_{n} {{
        Receiver_{e}_{n} {{ head: self.front(), count: self.count }}
    }}
"
            ),
        )
    } else {
        (format!("push: Option<{ts}>"), String::new(), String::new())
    };

    format!(
        "\
struct {name} {{
    data: [{ts}; {n}] = [{zeros}],
    count: u{cbits} = 0,
}}

impl {name} {{
    fn front(self) -> Option<{ts}> {{
        if self.count == 0 {{ None }} else {{ Some(self.data[0]) }}
    }}
    fn is_empty(self) -> u1 {{
        if self.count == 0 {{ 1 }} else {{ 0 }}
    }}
    fn is_full(self) -> u1 {{
        if self.count == {n} {{ 1 }} else {{ 0 }}
    }}
    fn len(self) -> u{cbits} {{
        self.count
    }}
{views}    fn next(self, {push_param}, pop: u1) -> {name} {{
        {push_unwrap}
        let popf: u1 = if self.count == 0 {{ 0 }} else {{ pop }};
        let hp: u1 = match push {{ Some(_) => 1, None => 0 }};
        {occupancy_flag}
        let pushf: u1 = {pushf};
        let shiftf: u1 = {shiftf};
        let pv: {ts} = match push {{ Some(v) => v, None => 0 }};
        // Write index and next count are computed with conditionals (not
        // arithmetic on the `u1` flags), so no boolean/ternary expression is ever
        // widened into an arithmetic context (which trips Verilator WIDTHEXPAND).
        let widx: u{cbits} = if shiftf == 1 {{ (self.count - 1) as u{cbits} wrap }} else {{ self.count }};
        let count2: u{cbits} = if pushf == 1 {{
            if shiftf == 1 {{ self.count }} else {{ (self.count + 1) as u{cbits} wrap }}
        }} else {{
            if shiftf == 1 {{ (self.count - 1) as u{cbits} wrap }} else {{ self.count }}
        }};
        {name} {{
            data: [{data_next}],
            count: count2,
        }}
    }}
}}
"
    )
}

/// Generate a `flow<T>` — a depth-1 newest-wins slot. No count and no pop: an
/// `Option<T>` register that takes the pushed word and otherwise holds, so it
/// erases exactly like `Option` (DESIGN §6.11).
fn gen_flow_source(m: &Mono) -> String {
    let Mono {
        name, elem_src: ts, ..
    } = m;
    format!(
        "\
struct {name} {{
    // No initializer: an `Option`-typed register resets to `None` (empty).
    v: Option<{ts}>,
}}

impl {name} {{
    fn value(self) -> Option<{ts}> {{
        self.v
    }}
    fn next(self, push: Option<{ts}>) -> {name} {{
        let hp: u1 = match push {{ Some(_) => 1, None => 0 }};
        {name} {{
            v: if hp == 1 {{ push }} else {{ self.v }},
        }}
    }}
}}
"
    )
}
