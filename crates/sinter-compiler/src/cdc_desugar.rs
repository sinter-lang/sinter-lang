//! Clock-domain crossing desugar (DESIGN §6.7 / §9.14, CLK2–CLK4).
//!
//! Rewrites each CDC-typed struct field (`sync<from, to>`, `pulse<from, to>`,
//! …) into a synthesized `struct` + `impl` generated as Sinter source, before
//! the channel desugar and type checking, like the channel family. The
//! generated struct is ordinary domain-tagged state, so the domain rule checks
//! it like any other, with three compiler-only markers:
//!
//! - `#[cdc(from = a, to = b)]` on the struct names its two domains;
//! - `#[cdc_sync]` on a field marks the first flip-flop of a synchronizer,
//!   the one kind of register whose next state may sample another domain;
//! - `#[cdc_side(d)]` on `send` / `recv` marks the method each domain's tick
//!   must call exactly once per cycle.
//!
//! User code may not use these attributes itself.

use std::collections::BTreeMap;

use sinter_syntax::ast::{CdcKind, Item, Pos, Program, TypeRef};
use sinter_syntax::parse_program;

use crate::error::CompileError;

/// The attributes reserved for synthesized crossings.
const RESERVED_ATTRS: [&str; 3] = ["cdc", "cdc_sync", "cdc_side"];

fn sem(pos: Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic("cdc", pos, msg)
}

fn kind_src(kind: CdcKind) -> &'static str {
    match kind {
        CdcKind::Sync => "sync",
        CdcKind::Pulse => "pulse",
        CdcKind::Mailbox => "mailbox",
        CdcKind::AsyncFifo => "async_fifo",
    }
}

/// Whether any struct field in `program` is a clock-domain crossing, or any
/// item uses a reserved crossing attribute (so it is reported, not ignored).
pub fn has_cdc(program: &Program) -> bool {
    program.items.iter().any(|it| {
        matches!(it, Item::Struct(s) if s.fields.iter().any(|f| matches!(f.ty, TypeRef::Cdc { .. })))
    }) || reject_reserved_attrs(program).is_err()
}

/// One crossing monomorphization.
struct Mono {
    kind: CdcKind,
    name: String,
    from: String,
    to: String,
}

/// Lower every CDC field to a synthesized `struct` + `impl`.
///
/// # Errors
/// A crossing whose two domains are the same, a kind not yet implemented, or
/// a user item using one of the reserved attributes.
pub fn desugar_cdc(program: &Program) -> Result<Program, CompileError> {
    reject_reserved_attrs(program)?;
    let mut out = program.clone();
    let mut monos: BTreeMap<String, Mono> = BTreeMap::new();
    for it in &mut out.items {
        let Item::Struct(s) = it else { continue };
        for f in &mut s.fields {
            let TypeRef::Cdc { kind, from, to, .. } = &f.ty else {
                continue;
            };
            let ks = kind_src(*kind);
            if from == to {
                return Err(sem(
                    f.pos,
                    format!(
                        "`{ks}<{from}, {to}>` crosses from a domain to itself; a crossing joins \
                         two different clock domains"
                    ),
                ));
            }
            if f.default.is_some() {
                return Err(sem(
                    f.pos,
                    format!(
                        "a `{ks}` field (`{}`) resets empty and takes no initializer",
                        f.name
                    ),
                ));
            }
            let name = match kind {
                CdcKind::Sync => format!("Sync_{from}_{to}"),
                CdcKind::Pulse => format!("Pulse_{from}_{to}"),
                CdcKind::Mailbox | CdcKind::AsyncFifo => {
                    return Err(sem(f.pos, format!("`{ks}` is not implemented yet")));
                }
            };
            monos.entry(name.clone()).or_insert(Mono {
                kind: *kind,
                name: name.clone(),
                from: from.clone(),
                to: to.clone(),
            });
            f.ty = TypeRef::Named(name);
        }
    }
    for m in monos.values() {
        let src = gen_source(m);
        let parsed = parse_program(&src).map_err(|e| {
            sem(
                Pos::default(),
                format!(
                    "internal error: synthesized `{}` failed to parse: {e}\n{src}",
                    m.name
                ),
            )
        })?;
        out.items.extend(parsed.items);
    }
    Ok(out)
}

/// The source spelling of each synthesized crossing name, for diagnostics.
pub fn source_names(program: &Program) -> Vec<(String, String)> {
    let mut names = Vec::new();
    for it in &program.items {
        let Item::Struct(s) = it else { continue };
        for f in &s.fields {
            if let TypeRef::Cdc { kind, from, to, .. } = &f.ty {
                let synth = match kind {
                    CdcKind::Sync => format!("Sync_{from}_{to}"),
                    CdcKind::Pulse => format!("Pulse_{from}_{to}"),
                    CdcKind::Mailbox | CdcKind::AsyncFifo => continue,
                };
                names.push((synth, format!("{}<{from}, {to}>", kind_src(*kind))));
            }
        }
    }
    names
}

fn reject_reserved_attrs(program: &Program) -> Result<(), CompileError> {
    let bad = |attrs: &[sinter_syntax::ast::Attr]| {
        attrs
            .iter()
            .find(|a| RESERVED_ATTRS.contains(&a.name.as_str()))
            .map(|a| (a.name.clone(), a.pos))
    };
    for it in &program.items {
        let found = match it {
            Item::Struct(s) => {
                bad(&s.attrs).or_else(|| s.fields.iter().find_map(|f| bad(&f.attrs)))
            }
            Item::Impl(im) => im.methods.iter().find_map(|m| bad(&m.attrs)),
            Item::Func(f) => bad(&f.attrs),
            _ => None,
        };
        if let Some((name, pos)) = found {
            return Err(sem(
                pos,
                format!(
                    "`#[{name}]` is reserved for the compiler's clock-domain crossings; use \
                     `sync`, `pulse`, `mailbox`, or `async_fifo`"
                ),
            ));
        }
    }
    Ok(())
}

fn gen_source(m: &Mono) -> String {
    let Mono {
        kind,
        name,
        from,
        to,
        ..
    } = m;
    match kind {
        // A source flip-flop, then two destination flip-flops.
        CdcKind::Sync => format!(
            "\
#[cdc(from = {from}, to = {to})]
struct {name} {{
    #[domain({from})]
    src: u1 = 0,
    #[domain({to})]
    #[cdc_sync]
    s1: u1 = 0,
    #[domain({to})]
    s2: u1 = 0,
}}

impl {name} {{
    // The `{from}` side: register the bit to send.
    #[cdc_side({from})]
    fn send(self, v: u1) -> {name} {{
        {name} {{ src: v, s1: self.s1, s2: self.s2 }}
    }}
    // The `{to}` side: advance the synchronizer.
    #[cdc_side({to})]
    fn recv(self) -> {name} {{
        {name} {{ src: self.src, s1: self.src, s2: self.s1 }}
    }}
    // The synchronized bit, in `{to}`.
    fn value(self) -> u1 {{
        self.s2
    }}
}}
"
        ),
        // A toggle per source pulse, synchronized, then edge-detected.
        CdcKind::Pulse => format!(
            "\
#[cdc(from = {from}, to = {to})]
struct {name} {{
    #[domain({from})]
    tog: u1 = 0,
    #[domain({to})]
    #[cdc_sync]
    s1: u1 = 0,
    #[domain({to})]
    s2: u1 = 0,
    #[domain({to})]
    s3: u1 = 0,
}}

impl {name} {{
    // The `{from}` side: flip the toggle for each pulse.
    #[cdc_side({from})]
    fn send(self, fire: u1) -> {name} {{
        {name} {{ tog: self.tog ^ fire, s1: self.s1, s2: self.s2, s3: self.s3 }}
    }}
    // The `{to}` side: advance the synchronizer.
    #[cdc_side({to})]
    fn recv(self) -> {name} {{
        {name} {{ tog: self.tog, s1: self.tog, s2: self.s1, s3: self.s2 }}
    }}
    // High for one `{to}` cycle per `{from}` pulse.
    fn value(self) -> u1 {{
        self.s2 ^ self.s3
    }}
}}
"
        ),
        CdcKind::Mailbox | CdcKind::AsyncFifo => String::new(),
    }
}
