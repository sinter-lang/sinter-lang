//! Lower a pin map (DESIGN §7.6) to a board-top Verilog wrapper + nextpnr PCF.
//!
//! The functional surface has no top-level Verilog ports (DESIGN §2.1): the
//! elaborated [`Module`] exposes its inputs plus *every* leaf register as an
//! observation output (`<reg>_q`). A [`PinMap`] bridges that to the physical
//! world. For each `set_io` binding this:
//!
//! 1. resolves the dotted/indexed path against the module — a clock domain, an
//!    input port, or a bit of an observation output;
//! 2. synthesizes a wrapper module with one 1-bit port per pin, a power-on reset
//!    (the map has no reset pin), an instance of the inner module wired up, and
//!    polarity inversion for `low` (active-low) signals; and
//! 3. emits the matching nextpnr PCF naming the generated wrapper ports.
//!
//! The emitted Verilog conforms to the `claude-skill-verilog` standard: every
//! inner port is connected by name to a real net (so there is neither
//! `PINMISSING` nor `PINCONNECTEMPTY`), every observation bit not routed to a pin
//! is tied into a `_unused`-named sink (so there is no `UNUSEDSIGNAL`), 1-bit
//! taps are not bit-selected, and the next-state logic lives in `always_comb`
//! with `always_ff` carrying only `<=`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use sinter_syntax::ast::{Dir, Pos};

use crate::error::CompileError;
use crate::ir::Module;
use crate::pinmap::{PinDir, PinMap};

/// Number of clocks the generated power-on reset holds the design in reset.
const POR_CYCLES_LOG2: u32 = 4;

/// Wrapper-internal signal names a pin path must not shadow. `clk` is omitted —
/// it is the clock pin's own wrapper port — but a duplicate `clk` is still
/// caught by the port-name uniqueness check.
const RESERVED_NETS: &[&str] = &["rst_n", "por_count", "por_count_next", "_unused_ok"];

/// A generated board top: the wrapper SystemVerilog and the nextpnr PCF.
#[derive(Debug, Clone)]
pub struct Board {
    /// The wrapper module name (also the synthesis top).
    pub module_name: String,
    /// The wrapper SystemVerilog source.
    pub verilog: String,
    /// The nextpnr physical-constraints file.
    pub pcf: String,
}

/// One pin tap of an observation output: `(bit, wrapper port, invert)`.
type Tap = (u32, String, bool);
/// Routed observation outputs, keyed by inner output port: `(width, taps)`.
type OutTaps = HashMap<String, (u32, Vec<Tap>)>;
/// One input-pin driver: `(Some(bit) | None when whole, wrapper port, invert)`.
type InDrive = (Option<u32>, String, bool);
/// Pending input drivers, keyed by inner input port name.
type InDrives = HashMap<String, Vec<InDrive>>;

/// How a wrapper input pin (or several) drives an inner input port.
enum InputConn {
    /// The whole 1-bit input port is driven by one pin.
    Whole { wrapper_port: String, invert: bool },
    /// A multi-bit input port assembled bit-by-bit from individual pins.
    Assembled { width: u32, bits: Vec<Tap> },
}

/// Emit the board wrapper + PCF binding `module` to the pins in `pinmap`.
/// `wrapper_name` becomes the wrapper module name (sanitized to an identifier).
///
/// # Errors
/// Returns a [`CompileError`] if a path does not resolve, a pin collides, an
/// inner input is left unbound, the clock pin is missing/mismatched, or a port
/// name clashes.
pub fn emit_board(
    module: &Module,
    pinmap: &PinMap,
    wrapper_name: &str,
) -> Result<Board, CompileError> {
    let src = &pinmap.source;
    let module_name = sanitize_ident(wrapper_name);
    if module_name == module.name {
        return Err(sem(
            src,
            1,
            format!(
                "wrapper name `{module_name}` collides with the inner module; \
                 rename the pin-map file"
            ),
        ));
    }

    // Pass 1: resolve every binding into wrapper ports + inner-side drive maps.
    let mut wrap_ports: Vec<WrapPort> = Vec::new();
    let mut seen_port: HashMap<String, usize> = HashMap::new();
    let mut seen_pin: HashMap<String, usize> = HashMap::new();
    let mut clock_port: Option<String> = None;
    let mut in_drives: InDrives = HashMap::new();
    let mut out_taps: OutTaps = HashMap::new();

    for b in &pinmap.bindings {
        match b.dir {
            PinDir::Clock => {
                if b.path != module.clock_domain {
                    return Err(sem(
                        src,
                        b.line,
                        format!(
                            "clock pin binds domain `{}`, but the module's domain is `{}`",
                            b.path, module.clock_domain
                        ),
                    ));
                }
                // Check single-domain BEFORE registering, or the duplicate `clk`
                // port name would mask this clearer diagnostic.
                if clock_port.is_some() {
                    return Err(sem(
                        src,
                        b.line,
                        format!("more than one clock pin (domain `{}` is single)", b.path),
                    ));
                }
                let name = "clk".to_string();
                register_port(
                    &mut wrap_ports,
                    &mut seen_port,
                    &mut seen_pin,
                    src,
                    b.line,
                    WrapPort {
                        name: name.clone(),
                        dir: Dir::In,
                        pin: b.pin.clone(),
                        comment: format!("{} clock", b.path),
                    },
                )?;
                clock_port = Some(name);
            }
            PinDir::In => {
                let (full, tail) = flatten_path(&b.path, src, b.line)?;
                let name = full.clone();
                let (inner, bit) =
                    resolve_input(module, &full, tail.as_ref(), &b.path, src, b.line)?;
                register_port(
                    &mut wrap_ports,
                    &mut seen_port,
                    &mut seen_pin,
                    src,
                    b.line,
                    WrapPort {
                        name: name.clone(),
                        dir: Dir::In,
                        pin: b.pin.clone(),
                        comment: pin_comment(&b.path, b.active_low),
                    },
                )?;
                in_drives
                    .entry(inner)
                    .or_default()
                    .push((bit, name, b.active_low));
            }
            PinDir::Out => {
                let (full, tail) = flatten_path(&b.path, src, b.line)?;
                let name = full.clone();
                let (port, bit, width) =
                    resolve_output(module, &full, tail.as_ref(), &b.path, src, b.line)?;
                register_port(
                    &mut wrap_ports,
                    &mut seen_port,
                    &mut seen_pin,
                    src,
                    b.line,
                    WrapPort {
                        name: name.clone(),
                        dir: Dir::Out,
                        pin: b.pin.clone(),
                        comment: pin_comment(&b.path, b.active_low),
                    },
                )?;
                let entry = out_taps.entry(port).or_insert((width, Vec::new()));
                entry.1.push((bit, name, b.active_low));
            }
        }
    }

    let clk = clock_port.ok_or_else(|| {
        sem(
            src,
            1,
            format!("no `clock` pin for domain `{}`", module.clock_domain),
        )
    })?;

    // Pass 2: validate every inner input is fully driven, building its connection.
    let mut input_conn: HashMap<String, InputConn> = HashMap::new();
    for p in module.inputs() {
        let drives = in_drives.get(&p.name).ok_or_else(|| {
            sem(
                src,
                1,
                format!("inner input `{}` has no pin binding", p.name),
            )
        })?;
        let width = p.ty.width();
        if drives.iter().any(|(bit, ..)| bit.is_none()) {
            // Whole-port drive: exactly one, and the port is 1 bit.
            if drives.len() != 1 {
                return Err(sem(
                    src,
                    1,
                    format!("input `{}` is driven both whole and per-bit", p.name),
                ));
            }
            let (_, wrapper_port, invert) = &drives[0];
            input_conn.insert(
                p.name.clone(),
                InputConn::Whole {
                    wrapper_port: wrapper_port.clone(),
                    invert: *invert,
                },
            );
        } else {
            // Per-bit assembly: require full, non-overlapping coverage.
            let mut bits: Vec<(u32, String, bool)> = drives
                .iter()
                .map(|(bit, wp, inv)| (bit.expect("checked some"), wp.clone(), *inv))
                .collect();
            bits.sort_by_key(|(bit, ..)| *bit);
            for w in 0..width {
                if !bits.iter().any(|(bit, ..)| *bit == w) {
                    return Err(sem(
                        src,
                        1,
                        format!("input `{}` bit {w} is not bound to a pin", p.name),
                    ));
                }
            }
            input_conn.insert(p.name.clone(), InputConn::Assembled { width, bits });
        }
    }

    // Guard the assembled-input wire namespace (`<port>_int`) against a pin path
    // that flattens to the same name — it would duplicate-declare in the wrapper.
    for p in module.inputs() {
        if matches!(input_conn.get(&p.name), Some(InputConn::Assembled { .. })) {
            let wire = format!("{}_int", p.name);
            if let Some(line) = seen_port.get(&wire) {
                return Err(sem(
                    src,
                    *line,
                    format!(
                        "pin port `{wire}` collides with the assembled-input wire for `{}`; \
                         rename the field/param",
                        p.name
                    ),
                ));
            }
        }
    }

    let verilog = emit_verilog(
        module,
        &module_name,
        &clk,
        &wrap_ports,
        &input_conn,
        &out_taps,
    );
    let pcf = emit_pcf(&module_name, src, &wrap_ports);

    Ok(Board {
        module_name,
        verilog,
        pcf,
    })
}

/// A generated wrapper port.
struct WrapPort {
    name: String,
    dir: Dir,
    pin: String,
    comment: String,
}

fn register_port(
    ports: &mut Vec<WrapPort>,
    seen: &mut HashMap<String, usize>,
    seen_pin: &mut HashMap<String, usize>,
    src: &str,
    line: usize,
    port: WrapPort,
) -> Result<(), CompileError> {
    if RESERVED_NETS.contains(&port.name.as_str()) {
        return Err(sem(
            src,
            line,
            format!(
                "pin path flattens to reserved wrapper signal `{}`; rename the field/param",
                port.name
            ),
        ));
    }
    if let Some(prev) = seen_pin.insert(port.pin.clone(), line) {
        return Err(sem(
            src,
            line,
            format!("physical pin `{}` is already bound (line {prev})", port.pin),
        ));
    }
    if let Some(prev) = seen.insert(port.name.clone(), line) {
        return Err(sem(
            src,
            line,
            format!("pin port `{}` already defined (line {prev})", port.name),
        ));
    }
    ports.push(port);
    Ok(())
}

/// Resolve an `in` path to `(inner port, Some(bit) | None for whole)`.
fn resolve_input(
    module: &Module,
    full: &str,
    tail: Option<&(String, u32)>,
    path: &str,
    src: &str,
    line: usize,
) -> Result<(String, Option<u32>), CompileError> {
    // DESIGN §7.6: a trailing `[i]` is the BIT index and the leaf is everything
    // before it; with no trailing bracket the whole fold is a 1-bit leaf. Honour
    // that order — never match the full fold as a whole leaf when a bit index is
    // present, or `data[1]` could silently bind a sibling register `data_1`.
    if let Some((base, bit)) = tail {
        let p = module.inputs().find(|p| &p.name == base).ok_or_else(|| {
            sem(
                src,
                line,
                format!("input pin path `{path}` resolves to no input port `{base}`"),
            )
        })?;
        let w = p.ty.width();
        if *bit >= w {
            return Err(sem(
                src,
                line,
                format!("bit {bit} is out of range for input `{base}` ({w} bits)"),
            ));
        }
        return Ok((base.clone(), Some(*bit)));
    }
    let p = module.inputs().find(|p| p.name == full).ok_or_else(|| {
        sem(
            src,
            line,
            format!("input pin path `{path}` resolves to no input port"),
        )
    })?;
    let w = p.ty.width();
    if w != 1 {
        return Err(sem(
            src,
            line,
            format!("input `{full}` is {w} bits wide; address a single bit, e.g. `{path}[0]`"),
        ));
    }
    Ok((full.to_string(), None))
}

/// Resolve an `out` path to `(inner output port `<reg>_q`, bit, reg width)`.
fn resolve_output(
    module: &Module,
    full: &str,
    tail: Option<&(String, u32)>,
    path: &str,
    src: &str,
    line: usize,
) -> Result<(String, u32, u32), CompileError> {
    // DESIGN §7.6: trailing `[i]` is the BIT; the leaf is everything before it.
    // (Mirrors `resolve_input` — see the note there.)
    if let Some((base, bit)) = tail {
        let r = module
            .regs
            .iter()
            .find(|r| &r.name == base)
            .ok_or_else(|| {
                sem(
                    src,
                    line,
                    format!("output pin path `{path}` resolves to no state register `{base}`"),
                )
            })?;
        let w = r.ty.width();
        if *bit >= w {
            return Err(sem(
                src,
                line,
                format!("bit {bit} is out of range for output `{base}` ({w} bits)"),
            ));
        }
        return Ok((format!("{base}_q"), *bit, w));
    }
    let r = module.regs.iter().find(|r| r.name == full).ok_or_else(|| {
        sem(
            src,
            line,
            format!("output pin path `{path}` resolves to no state register"),
        )
    })?;
    let w = r.ty.width();
    if w != 1 {
        return Err(sem(
            src,
            line,
            format!("output `{full}` is {w} bits wide; address a single bit, e.g. `{path}[0]`"),
        ));
    }
    Ok((format!("{full}_q"), 0, 1))
}

#[allow(clippy::too_many_lines)]
fn emit_verilog(
    module: &Module,
    wrapper: &str,
    clk: &str,
    ports: &[WrapPort],
    input_conn: &HashMap<String, InputConn>,
    out_taps: &OutTaps,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "`default_nettype none");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "// Auto-generated board wrapper. Do not edit by hand.\n\
         // Binds module `{}` (clock domain `{}`) to physical pins; emit the\n\
         // matching `{wrapper}.pcf` for nextpnr.",
        module.name, module.clock_domain
    );

    // Port list (every pin is a single-bit wire). The separating comma must
    // precede the trailing comment, or it would be commented out.
    let _ = writeln!(out, "module {wrapper} (");
    let align = port_align(ports) + 1;
    let last = ports.len().saturating_sub(1);
    let lines: Vec<String> = ports
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let dir = match p.dir {
                Dir::In => "input  wire",
                Dir::Out => "output wire",
            };
            let comma = if i == last { "" } else { "," };
            let field = format!("{}{comma}", p.name);
            format!("    {dir} {field:align$}  // pin {} ({})", p.pin, p.comment)
        })
        .collect();
    let _ = writeln!(out, "{}", lines.join("\n"));
    let _ = writeln!(out, ");");

    // Power-on reset: no reset pin in the map, so hold the design in reset until
    // a `POR_CYCLES_LOG2`-bit counter saturates (`max` = 2^N - 1 clocks), then
    // release. `por_count` starts from its configuration-time init value;
    // next-state in always_comb.
    let cw = POR_CYCLES_LOG2;
    let max = (1u64 << cw) - 1;
    let _ = writeln!(
        out,
        "    // Power-on reset (the map has no reset pin): hold `rst_n` low until"
    );
    let _ = writeln!(
        out,
        "    // the counter saturates ({max} clocks after configuration), then release.",
    );
    let _ = writeln!(out, "    logic [{}:0] por_count;", cw - 1);
    let _ = writeln!(out, "    logic [{}:0] por_count_next;", cw - 1);
    let _ = writeln!(out, "    logic        rst_n;");
    // Configuration-time init in a separate `initial` (a declaration initializer
    // plus a procedural write trips Verilator's PROCASSINIT under `-Wall`).
    let _ = writeln!(out, "    initial por_count = {cw}'d0;");
    let _ = writeln!(out, "    always_comb begin");
    let _ = writeln!(
        out,
        "        por_count_next = (por_count == {cw}'d{max}) ? por_count : (por_count + {cw}'d1);"
    );
    let _ = writeln!(out, "    end");
    let _ = writeln!(out, "    always_ff @(posedge {clk}) begin");
    let _ = writeln!(out, "        por_count <= por_count_next;");
    let _ = writeln!(out, "    end");
    let _ = writeln!(out, "    assign rst_n = (por_count == {cw}'d{max});");

    // A wire for EVERY observation output. We connect them all (so Verilator
    // sees neither PINMISSING nor PINCONNECTEMPTY); bits not routed to a pin are
    // sunk below.
    if module.outputs().next().is_some() {
        let _ = writeln!(out, "    // Observation outputs (all connected).");
        for port in module.outputs() {
            let w = port.ty.width();
            if w == 1 {
                let _ = writeln!(out, "    logic {};", port.name);
            } else {
                let _ = writeln!(out, "    logic [{}:0] {};", w - 1, port.name);
            }
        }
    }

    // Wires for bit-assembled multi-bit inputs.
    let assembled: Vec<(&String, u32)> = module
        .inputs()
        .filter_map(|p| match input_conn.get(&p.name) {
            Some(InputConn::Assembled { width, .. }) => Some((&p.name, *width)),
            _ => None,
        })
        .collect();
    if !assembled.is_empty() {
        let _ = writeln!(out, "    // Inputs assembled from individual pins.");
        for (name, width) in &assembled {
            let _ = writeln!(out, "    logic [{}:0] {name}_int;", width - 1);
        }
    }

    // Inner-module instance: connect EVERY port by name (observation outputs to
    // their wires, so there is neither PINMISSING nor PINCONNECTEMPTY).
    let _ = writeln!(out, "    // Design under test.");
    let _ = writeln!(out, "    {} u_{} (", module.name, lower(&module.name));
    let mut conns: Vec<String> = Vec::new();
    conns.push(format!("        .clk      ({clk})"));
    conns.push("        .rst_n    (rst_n)".to_string());
    for p in &module.ports {
        let conn = match p.dir {
            Dir::In => match input_conn.get(&p.name) {
                Some(InputConn::Whole {
                    wrapper_port,
                    invert,
                }) => {
                    let rhs = if *invert {
                        format!("~{wrapper_port}")
                    } else {
                        wrapper_port.clone()
                    };
                    format!("        .{} ({rhs})", p.name)
                }
                Some(InputConn::Assembled { .. }) => {
                    format!("        .{} ({}_int)", p.name, p.name)
                }
                None => unreachable!("every input validated as bound"),
            },
            Dir::Out => format!("        .{} ({})", p.name, p.name),
        };
        conns.push(conn);
    }
    let _ = writeln!(out, "{}", conns.join(",\n"));
    let _ = writeln!(out, "    );");

    // Assemble bit-indexed inputs.
    if !assembled.is_empty() {
        let _ = writeln!(out, "    // Assemble inputs from their pins.");
        for p in module.inputs() {
            if let Some(InputConn::Assembled { bits, .. }) = input_conn.get(&p.name) {
                for (bit, wp, inv) in bits {
                    let rhs = if *inv { format!("~{wp}") } else { wp.clone() };
                    let _ = writeln!(out, "    assign {}_int[{bit}] = {rhs};", p.name);
                }
            }
        }
    }

    // Drive output pins (polarity resolved here; `low` => active-low board pin).
    // `unused` collects every observation bit NOT routed to a pin — whole
    // untapped outputs and the spare bits of tapped ones.
    let _ = writeln!(out, "    // Pin drives.");
    let mut unused: Vec<String> = Vec::new();
    for port in module.outputs() {
        match out_taps.get(&port.name) {
            Some((w, taps)) => {
                let mut taps = taps.clone();
                taps.sort_by_key(|(bit, ..)| *bit);
                let mut used: HashSet<u32> = HashSet::new();
                for (bit, wp, inv) in &taps {
                    used.insert(*bit);
                    // A width-1 observation wire is declared scalar (`logic x;`),
                    // so it must NOT be bit-selected — `x[0]` is an illegal select
                    // under Verilator `-Wall`.
                    let sig = if *w == 1 {
                        port.name.clone()
                    } else {
                        format!("{}[{bit}]", port.name)
                    };
                    let rhs = if *inv { format!("~{sig}") } else { sig };
                    let _ = writeln!(out, "    assign {wp} = {rhs};");
                }
                for bit in 0..*w {
                    if !used.contains(&bit) {
                        unused.push(format!("{}[{bit}]", port.name));
                    }
                }
            }
            None => unused.push(port.name.clone()),
        }
    }

    // Tie off every observation bit not routed to a pin, so Verilator raises no
    // UNUSEDSIGNAL (it exempts names containing `unused`; no pragma needed).
    if !unused.is_empty() {
        let _ = writeln!(
            out,
            "    // Tie off observation bits not routed to pins (lint-clean sink)."
        );
        let _ = writeln!(
            out,
            "    wire _unused_ok = &{{1'b0, {}}};",
            unused.join(", ")
        );
    }

    let _ = writeln!(out, "endmodule");
    let _ = writeln!(out);
    let _ = writeln!(out, "`default_nettype wire");
    out
}

fn emit_pcf(wrapper: &str, source: &str, ports: &[WrapPort]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Auto-generated nextpnr PCF from `{source}`. Do not edit by hand.\n\
         # `{wrapper}.sv` is the synthesis top."
    );
    for p in ports {
        let _ = writeln!(out, "set_io {} {}  # {}", p.name, p.pin, p.comment);
    }
    out
}

/// The pin-comment text for an in/out binding.
fn pin_comment(path: &str, active_low: bool) -> String {
    if active_low {
        format!("{path}, active-low")
    } else {
        path.to_string()
    }
}

/// Column width for aligning port names in the module header.
fn port_align(ports: &[WrapPort]) -> usize {
    ports.iter().map(|p| p.name.len()).max().unwrap_or(0)
}

/// Parse a pin path into `(folded full name, optional (base-before-last-index,
/// last-index))`. `.`/`[i]` both fold to `_i` in leaf names (matching the
/// elaborator); the last `[i]` is the candidate *bit* index (DESIGN §7.6).
fn flatten_path(
    path: &str,
    src: &str,
    line: usize,
) -> Result<(String, Option<(String, u32)>), CompileError> {
    let mut name = String::new();
    let mut base_before_last: Option<String> = None;
    let mut last_index: Option<u32> = None;
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => name.push('_'),
            '[' => {
                let mut num = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some(d) if d.is_ascii_digit() => num.push(d),
                        Some(other) => {
                            return Err(sem(
                                src,
                                line,
                                format!("invalid index character `{other}` in path `{path}`"),
                            ))
                        }
                        None => {
                            return Err(sem(
                                src,
                                line,
                                format!("unterminated `[` in path `{path}`"),
                            ))
                        }
                    }
                }
                if num.is_empty() {
                    return Err(sem(src, line, format!("empty `[]` index in path `{path}`")));
                }
                let idx: u32 = num
                    .parse()
                    .map_err(|_| sem(src, line, format!("index `{num}` too large in `{path}`")))?;
                base_before_last = Some(name.clone());
                name.push('_');
                name.push_str(&num);
                last_index = Some(idx);
            }
            ']' => {
                return Err(sem(src, line, format!("unmatched `]` in path `{path}`")));
            }
            c if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
            other => {
                return Err(sem(
                    src,
                    line,
                    format!("invalid character `{other}` in path `{path}`"),
                ))
            }
        }
    }
    let tail = match (base_before_last, last_index) {
        (Some(b), Some(i)) => Some((b, i)),
        _ => None,
    };
    Ok((name, tail))
}

/// Sanitize a string into a legal Verilog identifier.
fn sanitize_ident(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "m_");
    }
    out
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn sem(src: &str, line: usize, msg: String) -> CompileError {
    CompileError::semantic(src, Pos { line, col: 1 }, msg)
}
