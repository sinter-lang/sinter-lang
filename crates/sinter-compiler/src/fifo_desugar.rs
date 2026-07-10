//! CHAN1 `fifo<T, N>` desugar (DESIGN §6.11 / §9.8).
//!
//! Rewrites each `fifo<T, N>` struct field into a synthesized `struct` + `impl`
//! (a shift-register / fall-through queue) *before* type-checking, so the rest of
//! the pipeline sees only ordinary functional-surface state — no new IR node and
//! no backend change (the same source-to-source strategy as `pipeline_desugar` /
//! `multiplex_desugar`). The synthesized items are produced by generating Sinter
//! source and re-parsing it, so the generated queue logic is legible and reuses
//! the existing parser/checker/elaborator unchanged.
//!
//! **Shift-register model (why there is no runtime array index).** The head is
//! always slot 0 (`front()` reads `data[0]`), and each slot's next value compares
//! the *runtime* `count` against a *constant* slot index — so every `data[k]`
//! access uses a literal `k`, and the desugar needs none of the runtime array
//! indexing that DESIGN §9.11 (REG1) will add. Good for shallow depth.
//!
//! **Lossless semantics (DESIGN §6.11).** `pop` when empty is inert; `push` into
//! a full queue with no simultaneous pop is a guarded hold (the word is refused,
//! never overwriting live data). A same-cycle push+pop streams through
//! (READ_FIRST). Everything resets empty (all-zero), so the oracle sees no `X`.

use std::collections::BTreeMap;

use sinter_syntax::ast::{Expr, Item, Pos, Program, Type, TypeRef};
use sinter_syntax::parse_program;

use crate::error::CompileError;

const CTX: &str = "fifo";

fn sem(pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(CTX, pos, msg)
}

/// One `fifo<T, N>` monomorphization → a synthesized `struct` + `impl`.
struct Mono {
    /// Synthesized struct name (e.g. `Fifo_u8_16`).
    name: String,
    /// Element type rendered as source (e.g. `u8`, `i16`, `u4.4`).
    elem_src: String,
    /// Capacity (a power of two).
    n: usize,
    /// Bit width of the occupancy counter — enough to hold `0..=N`.
    cbits: u32,
}

/// Whether any struct field in `program` is (or contains) a `fifo<T, N>`.
pub fn has_fifo(program: &Program) -> bool {
    program
        .items
        .iter()
        .any(|it| matches!(it, Item::Struct(s) if s.fields.iter().any(|f| type_has_fifo(&f.ty))))
}

fn type_has_fifo(t: &TypeRef) -> bool {
    match t {
        TypeRef::Fifo { .. } => true,
        TypeRef::Array { elem, .. } => type_has_fifo(elem),
        TypeRef::Tuple(ts) => ts.iter().any(type_has_fifo),
        TypeRef::Option(t) | TypeRef::Mem { elem: t, .. } => type_has_fifo(t),
        _ => false,
    }
}

/// Lower every `fifo<T, N>` field to a synthesized `struct` + `impl`.
pub fn desugar_fifos(program: &Program) -> Result<Program, CompileError> {
    // 1. Collect the distinct monomorphizations from struct fields, validating
    //    the element type and capacity as we go.
    let mut monos: BTreeMap<String, Mono> = BTreeMap::new();
    for it in &program.items {
        let Item::Struct(s) = it else { continue };
        for f in &s.fields {
            match &f.ty {
                TypeRef::Fifo { elem, depth } => {
                    if f.default.is_some() {
                        return Err(sem(
                            f.pos,
                            format!(
                                "a `fifo` field (`{}`) resets empty and takes no initializer in \
                                 CHAN1",
                                f.name
                            ),
                        ));
                    }
                    let m = build_mono(elem, depth, f.pos)?;
                    monos.entry(m.name.clone()).or_insert(m);
                }
                other => reject_nested_fifo(other, f.pos)?,
            }
        }
    }
    if monos.is_empty() {
        return Ok(program.clone());
    }

    // 2. Rewrite each `fifo<T, N>` field type to its synthesized struct name.
    let mut out = program.clone();
    for it in &mut out.items {
        let Item::Struct(s) = it else { continue };
        for f in &mut s.fields {
            if let TypeRef::Fifo { elem, depth } = &f.ty {
                let name = build_mono(elem, depth, f.pos)?.name;
                f.ty = TypeRef::Named(name);
            }
        }
    }

    // 3. Generate + parse the synthesized items and append them.
    for m in monos.values() {
        let src = gen_source(m);
        let parsed = parse_program(&src).map_err(|e| {
            sem(
                Pos { line: 0, col: 0 },
                format!("internal error: synthesized `fifo` source failed to parse: {e}\n{src}"),
            )
        })?;
        out.items.extend(parsed.items);
    }
    Ok(out)
}

fn build_mono(elem: &TypeRef, depth: &Expr, pos: Pos) -> Result<Mono, CompileError> {
    let elem_src = scalar_src(elem, pos)?;
    let n = match depth {
        Expr::Literal(v) => usize::try_from(*v)
            .map_err(|_| sem(pos, format!("`fifo` capacity `{v}` is too large")))?,
        _ => return Err(sem(pos, "a `fifo` capacity must be a literal in CHAN1")),
    };
    if n < 2 || !n.is_power_of_two() {
        return Err(sem(
            pos,
            format!("a `fifo` capacity (`{n}`) must be a power of two ≥ 2"),
        ));
    }
    // Bits to hold the occupancy `0..=N` (e.g. N=16 -> 0..=16 needs 5 bits).
    let cbits = u32::BITS - (n as u32).leading_zeros();
    let name = format!("Fifo_{}_{}", mangle(&elem_src), n);
    Ok(Mono {
        name,
        elem_src,
        n,
        cbits,
    })
}

/// Render a scalar element type as Sinter source (CHAN1 supports scalars only).
fn scalar_src(t: &TypeRef, pos: Pos) -> Result<String, CompileError> {
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
            "a `fifo` element must be a scalar (`uN`/`iN`/fixed-point) in CHAN1",
        )),
    }
}

/// Make a type-source string safe inside an identifier (`u4.4` -> `u4p4`).
fn mangle(elem_src: &str) -> String {
    elem_src.replace('.', "p")
}

/// Reject a `fifo` nested inside an array/tuple/`Option`/`mem` field (CHAN1 holds
/// a fifo only in a plain field, mirroring the `mem`/pipeline placement rule).
fn reject_nested_fifo(t: &TypeRef, pos: Pos) -> Result<(), CompileError> {
    match t {
        TypeRef::Fifo { .. } => Err(sem(
            pos,
            "a `fifo` must be held in a plain struct field (not nested in an array/tuple/Option) \
             in CHAN1",
        )),
        TypeRef::Array { elem, .. } => reject_nested_fifo(elem, pos),
        TypeRef::Tuple(ts) => ts.iter().try_for_each(|t| reject_nested_fifo(t, pos)),
        TypeRef::Option(t) | TypeRef::Mem { elem: t, .. } => reject_nested_fifo(t, pos),
        _ => Ok(()),
    }
}

/// Generate the `struct` + `impl` source for one monomorphization.
fn gen_source(m: &Mono) -> String {
    let Mono {
        name,
        elem_src: ts,
        n,
        cbits,
    } = m;
    let n = *n;
    let zeros = vec!["0"; n].join(", ");

    // Per-slot next-state (constant slot index `k`, runtime `count` comparisons):
    //   write target: slot `widx` (= count - popf) gets the pushed word;
    //   on a pop everything shifts down one slot; otherwise each slot holds.
    let mut slots = Vec::with_capacity(n);
    for k in 0..n {
        let shifted = if k + 1 < n {
            format!("self.data[{}]", k + 1)
        } else {
            "0".to_string()
        };
        slots.push(format!(
            "if pushf & (widx == {k}) {{ pv }} else if popf == 1 {{ {shifted} }} \
             else {{ self.data[{k}] }}"
        ));
    }
    let data_next = slots.join(",\n            ");

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
    fn next(self, push: Option<{ts}>, pop: u1) -> {name} {{
        let popf: u1 = if self.count == 0 {{ 0 }} else {{ pop }};
        let hp: u1 = match push {{ Some(_) => 1, None => 0 }};
        let space: u1 = if self.count == {n} {{ 0 }} else {{ 1 }};
        let pushf: u1 = hp & (space | popf);
        let pv: {ts} = match push {{ Some(v) => v, None => 0 }};
        // Write index and next count are computed with conditionals (not
        // arithmetic on the `u1` flags), so no boolean/ternary expression is ever
        // widened into an arithmetic context (which trips Verilator WIDTHEXPAND).
        let widx: u{cbits} = if popf == 1 {{ (self.count - 1) as u{cbits} wrap }} else {{ self.count }};
        let count2: u{cbits} = if pushf == 1 {{
            if popf == 1 {{ self.count }} else {{ (self.count + 1) as u{cbits} wrap }}
        }} else {{
            if popf == 1 {{ (self.count - 1) as u{cbits} wrap }} else {{ self.count }}
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
