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
//!   must call exactly once per cycle;
//! - `#[cdc_ram(w)]` on an `async_fifo`'s RAM makes it dual-clock: written by
//!   domain `w`'s tick, read by the domain its `#[domain(..)]` names.
//!
//! User code may not use these attributes itself.

use std::collections::BTreeMap;

use sinter_syntax::ast::{CdcKind, Item, Pos, Program, Type, TypeRef};
use sinter_syntax::parse_program;

use crate::error::CompileError;

/// The attributes reserved for synthesized crossings.
const RESERVED_ATTRS: [&str; 4] = ["cdc", "cdc_sync", "cdc_side", "cdc_ram"];

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
    /// The element type as source (`mailbox` / `async_fifo`), e.g. `u8`.
    elem: String,
    /// The depth (`async_fifo`).
    depth: usize,
}

/// A scalar element type as Sinter source; crossings carry scalars only.
fn elem_src(t: &TypeRef, pos: Pos) -> Result<String, CompileError> {
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
            "a crossing's element must be a scalar (`uN` / `iN` / fixed-point)",
        )),
    }
}

/// The synthesized struct name of a crossing.
fn mono_name(kind: CdcKind, elem: &str, depth: usize, from: &str, to: &str) -> String {
    let e = elem.replace('.', "p");
    match kind {
        CdcKind::Sync => format!("Sync_{from}_{to}"),
        CdcKind::Pulse => format!("Pulse_{from}_{to}"),
        CdcKind::Mailbox => format!("Mailbox_{e}_{from}_{to}"),
        CdcKind::AsyncFifo => format!("AsyncFifo_{e}_{depth}_{from}_{to}"),
    }
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
            let TypeRef::Cdc {
                kind,
                elem,
                depth,
                from,
                to,
            } = &f.ty
            else {
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
            let elem = match elem {
                Some(t) => elem_src(t, f.pos)?,
                None => String::new(),
            };
            let depth = match depth.as_deref() {
                None => 1,
                Some(sinter_syntax::ast::Expr::Literal(n)) => {
                    let n = usize::try_from(*n).unwrap_or(0);
                    if n < 2 || !n.is_power_of_two() {
                        return Err(sem(
                            f.pos,
                            format!("an `{ks}` depth (`{n}`) must be a power of two ≥ 2"),
                        ));
                    }
                    n
                }
                Some(_) => return Err(sem(f.pos, format!("an `{ks}` depth must be a literal"))),
            };
            let name = mono_name(*kind, &elem, depth, from, to);
            monos.entry(name.clone()).or_insert(Mono {
                kind: *kind,
                name: name.clone(),
                from: from.clone(),
                to: to.clone(),
                elem,
                depth,
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
            if let TypeRef::Cdc {
                kind,
                elem,
                depth,
                from,
                to,
            } = &f.ty
            {
                let e = elem
                    .as_ref()
                    .and_then(|t| elem_src(t, f.pos).ok())
                    .unwrap_or_default();
                let n = match depth.as_deref() {
                    Some(sinter_syntax::ast::Expr::Literal(n)) => usize::try_from(*n).unwrap_or(0),
                    _ => 1,
                };
                let ks = kind_src(*kind);
                let spelled = match kind {
                    CdcKind::Sync | CdcKind::Pulse => format!("{ks}<{from}, {to}>"),
                    CdcKind::Mailbox => format!("{ks}<{e}, {from}, {to}>"),
                    CdcKind::AsyncFifo => format!("{ks}<{e}, {n}, {from}, {to}>"),
                };
                names.push((mono_name(*kind, &e, n, from, to), spelled));
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
        elem: ts,
        depth: n,
    } = m;
    // A channel view's synthesized name uses the mangled element type.
    let e = ts.replace('.', "p");
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
        // A two-phase (toggle) handshake carrying one word at a time. The
        // source holds `data` steady from toggling `req` until the
        // destination's `ack` comes back, so the destination's capture of it
        // is safe: `held` is the second register that may sample the source.
        CdcKind::Mailbox => format!(
            "\
#[cdc(from = {from}, to = {to})]
struct {name} {{
    #[domain({from})]
    data: {ts} = 0,
    #[domain({from})]
    req: u1 = 0,
    #[domain({from})]
    #[cdc_sync]
    ack_s1: u1 = 0,
    #[domain({from})]
    ack_s2: u1 = 0,
    #[domain({to})]
    #[cdc_sync]
    req_s1: u1 = 0,
    #[domain({to})]
    req_s2: u1 = 0,
    #[domain({to})]
    ack: u1 = 0,
    #[domain({to})]
    #[cdc_sync]
    held: {ts} = 0,
    #[domain({to})]
    full: u1 = 0,
}}

impl {name} {{
    // The `{from}` side's view: full until the last word is acknowledged.
    fn sender(self) -> Sender<{ts}, 1> {{
        let busy: u1 = if self.ack_s2 == self.req {{ 0 }} else {{ 1 }};
        Sender_{e}_1 {{ full: busy, count: busy }}
    }}
    #[cdc_side({from})]
    fn send(self, push: Push<{ts}>) -> {name} {{
        let go: u1 = match push.v {{ Some(_) => 1, None => 0 }};
        let word: {ts} = match push.v {{ Some(v) => v, None => self.data }};
        {name} {{
            data: word,
            req: self.req ^ go,
            ack_s1: self.ack,
            ack_s2: self.ack_s1,
            req_s1: self.req_s1,
            req_s2: self.req_s2,
            ack: self.ack,
            held: self.held,
            full: self.full,
        }}
    }}
    // The `{to}` side's view of the word waiting to be taken.
    fn receiver(self) -> Receiver<{ts}, 1> {{
        Receiver_{e}_1 {{
            head: if self.full == 1 {{ Some(self.held) }} else {{ None }},
            count: self.full,
        }}
    }}
    #[cdc_side({to})]
    fn recv(self, pop: u1) -> {name} {{
        let take: u1 = self.full & pop;
        let room: u1 = if self.full == 0 | take == 1 {{ 1 }} else {{ 0 }};
        let fresh: u1 = if self.req_s2 == self.ack {{ 0 }} else {{ 1 }};
        let capture: u1 = fresh & room;
        {name} {{
            data: self.data,
            req: self.req,
            ack_s1: self.ack_s1,
            ack_s2: self.ack_s2,
            req_s1: self.req,
            req_s2: self.req_s1,
            ack: self.ack ^ capture,
            held: if capture == 1 {{ self.data }} else {{ self.held }},
            full: if capture == 1 {{ 1 }} else {{ if take == 1 {{ 0 }} else {{ self.full }} }},
        }}
    }}
}}
"
        ),
        CdcKind::AsyncFifo => gen_async_fifo(name, from, to, ts, *n),
    }
}

/// The source of `g` converted from gray code to binary, for a `bits`-wide
/// value: the XOR of `g` shifted right by every amount below `bits`.
fn gray_to_bin(g: &str, bits: u32) -> String {
    let sbits = u32::BITS - bits.leading_zeros();
    (0..bits)
        .map(|k| {
            if k == 0 {
                g.to_string()
            } else {
                format!("({g} >> {k}_u{sbits})")
            }
        })
        .collect::<Vec<_>>()
        .join(" ^ ")
}

/// A dual-clock FIFO (CLK4): gray-coded pointers through 2-flop synchronizers
/// and a RAM written on `from` and read on `to`, with a first-word-fall-through
/// `front()` at full throughput.
///
/// The reader keeps the head word in `out` and up to one read in flight
/// (`pend`, whose data is the RAM's read register). The pointer it sends back
/// to the writer counts only words actually popped (`rdone`), so the slots of
/// the words in `out` and in flight stay reserved: the writer can't overwrite
/// them, and an in-flight word can be re-read while it waits.
fn gen_async_fifo(name: &str, from: &str, to: &str, ts: &str, n: usize) -> String {
    let abits = n.trailing_zeros();
    let p = abits + 1; // pointer bits: one more than the address
    let e = ts.replace('.', "p");
    let sbits = u32::BITS - p.leading_zeros();
    let rdone_sync = gray_to_bin("self.rgray_s2", p);
    let wbin_sync = gray_to_bin("self.wgray_s2", p);
    format!(
        "\
#[cdc(from = {from}, to = {to})]
struct {name} {{
    #[domain({from})]
    wbin: u{p} = 0,
    #[domain({from})]
    wgray: u{p} = 0,
    #[domain({from})]
    #[cdc_sync]
    rgray_s1: u{p} = 0,
    #[domain({from})]
    rgray_s2: u{p} = 0,
    #[domain({to})]
    #[cdc_ram({from})]
    ram: mem<{ts}, {n}> = 0,
    #[domain({to})]
    rdone: u{p} = 0,
    #[domain({to})]
    rgray: u{p} = 0,
    #[domain({to})]
    #[cdc_sync]
    wgray_s1: u{p} = 0,
    #[domain({to})]
    wgray_s2: u{p} = 0,
    #[domain({to})]
    out: {ts} = 0,
    #[domain({to})]
    out_valid: u1 = 0,
    #[domain({to})]
    pend: u1 = 0,
}}

impl {name} {{
    // The `{from}` side's view: full when {n} words are in the queue.
    fn sender(self) -> Sender<{ts}, {n}> {{
        let rdone_sync: u{p} = {rdone_sync};
        let used: u{p} = (self.wbin - rdone_sync) as u{p} wrap;
        Sender_{e}_{n} {{ full: if used == {n} {{ 1 }} else {{ 0 }}, count: used }}
    }}
    #[cdc_side({from})]
    fn send(self, push: Push<{ts}>) -> {name} {{
        let go: u1 = match push.v {{ Some(_) => 1, None => 0 }};
        let word: {ts} = match push.v {{ Some(v) => v, None => 0 }};
        let wbin2: u{p} = (self.wbin + go) as u{p} wrap;
        let waddr: u{abits} = self.wbin[{last}:0];
        {name} {{
            wbin: wbin2,
            wgray: wbin2 ^ (wbin2 >> 1_u{sbits}),
            rgray_s1: self.rgray,
            rgray_s2: self.rgray_s1,
            ram: self.ram.next(0, go, waddr, word),
            rdone: self.rdone,
            rgray: self.rgray,
            wgray_s1: self.wgray_s1,
            wgray_s2: self.wgray_s2,
            out: self.out,
            out_valid: self.out_valid,
            pend: self.pend,
        }}
    }}
    // The `{to}` side's view: the head word, wherever it is.
    fn receiver(self) -> Receiver<{ts}, {n}> {{
        let head: Option<{ts}> = if self.out_valid == 1 {{
            Some(self.out)
        }} else {{
            if self.pend == 1 {{ Some(self.ram.value()) }} else {{ None }}
        }};
        let wbin_sync: u{p} = {wbin_sync};
        let used: u{p} = (wbin_sync - self.rdone) as u{p} wrap;
        Receiver_{e}_{n} {{ head: head, count: used }}
    }}
    #[cdc_side({to})]
    fn recv(self, pop: u1) -> {name} {{
        let occ: u2 = (self.out_valid + self.pend) as u2 wrap;
        let take: u1 = if occ == 0 {{ 0 }} else {{ pop }};
        let rdone2: u{p} = (self.rdone + take) as u{p} wrap;
        let after: u2 = (occ - take) as u2 wrap;
        let issued: u{p} = (self.rdone + occ) as u{p} wrap;
        let wbin_sync: u{p} = {wbin_sync};
        let avail: u{p} = (wbin_sync - issued) as u{p} wrap;
        let issue: u1 = if avail != 0 & after <= 1 {{ 1 }} else {{ 0 }};
        // When the in-flight word can't move up, keep it in flight by
        // re-reading its slot. (Dropping it would also be correct, since its
        // slot isn't popped and would be fetched again, but would cost a
        // bubble when the consumer resumes.)
        let keep: u1 = self.out_valid & self.pend & (take == 0);
        let next_slot: u{abits} = (self.rdone[{last}:0] + 1) as u{abits} wrap;
        let raddr: u{abits} = if issue == 1 {{ issued[{last}:0] }} else {{ next_slot }};
        let ov2: u1 = if self.out_valid == 1 & take == 0 {{ 1 }} else {{ if self.pend == 1 & (take == 0 | self.out_valid == 1) {{ 1 }} else {{ 0 }} }};
        let out2: {ts} = if self.out_valid == 1 & take == 0 {{ self.out }} else {{ self.ram.value() }};
        {name} {{
            wbin: self.wbin,
            wgray: self.wgray,
            rgray_s1: self.rgray_s1,
            rgray_s2: self.rgray_s2,
            ram: self.ram.next(raddr, 0, 0, 0),
            rdone: rdone2,
            rgray: rdone2 ^ (rdone2 >> 1_u{sbits}),
            wgray_s1: self.wgray,
            wgray_s2: self.wgray_s1,
            out: out2,
            out_valid: ov2,
            pend: issue | keep,
        }}
    }}
}}
",
        last = abits - 1,
    )
}
