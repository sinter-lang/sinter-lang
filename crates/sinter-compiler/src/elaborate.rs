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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use sinter_syntax::ast::{
    AttrValue, BinOp, CastMode, EnumItem, Expr, Func, Item, Param, Pattern, Pos, Program, Stmt,
    StructItem, Type, TypeRef, UnaryOp,
};

use crate::error::{mask, CompileError};
use crate::foreign::{self, Binding};
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
    /// `Option<T>` (DESIGN §6.5) — erases to `(is_some: u1, payload: T)`.
    Option(Box<Ty>),
    Array(Box<Ty>, usize),
    Tuple(Vec<Ty>),
    /// A ported memory (DESIGN §6.6/§9.6/§9.7): an opaque block-RAM array, never
    /// SROA-flattened. `addr_bits = log2(depth)` (depth = `1 << addr_bits`).
    /// `writable` distinguishes `mem<T, N>` (1R1W) from `rom<T, N>` (read-only,
    /// contents from `reset`).
    Mem {
        elem: Box<Ty>,
        addr_bits: u32,
        writable: bool,
    },
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
    /// A memory handle: the flattened mem name plus its element type and address
    /// width, and the pending state transition (a scheduled read + a `we`-gated
    /// write) attached by `next()` — `None` on a plain `self.<field>` handle. It is
    /// opaque (never flattens to a scalar register), and the pending transition is
    /// committed only when the handle is threaded into the next-state, so a
    /// discarded `next()` has no effect (DESIGN §9.6).
    Mem {
        name: String,
        elem: Box<Ty>,
        addr_bits: u32,
        writable: bool,
        pending: Option<Box<MemPending>>,
        /// Compile-time contents for a `rom` (per-word init); `None` for a `mem`
        /// (zero-init) and on runtime handles. Set only on the reset/default path.
        data: Option<Vec<u64>>,
    },
    /// An `Option<T>` value: a 1-bit validity flag plus the (always-present,
    /// defined) payload. `None` is `(0, zero(T))`; a `match` muxes on `is_some`.
    Option {
        /// The 1-bit "is this `Some`?" flag.
        is_some: ir::Expr,
        /// The payload (defined even when `is_some` is 0).
        payload: Box<Val>,
    },
}

/// The transition `next(…)` attaches to a memory handle: a scheduled read (its
/// result is the read-data register next cycle) plus — for a writable `mem` — a
/// `we`-gated write `(we, addr, data)` (`None` for a read-only `rom`). All
/// addresses are already masked to the memory's `addr_bits`.
#[derive(Debug, Clone)]
struct MemPending {
    read_addr: ir::Expr,
    write: Option<(ir::Expr, ir::Expr, ir::Expr)>,
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
        branch_depth: Cell::new(0),
        guards: RefCell::new(Vec::new()),
        asserts: RefCell::new(Vec::new()),
        // Pre-seed the backtrace with the root frame (the `#[clock]` state type);
        // it is never popped, so an assert at the root body reproduces the FLT0
        // `"<ctx> (line N)"` loc exactly, and inlined bodies stack on top.
        frames: RefCell::new(vec![state_ty.to_string()]),
        wires: RefCell::new(Vec::new()),
        instances: RefCell::new(Vec::new()),
        in_model: Cell::new(false),
        reserved: RefCell::new(std::collections::HashSet::new()),
        wire_names: RefCell::new(std::collections::HashSet::new()),
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
            // `const` carries no table entry; `mod` / `use` are rejected earlier
            // by `check_functional` (the module system is not yet lowered, S2/S3).
            Item::Const(_) | Item::Mod(_) | Item::Use(_) => {}
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
    /// Tracks lowering inside an `if`/`match` arm, so a memory `next()` (which
    /// returns a handle that must be threaded unchanged) is rejected in a branch
    /// rather than being muxed — gate a write with its `we` argument instead, and
    /// mux the address explicitly (DESIGN §9.6). `value()` is a pure read and is
    /// unrestricted.
    branch_depth: Cell<usize>,
    /// Validity bits collected from `?` operators (DESIGN §6.4) in the function
    /// body currently being lowered. `lower_body` swaps in a fresh accumulator
    /// per inlined function and AND-folds whatever it collected into that
    /// function's `Option` result.
    guards: RefCell<Vec<ir::Expr>>,
    /// Sim-only assertions collected across the whole elaboration (DESIGN §6.12,
    /// FLT0). Unlike `guards`, this accumulator is **never** swapped — every
    /// `assert!` site, in the root body or any inlined method, appends here, and
    /// the finished list becomes `ir::Module::asserts`.
    asserts: RefCell<Vec<ir::Assert>>,
    /// The chain of inlined frames, root-first, from the `#[clock]` root to the
    /// body currently being lowered — the static backtrace an `assert!` records
    /// (DESIGN §6.12, FLT1a). Pre-seeded with the root state-type name; `lower_body`
    /// pushes one frame per inlined function/method and pops it (balanced across
    /// the `?` error path), so a nested assert reports its full inline path. The
    /// root frame is never popped (the elaborator is single-use), so it is never
    /// empty.
    frames: RefCell<Vec<String>>,
    /// Named wires created while lowering `#[verilog]` instances (DESIGN §6.15):
    /// their port connections and the model's next state. In creation order,
    /// which is topological; they become `ir::Module::lets`.
    wires: RefCell<Vec<ir::Let>>,
    /// The `#[verilog]` instances stepped so far.
    instances: RefCell<Vec<ir::Instance>>,
    /// Set while lowering a `#[verilog]` reference model, whose body may not
    /// step another binding (that instance would exist only in simulation).
    in_model: Cell<bool>,
    /// Names the emitted module already uses (registers and their `_q` / `_next`
    /// nets, ports, memories, `clk` / `rst_n`), which a shared wire must avoid.
    reserved: RefCell<std::collections::HashSet<String>>,
    /// Names given to wires so far.
    wire_names: RefCell<std::collections::HashSet<String>>,
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
        for (name, ty, frac_bits) in &leaves {
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
                frac_bits: *frac_bits,
                reset,
            });
        }

        // Memories: the state struct's `mem`/`rom` fields, held as opaque arrays
        // (never SROA-flattened), each with its contents in `reset` (zeros for a
        // `mem`, the compile-time data for a `rom`). Each has a memory-owned
        // read-data register `<m>_rdata` (reset 0, observed as `<m>_rdata_q`) whose
        // next-state is the scheduled read (or a hold) derived from the threaded
        // handle below.
        let mems = self.mems_with_data(state_ty, tick.pos)?;
        let mut rdata_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for mem in &mems {
            let name = format!("{}_rdata", mem.name);
            if seen.insert(name.clone(), ()).is_some() {
                return Err(err(
                    &self.ctx,
                    tick.pos,
                    format!(
                        "memory read-data register `{name}` collides with a state field; rename \
                         the field or memory"
                    ),
                ));
            }
            rdata_names.insert(name.clone());
            regs.push(ir::Reg {
                name,
                ty: mem.elem_ty,
                frac_bits: 0,
                reset: 0,
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
                for (pname, pty, frac_bits) in pleaves {
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
                    // Every emitted name shares one flat namespace: a port and a
                    // `logic` (or two ports) of the same name would double-declare
                    // in Verilog. `seen` holds the state fields + synthesized
                    // `<mem>_rdata` registers; reserve each input leaf against it
                    // (this also rejects the duplicate ports an `Option<T>` field's
                    // `_is_some`/`_payload` leaves could otherwise produce — §6.5).
                    if seen.insert(pname.clone(), ()).is_some() {
                        return Err(err(
                            &self.ctx,
                            *pos,
                            format!(
                                "input `{pname}` collides with a state field, register, or another \
                                 input; rename the parameter or field"
                            ),
                        ));
                    }
                    ports.push(ir::Port {
                        dir: sinter_syntax::ast::Dir::In,
                        name: pname,
                        ty: pty,
                        frac_bits,
                    });
                }
                env.insert(name.clone(), self.idents_val(&pty, name));
            }
        }

        // Registers and ports keep their source names in the emitted Verilog
        // (they are its interface), so a SystemVerilog keyword can't be one.
        for name in regs
            .iter()
            .map(|r| &r.name)
            .chain(ports.iter().map(|p| &p.name))
        {
            if crate::verilog::is_keyword(name) {
                return Err(err(
                    &self.ctx,
                    tick.pos,
                    format!(
                        "`{name}` is a SystemVerilog keyword, so it can't name a register or \
                         port in the emitted Verilog; rename the field or parameter"
                    ),
                ));
            }
        }

        // Names the emitted module will use, which shared wires must avoid.
        {
            let mut reserved = self.reserved.borrow_mut();
            for r in &regs {
                reserved.insert(r.name.clone());
                reserved.insert(format!("{}_q", r.name));
                reserved.insert(format!("{}_next", r.name));
            }
            reserved.extend(mems.iter().map(|m| m.name.clone()));
            reserved.extend(ports.iter().map(|p| p.name.clone()));
            for fixed in ["clk", "rst_n", "fault_code"] {
                reserved.insert(fixed.to_string());
            }
        }

        // Body: lower each `let` (non-trivial values become shared wires), then
        // the tail.
        for stmt in &tick.body.stmts {
            let (pattern, ty, value, pos) = match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => (pattern, ty, value, pos),
                Stmt::Assert { cond, msg, pos, .. } => {
                    self.lower_assert(&env, cond, msg, 0, *pos)?;
                    continue;
                }
                Stmt::Yield { pos, .. } => {
                    return Err(err(
                        &self.ctx,
                        *pos,
                        "`yield` is only valid in a `pipeline fn` (not yet lowered)",
                    ))
                }
            };
            let want = self.let_hint(ty.as_ref(), *pos)?;
            let v = self.lower(&env, value, want.as_ref(), 0, *pos)?;
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

        // Collect the next-state of every `mem` field (the pending transition the
        // threaded handle carries), verifying each is passed through to its own
        // field — cross-assigning one memory to another would mis-route writes.
        let mut transitions = Vec::new();
        self.mem_transitions(&next_state, "", &mut transitions, tick.body.pos)?;

        // Register next-state from the returned state value. Read-data registers
        // are driven from their memory transition (below), not the tail struct, so
        // they are excluded here.
        let mut next = Vec::new();
        flatten_val(&next_state, "", &mut next);
        let next_by_name: HashMap<_, _> = next.into_iter().collect();
        let mut reg_updates = Vec::new();
        for r in &regs {
            if rdata_names.contains(&r.name) {
                continue;
            }
            let value = next_by_name.get(&r.name).cloned().ok_or_else(|| {
                err(
                    &self.ctx,
                    tick.body.pos,
                    format!("no next-state for `{}`", r.name),
                )
            })?;
            reg_updates.push((r.name.clone(), value));
        }

        // `#[verilog]` instances (DESIGN §6.15): the real module is clocked every
        // cycle, so each model register's next state must be exactly what its
        // one `step` produced. A hold, a mux, or another instance's state would
        // make the simulator diverge from the Verilog.
        let lets = self.wires.take();
        let instances = self.instances.take();
        let mut bound = Vec::new();
        self.binding_fields(&state, "", &mut bound, tick.body.pos)?;
        for (path, model) in bound {
            if !instances.iter().any(|i| i.name == format!("u_{path}")) {
                return Err(err(
                    &self.ctx,
                    tick.body.pos,
                    format!(
                        "`#[verilog]` instance `self.{}` (`{model}`) is never stepped; the real \
                         module runs every cycle, so call its `step` once per cycle",
                        path.replace('_', ".")
                    ),
                ));
            }
        }
        for inst in &instances {
            for (reg, wire) in inst.model_regs.iter().zip(&inst.model_lets) {
                let threaded = reg_updates.iter().any(|(r, e)| {
                    r == reg && matches!(&e.kind, ir::ExprKind::Ident(n) if n == wire)
                });
                if !threaded {
                    return Err(err(
                        &self.ctx,
                        tick.body.pos,
                        format!(
                            "the state of `#[verilog]` instance `{}` (`{}`) must be the result \
                             of its `step`, passed into the next state unchanged",
                            inst.name, inst.model
                        ),
                    ));
                }
            }
        }
        for l in &lets {
            if seen.insert(l.name.clone(), ()).is_some() {
                return Err(err(
                    &self.ctx,
                    tick.body.pos,
                    format!(
                        "`#[verilog]` port wire `{}` collides with another name",
                        l.name
                    ),
                ));
            }
        }
        let model_only: std::collections::HashSet<&str> = instances
            .iter()
            .flat_map(|i| &i.model_regs)
            .map(String::as_str)
            .collect();

        // Memory transitions: a threaded `next()` schedules the read-data register
        // (`<m>_rdata <= mem[raddr]`, pre-write ⇒ READ_FIRST) and a `we`-gated
        // write; a memory threaded unchanged holds its read-data register.
        let elem_of: HashMap<&str, Type> =
            mems.iter().map(|m| (m.name.as_str(), m.elem_ty)).collect();
        let mut mem_reads = Vec::new();
        let mut mem_writes = Vec::new();
        for (path, pending) in transitions {
            let rdata = format!("{path}_rdata");
            match pending {
                Some(p) => {
                    mem_reads.push(ir::MemRead {
                        rdata,
                        mem: path.clone(),
                        addr: p.read_addr,
                    });
                    // A `rom`'s `next()` has no write.
                    if let Some((we, addr, data)) = p.write {
                        mem_writes.push(ir::MemWrite {
                            mem: path,
                            we,
                            addr,
                            data,
                        });
                    }
                }
                None => {
                    // Idle this cycle: the read-data register holds its value.
                    let ty = elem_of.get(path.as_str()).copied().unwrap_or(Type::Uint(1));
                    reg_updates.push((rdata.clone(), typed(ir::ExprKind::Ident(rdata), ty)));
                }
            }
        }

        // Observation outputs: every register, plus any explicit outputs. Each
        // `_q` name must be fresh in the flat namespace (`seen`).
        let mut out_drives = Vec::new();
        // A `#[verilog]` model's registers exist only in simulation, so they get
        // no `_q` port.
        for r in regs
            .iter()
            .filter(|r| !model_only.contains(r.name.as_str()))
        {
            let name = format!("{}_q", r.name);
            if seen.insert(name.clone(), ()).is_some() {
                return Err(err(
                    &self.ctx,
                    tick.body.pos,
                    format!("output port `{name}` collides with another port; rename a field"),
                ));
            }
            ports.push(ir::Port {
                dir: sinter_syntax::ast::Dir::Out,
                name: name.clone(),
                ty: r.ty,
                frac_bits: r.frac_bits,
            });
            out_drives.push((name, typed(ir::ExprKind::Ident(r.name.clone()), r.ty)));
        }
        if let Some(out) = extra_out {
            let mut outs = Vec::new();
            flatten_leaves(&out, "out", &mut outs);
            for (name, value, frac_bits) in outs {
                if seen.insert(name.clone(), ()).is_some() {
                    return Err(err(
                        &self.ctx,
                        tick.body.pos,
                        format!("output port `{name}` collides with another port; rename a field"),
                    ));
                }
                ports.push(ir::Port {
                    dir: sinter_syntax::ast::Dir::Out,
                    name: name.clone(),
                    ty: value.ty,
                    frac_bits,
                });
                out_drives.push((name, value));
            }
        }

        let module = ir::Module {
            name: state_ty.to_string(),
            clock_domain: domain.to_string(),
            ports,
            regs,
            lets,
            out_drives,
            reg_updates,
            mems,
            mem_reads,
            mem_writes,
            // Every `assert!` site collected while lowering the body + inlined
            // methods (DESIGN §6.12, FLT0). Sim-only; the Verilog backend ignores
            // this vector, so the two lowerings stay byte-identical.
            asserts: self.asserts.take(),
            instances,
        };
        foreign::check_isolation(&module, &self.ctx, tick.body.pos)?;
        Ok(module)
    }

    /// Walk the next-state, collecting each `mem` field's `(path, pending
    /// transition)` and verifying it is passed through to its *own* field (the
    /// handle name equals its flattened path) — so a write can never be silently
    /// re-routed to a different field.
    fn mem_transitions(
        &self,
        v: &Val,
        path: &str,
        out: &mut Vec<(String, Option<MemPending>)>,
        pos: Pos,
    ) -> Result<(), CompileError> {
        match v {
            Val::Mem { name, pending, .. } => {
                if name != path {
                    return Err(err(
                        &self.ctx,
                        pos,
                        format!(
                            "memory field `{path}` is assigned a different memory (`{name}`); a \
                             memory must be passed through unchanged or via its own `next()`"
                        ),
                    ));
                }
                out.push((path.to_string(), pending.as_deref().cloned()));
                Ok(())
            }
            Val::Struct(_, fields) => fields
                .iter()
                .try_for_each(|(f, val)| self.mem_transitions(val, &join(path, f), out, pos)),
            Val::Array(items) => items.iter().enumerate().try_for_each(|(i, val)| {
                self.mem_transitions(val, &join(path, &i.to_string()), out, pos)
            }),
            Val::Tuple(items) => items.iter().enumerate().try_for_each(|(i, val)| {
                self.mem_transitions(val, &join(path, &i.to_string()), out, pos)
            }),
            Val::Option { payload, .. } => {
                self.mem_transitions(payload, &join(path, "payload"), out, pos)
            }
            Val::Scalar(..) | Val::Enum(..) => Ok(()),
        }
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
            // Expanded to an array before elaboration (via `compile_program`); if it
            // survives, it was used outside a `rom` initializer.
            Expr::IncludeHex(_) => Err(err(
                &self.ctx,
                pos,
                "`include_hex!` is only valid as a `rom` field initializer",
            )),
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
                // Constant index: pick the element directly (the SROA fast path).
                if let Ok(i) = self.const_index(index, pos) {
                    return items
                        .into_iter()
                        .nth(i)
                        .ok_or_else(|| err(&self.ctx, pos, format!("index {i} out of bounds")));
                }
                // Runtime index (REG1, §9.11): a combinational N:1 mux tree over the
                // elements. Start from the last element (the default for an
                // out-of-range index) and fold down so element `k` wins on `i == k`.
                let idx = self.lower_scalar(env, index, None, depth, pos)?;
                let iw = idx.ty.width();
                let mut result = items[items.len() - 1].clone();
                for k in (0..items.len() - 1).rev() {
                    let klit = typed(ir::ExprKind::Literal(mask(k as u64, iw)), idx.ty);
                    let cond = typed(binary(BinOp::Eq, idx.clone(), klit), Type::Uint(1));
                    result = self.mux(&cond, &items[k], &result, pos)?;
                }
                Ok(result)
            }
            Expr::Slice { base, hi, lo } => {
                // `base[hi:lo]` -> `(base >> lo)` truncated to `hi-lo+1` bits. The
                // operand is unsigned (F1), so the shift is logical and the wrap
                // cast keeps exactly bits `lo..=hi`. Reuses the IR shift + cast.
                let b = self.lower(env, base, None, depth, pos)?;
                let (be, _) = scalar_parts(&b, &self.ctx, pos)?;
                let hi = self.const_index(hi, pos)? as u32;
                let lo = self.const_index(lo, pos)? as u32;
                let out_w = hi - lo + 1;
                let w = be.ty.width();
                let shifted = if lo == 0 {
                    be
                } else {
                    typed(
                        ir::ExprKind::Shr {
                            expr: Box::new(be),
                            amount: lo,
                        },
                        Type::Uint(w - lo),
                    )
                };
                let result = if shifted.ty.width() == out_w {
                    shifted
                } else {
                    typed(
                        ir::ExprKind::Cast {
                            expr: Box::new(shifted),
                            mode: CastMode::Wrap,
                        },
                        Type::Uint(out_w),
                    )
                };
                Ok(Val::Scalar(result, 0))
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
            Expr::Some(inner) => {
                let hint = match expected {
                    Some(Ty::Option(t)) => Some((**t).clone()),
                    _ => None,
                };
                let payload = self.lower(env, inner, hint.as_ref(), depth, pos)?;
                Ok(Val::Option {
                    is_some: typed(ir::ExprKind::Literal(1), Type::Uint(1)),
                    payload: Box::new(payload),
                })
            }
            Expr::None => {
                let Some(Ty::Option(t)) = expected else {
                    return Err(err(&self.ctx, pos, "cannot infer the type of `None`"));
                };
                Ok(Val::Option {
                    is_some: typed(ir::ExprKind::Literal(0), Type::Uint(1)),
                    payload: Box::new(self.zero_val(t)),
                })
            }
            Expr::Try(inner) => {
                // `?` (DESIGN §6.4): record the operand's validity bit and yield
                // its payload unconditionally; `lower_body` AND-folds the recorded
                // bits into the enclosing function's `Option` result.
                let v = self.lower(env, inner, None, depth, pos)?;
                let Val::Option { is_some, payload } = v else {
                    return Err(err(&self.ctx, pos, "`?` operand is not an `Option`"));
                };
                self.guards.borrow_mut().push(is_some);
                Ok(*payload)
            }
            Expr::Unary { op, expr: inner } => {
                self.lower_unary(env, *op, inner, expected, depth, pos)
            }
            Expr::Binary { op, lhs, rhs } => {
                self.lower_binary(env, *op, lhs, rhs, expected, depth, pos)
            }
            Expr::If { cond, then, els } => {
                let c = self.lower_scalar(env, cond, Some(Type::Uint(1)), depth, pos)?;
                // The arms are muxed, so a memory access inside one would be
                // committed unconditionally — `branch_depth` lets `lower_mem_method`
                // reject it (the `cond` itself is unconditional, so it is not gated).
                self.branch_depth.set(self.branch_depth.get() + 1);
                let t = self.lower(env, then, expected, depth, pos);
                let e = self.lower(env, els, expected, depth, pos);
                self.branch_depth.set(self.branch_depth.get() - 1);
                self.mux(&c, &t?, &e?, pos)
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
                    let (pattern, ty, value, pos) = match stmt {
                        Stmt::Let {
                            pattern,
                            ty,
                            value,
                            pos,
                        } => (pattern, ty, value, pos),
                        Stmt::Assert { cond, msg, pos, .. } => {
                            self.lower_assert(&local, cond, msg, depth, *pos)?;
                            continue;
                        }
                        Stmt::Yield { pos, .. } => {
                            return Err(err(
                                &self.ctx,
                                *pos,
                                "`yield` is only valid in a `pipeline fn` (not yet lowered)",
                            ))
                        }
                    };
                    let want = self.let_hint(ty.as_ref(), *pos)?;
                    let v = self.lower(&local, value, want.as_ref(), depth, *pos)?;
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

    /// Lower an `assert!(cond, "msg")` site (DESIGN §6.12, FLT0): evaluate the
    /// condition to a 1-bit IR expression and append it to the shared assert
    /// accumulator (→ `ir::Module::asserts`). The condition is stored only in the
    /// assert record — never in `Module::lets` — so the Verilog backend emits
    /// nothing for it (the byte-identical / compile-out guarantee).
    ///
    /// An `assert!` in an `if`/`match` arm is only conditionally reached, but the
    /// sim evaluates the flat assert list unconditionally, so it is rejected in a
    /// branch (as `mem.next()` is): gate the predicate explicitly instead. Being
    /// out of every branch, a top-level assert is always live, so its unconditional
    /// evaluation is exactly correct.
    fn lower_assert(
        &self,
        env: &Env,
        cond: &Expr,
        msg: &str,
        depth: usize,
        pos: Pos,
    ) -> Result<(), CompileError> {
        if self.branch_depth.get() > 0 {
            return Err(err(
                &self.ctx,
                pos,
                "`assert!` may not appear in an `if`/`match` arm (FLT0); fold the branch condition \
                 into the predicate instead",
            ));
        }
        let c = self.lower_scalar(env, cond, Some(Type::Uint(1)), depth, pos)?;
        self.asserts.borrow_mut().push(ir::Assert {
            cond: c,
            msg: msg.to_string(),
            loc: self.assert_loc(pos),
        });
        Ok(())
    }

    /// The static backtrace for an `assert!` at `pos` (DESIGN §6.12, FLT1a): the
    /// current inline frame stack (root-first, `" -> "`-joined) with the assert's
    /// own source line appended — e.g. `"Cpu -> Alu::step@88 -> divide@45 (line
    /// 41)"`, where each `@N` is the call-site line of that inlined frame. With no
    /// inlining the stack is just the root frame, reproducing the FLT0
    /// `"<ctx> (line N)"` form exactly.
    fn assert_loc(&self, pos: Pos) -> String {
        format!("{} (line {})", self.frames.borrow().join(" -> "), pos.line)
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
            BinOp::Shl | BinOp::Shr => {
                let l = self.lower(env, lhs, None, depth, pos)?;
                let (le, lf) = scalar_parts(&l, &self.ctx, pos)?;
                let lty = le.ty;
                if let Expr::Literal(k) = rhs {
                    // BIT1: a literal amount -> the widening/narrowing IR const-shift
                    // nodes (the fixed-point path): `<<` widens by `k`, `>>` narrows
                    // by `k` (arithmetic for a signed operand).
                    let k = *k as u32;
                    let signed = lty.is_signed();
                    let width = lty.width();
                    let (kind, ty) = if op == BinOp::Shl {
                        (
                            ir::ExprKind::Shl {
                                expr: Box::new(le),
                                amount: k,
                            },
                            int_ty(signed, width + k),
                        )
                    } else {
                        (
                            ir::ExprKind::Shr {
                                expr: Box::new(le),
                                amount: k,
                            },
                            int_ty(signed, width - k),
                        )
                    };
                    Ok(Val::Scalar(typed(kind, ty), lf))
                } else {
                    // BIT2 (DESIGN §9.10): a runtime amount -> a same-width barrel
                    // shift node (arithmetic vs logical by the operand's signedness).
                    let amount = self.lower_scalar(env, rhs, None, depth, pos)?;
                    let kind = ir::ExprKind::VarShift {
                        expr: Box::new(le),
                        amount: Box::new(amount),
                        left: op == BinOp::Shl,
                    };
                    Ok(Val::Scalar(typed(kind, lty), lf))
                }
            }
            BinOp::Concat => {
                // `a @ b` -> `(a << wb) | zext(b)` in the `wa+wb`-bit result. Both
                // operands are unsigned (F1). Reuses the IR shift / cast / or.
                let a = self.lower(env, lhs, None, depth, pos)?;
                let b = self.lower(env, rhs, None, depth, pos)?;
                let (ae, _) = scalar_parts(&a, &self.ctx, pos)?;
                let (be, _) = scalar_parts(&b, &self.ctx, pos)?;
                let wb = be.ty.width();
                let out_ty = Type::Uint(ae.ty.width() + wb);
                let hi = typed(
                    ir::ExprKind::Shl {
                        expr: Box::new(ae),
                        amount: wb,
                    },
                    out_ty,
                );
                let lo = typed(
                    ir::ExprKind::Cast {
                        expr: Box::new(be),
                        mode: CastMode::Wrap,
                    },
                    out_ty,
                );
                Ok(Val::Scalar(typed(binary(BinOp::Or, hi, lo), out_ty), 0))
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
        // A bare literal is typed as the target when it fits, else as the
        // narrowest unsigned type holding it (mirrors `functional`'s check).
        let hint = match inner {
            Expr::Literal(v) => {
                let w = ti + tf;
                let fits = if ts {
                    w >= 1 && (w > 127 || *v < (1u128 << (w - 1)))
                } else {
                    w >= 128 || *v < (1u128 << w)
                };
                Some(if fits {
                    target.clone()
                } else {
                    Ty::Scalar(Type::Uint(crate::functional::literal_bits(*v)))
                })
            }
            _ => None,
        };
        let val = self.lower(env, inner, hint.as_ref(), depth, pos)?;
        let (mut e, xf) = scalar_parts(&val, &self.ctx, pos)?;
        let xs = e.ty.is_signed();
        let xi = e.ty.width() - xf;
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
        if xs != ts {
            return Ok(Val::Scalar(
                sign_change(e, target_ty, mode == CastMode::Sat),
                tf,
            ));
        }
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
        // Backtrace frame for a free function: `<callee>@<call-site line>` (FLT1a).
        // The call-site line disambiguates two calls to the same function.
        self.lower_body(&inner, f, format!("{name}@{}", pos.line), depth + 1)
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
        if let Val::Mem {
            name,
            elem,
            addr_bits,
            writable,
            ..
        } = &recv
        {
            return self.lower_mem_method(
                env,
                &name.clone(),
                &elem.clone(),
                *addr_bits,
                *writable,
                method,
                args,
                depth,
                pos,
            );
        }
        if let Val::Array(items) = &recv {
            // `a.set(i, v)` (REG1, §9.11) -> a new array where element `k` is
            // `mux(i == k, v, a[k])`: with a runtime `i` this is the 1:N
            // guarded-hold decode; each element remains an ordinary register.
            if method != "set" {
                return Err(err(
                    &self.ctx,
                    pos,
                    format!("array has no method `{method}`"),
                ));
            }
            let idx = self.lower_scalar(env, &args[0], None, depth, pos)?;
            let elem_ty = items.first().map(val_ty);
            let v = self.lower(env, &args[1], elem_ty.as_ref(), depth, pos)?;
            let iw = idx.ty.width();
            let out = items
                .iter()
                .enumerate()
                .map(|(k, cur)| {
                    let klit = typed(ir::ExprKind::Literal(mask(k as u64, iw)), idx.ty);
                    let cond = typed(binary(BinOp::Eq, idx.clone(), klit), Type::Uint(1));
                    self.mux(&cond, &v, cur, pos)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Val::Array(out));
        }
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
        if let Some(binding) = self.binding(&type_name, pos)? {
            return self.lower_binding_step(
                env, receiver, recv, &type_name, &binding, m, args, depth, pos,
            );
        }
        let inner = self.bind_params(env, Some(recv), &m.params, args, depth, pos)?;
        // Backtrace frame for a method: `<ReceiverType>::<method>@<call-site line>`
        // (FLT1a) — the receiver's concrete type qualifies the frame, and the
        // call-site line disambiguates two calls to the same method.
        self.lower_body(
            &inner,
            m,
            format!("{type_name}::{method}@{}", pos.line),
            depth + 1,
        )
    }

    /// Give each non-trivial scalar leaf of `v` (bound to `name` by a `let`, a
    /// pattern, or a call argument) its own named wire, and return `v` with
    /// those leaves replaced by references to the wires.
    ///
    /// Without this, every use of a binding would copy its whole expression
    /// tree into the IR: a register-file read used three times would be three
    /// 32-way mux trees in the Verilog and three evaluations per cycle in the
    /// simulator. Literals and plain references are already cheap and are left
    /// alone, as are memory handles.
    fn share(&self, name: &str, v: Val) -> Val {
        let leaf = |path: &str, e: ir::Expr, frac: u32| {
            if matches!(e.kind, ir::ExprKind::Literal(_) | ir::ExprKind::Ident(_)) {
                return e;
            }
            let wire = self.wire_name(path);
            let ty = e.ty;
            self.wires.borrow_mut().push(ir::Let {
                name: wire.clone(),
                ty,
                frac_bits: frac,
                value: e,
            });
            typed(ir::ExprKind::Ident(wire), ty)
        };
        match v {
            Val::Scalar(e, f) => Val::Scalar(leaf(name, e, f), f),
            Val::Enum(n, e) => Val::Enum(n, leaf(name, e, 0)),
            Val::Struct(n, fields) => Val::Struct(
                n,
                fields
                    .into_iter()
                    .map(|(f, fv)| {
                        let fv = self.share(&join(name, &f), fv);
                        (f, fv)
                    })
                    .collect(),
            ),
            Val::Tuple(items) => Val::Tuple(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, it)| self.share(&join(name, &i.to_string()), it))
                    .collect(),
            ),
            Val::Array(items) => Val::Array(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, it)| self.share(&join(name, &i.to_string()), it))
                    .collect(),
            ),
            Val::Option { is_some, payload } => Val::Option {
                is_some: leaf(&join(name, "is_some"), is_some, 0),
                payload: Box::new(self.share(&join(name, "payload"), *payload)),
            },
            Val::Mem { .. } => v,
        }
    }

    /// A fresh wire name based on the source name `base`. A name that a
    /// generated net could use (a register's, a port's, an `out*` or `u_*`
    /// port wire, a `__` internal) or that is a SystemVerilog keyword gets a
    /// `_w` suffix, then a numeric suffix makes it unique.
    fn wire_name(&self, base: &str) -> String {
        let generated = base == "out"
            || base.starts_with("out_")
            || base.starts_with("u_")
            || base.starts_with("__")
            || crate::verilog::is_keyword(base);
        let reserved = self.reserved.borrow();
        let mut used = self.wire_names.borrow_mut();
        let stem = if generated || reserved.contains(base) {
            format!("{base}_w")
        } else {
            base.to_string()
        };
        let mut name = stem.clone();
        let mut n = 1;
        while used.contains(&name) || reserved.contains(&name) {
            name = format!("{stem}_{n}");
            n += 1;
        }
        used.insert(name.clone());
        name
    }

    /// Every stateful `#[verilog]` binding in state type `ty`, as `(flattened
    /// path, model struct)`. Each one must be stepped every cycle.
    fn binding_fields(
        &self,
        ty: &Ty,
        path: &str,
        out: &mut Vec<(String, String)>,
        pos: Pos,
    ) -> Result<(), CompileError> {
        match ty {
            Ty::Struct(name) => {
                if self.binding(name, pos)?.is_some() {
                    let mut leaves = Vec::new();
                    self.flatten_ty(ty, "", &mut leaves);
                    if !leaves.is_empty() {
                        out.push((path.to_string(), name.clone()));
                    }
                    return Ok(());
                }
                for (f, fty) in self.struct_fields(name, pos)? {
                    self.binding_fields(&fty, &join(path, &f), out, pos)?;
                }
                Ok(())
            }
            Ty::Array(et, n) => {
                for i in 0..*n {
                    self.binding_fields(et, &join(path, &i.to_string()), out, pos)?;
                }
                Ok(())
            }
            Ty::Tuple(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    self.binding_fields(t, &join(path, &i.to_string()), out, pos)?;
                }
                Ok(())
            }
            Ty::Option(inner) => self.binding_fields(inner, &join(path, "payload"), out, pos),
            Ty::Scalar(_) | Ty::Fixed { .. } | Ty::Enum(..) | Ty::Mem { .. } => Ok(()),
        }
    }

    /// The `#[verilog]` binding on struct `name`, if any (DESIGN §6.15).
    fn binding(&self, name: &str, pos: Pos) -> Result<Option<Binding>, CompileError> {
        let Some(s) = self.tables.structs.get(name) else {
            return Ok(None);
        };
        foreign::binding_of(s).map_err(|e| match e {
            CompileError::Semantic { msg, .. } => err(&self.ctx, pos, msg),
            other => other,
        })
    }

    /// Lower `recv.step(args)` on a `#[verilog]` binding (DESIGN §6.15, EXT1).
    ///
    /// The model is inlined like any method, so the simulator runs it. Around
    /// it, every port and every next-state leaf becomes a named wire, recorded
    /// on an [`ir::Instance`], so the Verilog backend can emit the real module
    /// in the model's place:
    ///
    /// - each input argument drives wire `u_<path>_<param>`, which the model
    ///   reads;
    /// - each field of the model's `Out` struct drives wire `u_<path>_<field>`
    ///   (in Verilog, driven by the instance);
    /// - each next-state leaf drives the model-only wire `u_<path>__next_<leaf>`.
    ///
    /// A stateful model is identified by its state field (`self.uart` →
    /// `u_uart`) and must be stepped exactly once per cycle, outside any
    /// `if` / `match` arm, because the real module is clocked every cycle. A
    /// stateless (combinational) model gets a fresh instance per call.
    #[allow(clippy::too_many_arguments)]
    fn lower_binding_step(
        &self,
        env: &Env,
        receiver: &Expr,
        recv: Val,
        type_name: &str,
        binding: &Binding,
        m: &Func,
        args: &[Expr],
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        if self.in_model.get() {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "`{type_name}` is a `#[verilog]` binding; a reference model cannot step \
                     another binding"
                ),
            ));
        }
        // A memory is not a register leaf, so model-only memories would leak
        // into the emitted Verilog.
        if self
            .struct_fields(type_name, pos)?
            .iter()
            .any(|(_, t)| matches!(t, Ty::Mem { .. }))
        {
            return Err(err(
                &self.ctx,
                pos,
                format!("the `#[verilog]` model `{type_name}` cannot hold a `mem` / `rom` in EXT1"),
            ));
        }
        let mut leaves = Vec::new();
        self.flatten_ty(&Ty::Struct(type_name.to_string()), "", &mut leaves);
        let stateful = !leaves.is_empty();
        let path = if stateful {
            if self.branch_depth.get() > 0 {
                return Err(err(
                    &self.ctx,
                    pos,
                    format!(
                        "the `#[verilog]` instance `{type_name}` must be stepped every cycle, \
                         outside any `if` / `match` arm (the real module is always clocked); \
                         select its inputs instead"
                    ),
                ));
            }
            self_field_path(receiver).ok_or_else(|| {
                err(
                    &self.ctx,
                    pos,
                    format!(
                        "step the `#[verilog]` instance `{type_name}` through its state field, \
                         e.g. `self.{}.{}(..)`",
                        type_name.to_lowercase(),
                        m.name
                    ),
                )
            })?
        } else {
            let k = self
                .instances
                .borrow()
                .iter()
                .filter(|i| i.module == binding.module)
                .count();
            format!("{}_{k}", binding.module)
        };
        let inst = format!("u_{path}");
        if self.instances.borrow().iter().any(|i| i.name == inst) {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "`self.{}` is stepped more than once in one cycle",
                    path.replace('_', ".")
                ),
            ));
        }

        // Inputs: route each scalar argument through a named port wire.
        let mut inner = self.bind_params(env, Some(recv), &m.params, args, depth, pos)?;
        let mut inputs = Vec::new();
        for p in &m.params {
            let Param::Typed { name, .. } = p else {
                continue;
            };
            let Some(Val::Scalar(e, frac)) = inner.remove(name) else {
                return Err(err(
                    &self.ctx,
                    pos,
                    format!(
                        "`#[verilog]` model input `{name}` must be a scalar (`uN`, `iN`, or \
                         fixed-point) in EXT1"
                    ),
                ));
            };
            let wire = format!("{inst}_{name}");
            let ty = e.ty;
            self.wire_names.borrow_mut().insert(wire.clone());
            self.wires.borrow_mut().push(ir::Let {
                name: wire.clone(),
                ty,
                frac_bits: frac,
                value: e,
            });
            inner.insert(
                name.clone(),
                Val::Scalar(typed(ir::ExprKind::Ident(wire.clone()), ty), frac),
            );
            inputs.push((name.clone(), wire));
        }

        // The model itself, inlined (and so simulated) like any method. Wires
        // its body creates are part of the model, so simulation-only.
        let first_model_wire = self.wires.borrow().len();
        self.in_model.set(true);
        let result = self.lower_body(
            &inner,
            m,
            format!("{type_name}::{}@{}", m.name, pos.line),
            depth + 1,
        );
        self.in_model.set(false);
        let body_wires: Vec<String> = self.wires.borrow()[first_model_wire..]
            .iter()
            .map(|l| l.name.clone())
            .collect();
        let Val::Tuple(mut parts) = result? else {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "the `#[verilog]` model `{type_name}::{}` must return `(Self, Out)`",
                    m.name
                ),
            ));
        };
        let (Some(out), Some(state), true) = (parts.pop(), parts.pop(), parts.is_empty()) else {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "the `#[verilog]` model `{type_name}::{}` must return `(Self, Out)`",
                    m.name
                ),
            ));
        };

        // Outputs: one wire per scalar field of `Out`, named after the port.
        let Val::Struct(out_ty, fields) = out else {
            return Err(err(
                &self.ctx,
                pos,
                format!(
                    "the `#[verilog]` model `{type_name}`'s outputs must be a struct of scalars"
                ),
            ));
        };
        let mut outputs = Vec::new();
        let mut out_fields = Vec::new();
        for (port, v) in fields {
            let Val::Scalar(e, frac) = v else {
                return Err(err(
                    &self.ctx,
                    pos,
                    format!(
                        "`#[verilog]` model output `{out_ty}.{port}` must be a scalar (`uN`, \
                         `iN`, or fixed-point) in EXT1"
                    ),
                ));
            };
            let wire = format!("{inst}_{port}");
            let ty = e.ty;
            self.wire_names.borrow_mut().insert(wire.clone());
            self.wires.borrow_mut().push(ir::Let {
                name: wire.clone(),
                ty,
                frac_bits: frac,
                value: e,
            });
            out_fields.push((
                port.clone(),
                Val::Scalar(typed(ir::ExprKind::Ident(wire.clone()), ty), frac),
            ));
            outputs.push((port, wire));
        }

        // Next state: model-only wires, so `build` can check the parent threads
        // them into the state field unchanged.
        let mut next = Vec::new();
        flatten_leaves(&state, "", &mut next);
        let mut model_regs = Vec::new();
        // Next-state wires first: `build` pairs them with `model_regs` by index.
        let mut model_lets = Vec::new();
        let mut renamed = HashMap::new();
        for (leaf, e, frac) in next {
            let wire = format!("{inst}__next_{leaf}");
            renamed.insert(leaf.clone(), wire.clone());
            self.wire_names.borrow_mut().insert(wire.clone());
            self.wires.borrow_mut().push(ir::Let {
                name: wire.clone(),
                ty: e.ty,
                frac_bits: frac,
                value: e,
            });
            model_regs.push(join(&path, &leaf));
            model_lets.push(wire);
        }
        let state = rename_leaves(&state, "", &renamed);
        model_lets.extend(body_wires);

        self.instances.borrow_mut().push(ir::Instance {
            name: inst,
            module: binding.module.clone(),
            model: type_name.to_string(),
            clk: stateful.then(|| binding.clk.clone()),
            rst_n: if stateful {
                binding.rst_n.clone()
            } else {
                None
            },
            inputs,
            outputs,
            model_regs,
            model_lets,
            src: binding.src.clone(),
        });
        Ok(Val::Tuple(vec![state, Val::Struct(out_ty, out_fields)]))
    }

    /// Lower a built-in memory method (DESIGN §6.6/§9.6). `value()` returns the
    /// memory-owned read-data register (last cycle's addressed value — `Static(1)`
    /// latency) and is a pure read. `next(raddr, we, waddr, wdata)` returns the
    /// memory handle carrying a *pending* transition (a scheduled read + a
    /// `we`-gated write); the transition is committed only when the handle is
    /// threaded into the next-state (`build`), so a discarded `next()` is inert.
    /// `next()` must be unconditional — its handle cannot be muxed by an
    /// `if`/`match` (gate the write with `we`); `value()` is unrestricted.
    #[allow(clippy::too_many_arguments)]
    fn lower_mem_method(
        &self,
        env: &Env,
        mem_name: &str,
        elem: &Ty,
        addr_bits: u32,
        writable: bool,
        method: &str,
        args: &[Expr],
        depth: usize,
        pos: Pos,
    ) -> Result<Val, CompileError> {
        let elem_ty = elem
            .scalar()
            .ok_or_else(|| err(&self.ctx, pos, "memory element is not a scalar"))?;
        match method {
            "value" => Ok(Val::Scalar(
                typed(ir::ExprKind::Ident(format!("{mem_name}_rdata")), elem_ty),
                elem.fbits(),
            )),
            "next" => {
                if self.branch_depth.get() > 0 {
                    return Err(err(
                        &self.ctx,
                        pos,
                        "a memory `next()` must be unconditional (its result is threaded into the \
                         next-state, so it cannot be selected by an `if`/`match`); gate the write \
                         with the `we` argument and mux the address explicitly (MEM1)",
                    ));
                }
                let read_addr = self.mem_addr(env, &args[0], addr_bits, depth, pos)?;
                // A writable `mem` also carries a `we`-gated write; a `rom` does not.
                let write = if writable {
                    let we = self.lower_scalar(env, &args[1], Some(Type::Uint(1)), depth, pos)?;
                    let write_addr = self.mem_addr(env, &args[2], addr_bits, depth, pos)?;
                    let data_val = self.lower(env, &args[3], Some(elem), depth, pos)?;
                    let (write_data, _) = scalar_parts(&data_val, &self.ctx, pos)?;
                    Some((we, write_addr, write_data))
                } else {
                    None
                };
                Ok(Val::Mem {
                    name: mem_name.to_string(),
                    elem: Box::new(elem.clone()),
                    addr_bits,
                    writable,
                    pending: Some(Box::new(MemPending { read_addr, write })),
                    data: None,
                })
            }
            _ => Err(err(
                &self.ctx,
                pos,
                format!("a memory has no method `{method}` (only `value`/`next`)"),
            )),
        }
    }

    /// Lower a memory address: a scalar expression masked (a `Wrap` cast) to the
    /// memory's `addr_bits`, so both backends index in range identically.
    fn mem_addr(
        &self,
        env: &Env,
        e: &Expr,
        addr_bits: u32,
        depth: usize,
        pos: Pos,
    ) -> Result<ir::Expr, CompileError> {
        // A bare literal address adopts the natural address width (matching the
        // checker); a non-literal self-infers, then both are masked below.
        let hint = matches!(e, Expr::Literal(_)).then_some(Type::Uint(addr_bits));
        let a = self.lower_scalar(env, e, hint, depth, pos)?;
        let to = Type::Uint(addr_bits);
        if a.ty == to {
            Ok(a)
        } else {
            Ok(typed(
                ir::ExprKind::Cast {
                    expr: Box::new(a),
                    mode: CastMode::Wrap,
                },
                to,
            ))
        }
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
                    let v = self.lower(caller, arg, Some(&pty), depth, pos)?;
                    inner.insert(name.clone(), self.share(name, v));
                }
            }
        }
        Ok(inner)
    }

    /// Resolve an optional `let` type annotation to an expected [`Ty`], so an
    /// annotated binding (e.g. `let x: u9 = match …`) hints the value's lowering —
    /// needed where the value cannot self-infer (a bare literal / a literal match
    /// arm). An un-annotated `let` keeps `None` (self-inferring), as before.
    fn let_hint(&self, ty: Option<&TypeRef>, pos: Pos) -> Result<Option<Ty>, CompileError> {
        ty.map(|t| self.resolve(t, pos)).transpose()
    }

    fn lower_body(
        &self,
        env: &Env,
        f: &Func,
        frame: String,
        depth: usize,
    ) -> Result<Val, CompileError> {
        let ret = self.resolve(&f.ret, f.pos)?;
        // `?` operators in THIS body contribute validity bits to THIS function's
        // result; swap in a fresh accumulator and restore the caller's on the way
        // out (so a nested call's `?`s do not leak into ours, and vice-versa).
        let outer_guards = self.guards.replace(Vec::new());
        // Push this body's backtrace frame for the span of its lowering (FLT1a);
        // pop it on the way out — balanced across the `?` error path below because
        // `result` is captured without `?` and propagated only after the pop, the
        // same discipline as the `guards` restore it sits beside.
        self.frames.borrow_mut().push(frame);
        let result = self.lower_body_inner(env, f, &ret, depth);
        let my_guards = self.guards.replace(outer_guards);
        self.frames.borrow_mut().pop();
        let body = result?;

        if my_guards.is_empty() {
            return Ok(body);
        }
        // `?` was used, so this function returns `Option<U>` (the checker enforced
        // it): AND every collected validity bit into the result's `is_some`.
        let Val::Option { is_some, payload } = body else {
            return Err(err(
                &self.ctx,
                f.body.pos,
                "`?` used in a function that does not return an `Option`",
            ));
        };
        let combined = my_guards.into_iter().fold(is_some, |acc, g| {
            typed(binary(BinOp::And, acc, g), Type::Uint(1))
        });
        Ok(Val::Option {
            is_some: combined,
            payload,
        })
    }

    /// Lower a function body's `let`s + tail (shared by `lower_body`, which wraps
    /// it with the `?`-guard accumulator).
    fn lower_body_inner(
        &self,
        env: &Env,
        f: &Func,
        ret: &Ty,
        depth: usize,
    ) -> Result<Val, CompileError> {
        let mut local = env.clone();
        for stmt in &f.body.stmts {
            let (pattern, ty, value, pos) = match stmt {
                Stmt::Let {
                    pattern,
                    ty,
                    value,
                    pos,
                } => (pattern, ty, value, pos),
                Stmt::Assert { cond, msg, pos, .. } => {
                    self.lower_assert(&local, cond, msg, depth, *pos)?;
                    continue;
                }
                Stmt::Yield { pos, .. } => {
                    return Err(err(
                        &self.ctx,
                        *pos,
                        "`yield` is only valid in a `pipeline fn` (not yet lowered)",
                    ))
                }
            };
            let want = self.let_hint(ty.as_ref(), *pos)?;
            let v = self.lower(&local, value, want.as_ref(), depth, *pos)?;
            self.bind(pattern, v, &mut local, *pos)?;
        }
        self.lower(&local, &f.body.tail, Some(ret), depth, f.body.pos)
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
            // Arm bodies are muxed: a memory access inside one is conditional and
            // therefore rejected (see the `if` arm).
            self.branch_depth.set(self.branch_depth.get() + 1);
            let body = self.lower(&local, &arm.body, expected, depth, pos);
            self.branch_depth.set(self.branch_depth.get() - 1);
            cases.push((cond, body?));
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
            // `Some(p)` selects when `is_some` is set and binds the payload; `None`
            // selects when it is clear. (Match exhaustiveness is checked in F1.)
            Pattern::Some(inner) => {
                let Val::Option { is_some, payload } = scrut else {
                    return Err(err(&self.ctx, pos, "`Some(..)` pattern on a non-`Option`"));
                };
                self.bind(inner, (**payload).clone(), env, pos)?;
                Ok(Some(typed(
                    binary(
                        BinOp::Eq,
                        is_some.clone(),
                        typed(ir::ExprKind::Literal(1), Type::Uint(1)),
                    ),
                    Type::Uint(1),
                )))
            }
            Pattern::None => {
                let Val::Option { is_some, .. } = scrut else {
                    return Err(err(&self.ctx, pos, "`None` pattern on a non-`Option`"));
                };
                Ok(Some(typed(
                    binary(
                        BinOp::Eq,
                        is_some.clone(),
                        typed(ir::ExprKind::Literal(0), Type::Uint(1)),
                    ),
                    Type::Uint(1),
                )))
            }
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
            // A memory is opaque state — it cannot be selected by a mux. Both
            // branches threading the *same* memory is a no-op (return the handle);
            // two different memories cannot be muxed (there is one physical array).
            (Val::Mem { name: a, .. }, Val::Mem { name: b, .. }) => {
                if a == b {
                    Ok(t.clone())
                } else {
                    Err(err(
                        &self.ctx,
                        pos,
                        format!(
                            "cannot select between two different memories (`{a}` vs `{b}`) with an \
                             `if`/`match`; thread a memory through unchanged and gate its `.write` \
                             with the `we` argument (MEM1)"
                        ),
                    ))
                }
            }
            (
                Val::Option {
                    is_some: sa,
                    payload: pa,
                },
                Val::Option {
                    is_some: sb,
                    payload: pb,
                },
            ) => Ok(Val::Option {
                is_some: mux_expr(cond, sa, sb),
                payload: Box::new(self.mux(cond, pa, pb, pos)?),
            }),
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
                let v = self.share(name, v);
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
            Pattern::Literal(_) | Pattern::Path(_) | Pattern::Some(_) | Pattern::None => {
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
            TypeRef::Option(t) => Ok(Ty::Option(Box::new(self.resolve(t, pos)?))),
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
            TypeRef::Mem {
                elem,
                depth,
                write_ports,
                ..
            } => {
                let et = self.resolve(elem, pos)?;
                if et.scalar().is_none() {
                    return Err(err(&self.ctx, pos, "a memory element must be a scalar"));
                }
                let n = self.const_index(depth, pos)?;
                // The checker (`functional`) already enforces a power-of-two depth
                // ≥ 2; `trailing_zeros` is then exactly `log2(depth)`.
                let addr_bits = (n as u64).trailing_zeros();
                Ok(Ty::Mem {
                    elem: Box::new(et),
                    addr_bits,
                    writable: *write_ports == 1,
                })
            }
            TypeRef::Channel { .. } => Err(err(
                &self.ctx,
                pos,
                "internal error: a channel type should have been desugared before elaboration",
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
            // An `Option<T>` field defaults to `None` when no reset is written.
            Ty::Option(_) => Ok(self.zero_val(ty)),
            _ => Err(err(&self.ctx, pos, "field needs a reset value (`= …`)")),
        }
    }

    /// The all-zero value of a type: scalars/enums to `0`, aggregates recursively,
    /// an `Option` to `None`. Used for a `None` payload (a defined don't-care) and
    /// for an `Option` field's `None` reset.
    fn zero_val(&self, ty: &Ty) -> Val {
        match ty {
            Ty::Scalar(t) => Val::Scalar(typed(ir::ExprKind::Literal(0), *t), 0),
            Ty::Fixed {
                signed,
                ibits,
                fbits,
            } => Val::Scalar(
                typed(ir::ExprKind::Literal(0), int_ty(*signed, ibits + fbits)),
                *fbits,
            ),
            Ty::Enum(n, w) => Val::Enum(n.clone(), typed(ir::ExprKind::Literal(0), Type::Uint(*w))),
            Ty::Struct(name) => {
                let fields = self.struct_fields(name, Pos::default()).unwrap_or_default();
                Val::Struct(
                    name.clone(),
                    fields
                        .into_iter()
                        .map(|(f, fty)| (f, self.zero_val(&fty)))
                        .collect(),
                )
            }
            Ty::Array(et, n) => Val::Array((0..*n).map(|_| self.zero_val(et)).collect()),
            Ty::Tuple(ts) => Val::Tuple(ts.iter().map(|t| self.zero_val(t)).collect()),
            // A memory is opaque state, not a zeroable value, and cannot be an
            // `Option` payload — unreachable, but keep the match total.
            Ty::Mem {
                elem,
                addr_bits,
                writable,
                ..
            } => Val::Mem {
                name: String::new(),
                elem: elem.clone(),
                addr_bits: *addr_bits,
                writable: *writable,
                pending: None,
                data: None,
            },
            Ty::Option(inner) => Val::Option {
                is_some: typed(ir::ExprKind::Literal(0), Type::Uint(1)),
                payload: Box::new(self.zero_val(inner)),
            },
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
            // A `rom`'s contents: fold each array element to a literal word, carried
            // on the handle's `data` (→ `ir::Mem.reset`). The checker has already
            // confirmed the count and element fit.
            (
                Expr::Array(items),
                Ty::Mem {
                    elem,
                    addr_bits,
                    writable,
                    ..
                },
            ) => {
                let et = elem
                    .scalar()
                    .ok_or_else(|| err(&self.ctx, pos, "rom element is not a scalar"))?;
                let data = items
                    .iter()
                    .map(|it| self.fold_scalar(it, et, pos))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Val::Mem {
                    name: String::new(),
                    elem: elem.clone(),
                    addr_bits: *addr_bits,
                    writable: *writable,
                    pending: None,
                    data: Some(data),
                })
            }
            // A `mem`'s reset is its zero-initialized array; the field default `= 0`
            // yields an opaque handle that contributes no scalar reset leaf (the
            // checker enforces `= 0`).
            (
                _,
                Ty::Mem {
                    elem,
                    addr_bits,
                    writable,
                    ..
                },
            ) => Ok(Val::Mem {
                name: String::new(),
                elem: elem.clone(),
                addr_bits: *addr_bits,
                writable: *writable,
                pending: None,
                data: None,
            }),
            (Expr::None, Ty::Option(_)) => Ok(self.zero_val(ty)),
            (Expr::Some(inner), Ty::Option(it)) => Ok(Val::Option {
                is_some: typed(ir::ExprKind::Literal(1), Type::Uint(1)),
                payload: Box::new(self.const_val(inner, it, pos)?),
            }),
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
    /// Flatten `ty` to its scalar leaves as `(path, type, fractional bits)`;
    /// the fractional-bit count is non-zero only for a fixed-point leaf.
    fn flatten_ty(&self, ty: &Ty, path: &str, out: &mut Vec<(String, Type, u32)>) {
        match ty {
            Ty::Scalar(t) => out.push((path.to_string(), *t, 0)),
            Ty::Fixed {
                signed,
                ibits,
                fbits,
            } => out.push((path.to_string(), int_ty(*signed, ibits + fbits), *fbits)),
            Ty::Enum(_, w) => out.push((path.to_string(), Type::Uint(*w), 0)),
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
            // A memory is not a scalar register: its array is collected by
            // `collect_mem_decls`, and its read-data register is added when a
            // `.read`/`next()` is lowered. So it contributes no leaf here.
            Ty::Mem { .. } => {}
            // An `Option<T>` flattens to a validity leaf + the payload leaves.
            Ty::Option(inner) => {
                out.push((join(path, "is_some"), Type::Uint(1), 0));
                self.flatten_ty(inner, &join(path, "payload"), out);
            }
        }
    }

    /// Build the block-RAM arrays of the (possibly nested) state struct, with each
    /// `mem`/`rom` field named by its flattened path and its `reset` set to its
    /// contents — zeros for a `mem`, the compile-time data for a `rom`. Derived
    /// from the default value so the `rom` data (the field initializer) is in hand.
    fn mems_with_data(&self, state_ty: &str, pos: Pos) -> Result<Vec<ir::Mem>, CompileError> {
        let default = self.default_val(&Ty::Struct(state_ty.to_string()), None, pos)?;
        let mut out = Vec::new();
        self.collect_mem_decls(&default, "", &mut out, pos)?;
        Ok(out)
    }

    /// Walk a default value, emitting an `ir::Mem` (with contents) for each memory
    /// leaf (mirrors `flatten_val`).
    fn collect_mem_decls(
        &self,
        v: &Val,
        path: &str,
        out: &mut Vec<ir::Mem>,
        pos: Pos,
    ) -> Result<(), CompileError> {
        match v {
            Val::Mem {
                elem,
                addr_bits,
                data,
                ..
            } => {
                let elem_ty = elem
                    .scalar()
                    .ok_or_else(|| err(&self.ctx, pos, "memory element is not a scalar"))?;
                let depth = 1usize << addr_bits;
                let reset = data.clone().unwrap_or_else(|| vec![0; depth]);
                out.push(ir::Mem {
                    name: path.to_string(),
                    elem_ty,
                    depth,
                    addr_bits: *addr_bits,
                    reset,
                });
                Ok(())
            }
            Val::Struct(_, fields) => fields
                .iter()
                .try_for_each(|(f, val)| self.collect_mem_decls(val, &join(path, f), out, pos)),
            Val::Array(items) => items.iter().enumerate().try_for_each(|(i, val)| {
                self.collect_mem_decls(val, &join(path, &i.to_string()), out, pos)
            }),
            Val::Tuple(items) => items.iter().enumerate().try_for_each(|(i, val)| {
                self.collect_mem_decls(val, &join(path, &i.to_string()), out, pos)
            }),
            Val::Option { payload, .. } => {
                self.collect_mem_decls(payload, &join(path, "payload"), out, pos)
            }
            Val::Scalar(..) | Val::Enum(..) => Ok(()),
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
            Ty::Mem {
                elem,
                addr_bits,
                writable,
                ..
            } => Val::Mem {
                name: path.to_string(),
                elem: elem.clone(),
                addr_bits: *addr_bits,
                writable: *writable,
                pending: None,
                data: None,
            },
            Ty::Option(inner) => Val::Option {
                is_some: typed(ir::ExprKind::Ident(join(path, "is_some")), Type::Uint(1)),
                payload: Box::new(self.idents_val(inner, &join(path, "payload"))),
            },
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
    let mut leaves = Vec::new();
    flatten_leaves(v, path, &mut leaves);
    out.extend(leaves.into_iter().map(|(name, e, _)| (name, e)));
}

/// [`flatten_val`], also keeping each leaf's fractional-bit count (non-zero
/// only for a fixed-point leaf).
fn flatten_leaves(v: &Val, path: &str, out: &mut Vec<(String, ir::Expr, u32)>) {
    match v {
        Val::Scalar(e, f) => out.push((path.to_string(), e.clone(), *f)),
        Val::Enum(_, e) => out.push((path.to_string(), e.clone(), 0)),
        Val::Struct(_, fields) => {
            for (f, val) in fields {
                flatten_leaves(val, &join(path, f), out);
            }
        }
        Val::Array(items) => {
            for (i, val) in items.iter().enumerate() {
                flatten_leaves(val, &join(path, &i.to_string()), out);
            }
        }
        Val::Tuple(items) => {
            for (i, val) in items.iter().enumerate() {
                flatten_leaves(val, &join(path, &i.to_string()), out);
            }
        }
        // A memory is opaque: its next-state is its `MemWrite`, not a scalar
        // register update, so it contributes no leaf.
        Val::Mem { .. } => {}
        Val::Option { is_some, payload } => {
            out.push((join(path, "is_some"), is_some.clone(), 0));
            flatten_leaves(payload, &join(path, "payload"), out);
        }
    }
}

/// Convert `e` to `to`, whose signedness differs from `e`'s (DESIGN §6.3).
///
/// `wrap` is modular: extend by the *source* signedness when widening,
/// truncate when narrowing, so `u32 0xFFFF_FFFF as i32 wrap` is `-1` and
/// `i4 -1 as u8 wrap` is `255`. One IR resize does exactly that in both
/// backends. `sat` clamps the value into `to`'s range: an unsigned source
/// clamps to `to`'s maximum (only needed when `to` can't hold every source
/// value); a signed source first maps negatives to 0, then is reinterpreted as
/// unsigned at the same width and resized (saturating when narrowing).
fn sign_change(e: ir::Expr, to: Type, sat: bool) -> ir::Expr {
    let resize = |e: ir::Expr, to: Type, mode: CastMode| {
        typed(
            ir::ExprKind::Cast {
                expr: Box::new(e),
                mode,
            },
            to,
        )
    };
    let lit = |v: u64, ty: Type| typed(ir::ExprKind::Literal(mask(v, ty.width())), ty);
    let (sw, tw) = (e.ty.width(), to.width());
    if !sat {
        return resize(e, to, CastMode::Wrap);
    }
    if !e.ty.is_signed() {
        // Unsigned -> signed: values above `to`'s maximum clamp to it.
        if tw > sw {
            return resize(e, to, CastMode::Wrap);
        }
        let hi = (1u64 << (tw - 1)) - 1;
        let src = e.ty;
        let over = typed(binary(BinOp::Gt, e.clone(), lit(hi, src)), Type::Uint(1));
        let clamped = typed(
            ir::ExprKind::If {
                cond: Box::new(over),
                then: Box::new(lit(hi, src)),
                els: Box::new(e),
            },
            src,
        );
        return resize(clamped, to, CastMode::Wrap);
    }
    // Signed -> unsigned: negatives clamp to 0; the rest are non-negative, so
    // reinterpreting at the same width keeps their value.
    let src = e.ty;
    let neg = typed(binary(BinOp::Lt, e.clone(), lit(0, src)), Type::Uint(1));
    let nonneg = typed(
        ir::ExprKind::If {
            cond: Box::new(neg),
            then: Box::new(lit(0, src)),
            els: Box::new(e),
        },
        src,
    );
    let unsigned = resize(nonneg, Type::Uint(sw), CastMode::Wrap);
    if tw == sw {
        unsigned
    } else if tw > sw {
        resize(unsigned, to, CastMode::Wrap)
    } else {
        resize(unsigned, to, CastMode::Sat)
    }
}

/// `self.a.b` → `Some("a_b")` (the flattened state path); anything else → `None`.
fn self_field_path(e: &Expr) -> Option<String> {
    match e {
        Expr::Field { base, field } => match base.as_ref() {
            Expr::Ident(s) if s == "self" => Some(field.clone()),
            inner => self_field_path(inner).map(|p| join(&p, field)),
        },
        _ => None,
    }
}

/// `v` with each scalar leaf at flattened path `p` replaced by
/// `Ident(renamed[p])` (leaves not in `renamed` are kept).
fn rename_leaves(v: &Val, path: &str, renamed: &HashMap<String, String>) -> Val {
    let leaf = |e: &ir::Expr| match renamed.get(path) {
        Some(w) => typed(ir::ExprKind::Ident(w.clone()), e.ty),
        None => e.clone(),
    };
    match v {
        Val::Scalar(e, f) => Val::Scalar(leaf(e), *f),
        Val::Enum(n, e) => Val::Enum(n.clone(), leaf(e)),
        Val::Struct(n, fields) => Val::Struct(
            n.clone(),
            fields
                .iter()
                .map(|(f, fv)| (f.clone(), rename_leaves(fv, &join(path, f), renamed)))
                .collect(),
        ),
        Val::Tuple(items) => Val::Tuple(
            items
                .iter()
                .enumerate()
                .map(|(i, it)| rename_leaves(it, &join(path, &i.to_string()), renamed))
                .collect(),
        ),
        Val::Array(items) => Val::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, it)| rename_leaves(it, &join(path, &i.to_string()), renamed))
                .collect(),
        ),
        Val::Option { is_some, payload } => Val::Option {
            is_some: match renamed.get(&join(path, "is_some")) {
                Some(w) => typed(ir::ExprKind::Ident(w.clone()), is_some.ty),
                None => is_some.clone(),
            },
            payload: Box::new(rename_leaves(payload, &join(path, "payload"), renamed)),
        },
        Val::Mem { .. } => v.clone(),
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
        Val::Option { payload, .. } => Ty::Option(Box::new(val_ty(payload))),
        // Aggregates and memories are never arithmetic operands (rejected by the
        // checker); the placeholder is unreachable in a well-typed program.
        Val::Tuple(_) | Val::Array(_) | Val::Mem { .. } => Ty::Scalar(Type::Uint(1)),
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
