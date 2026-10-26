//! Foreign Verilog modules (DESIGN §6.15, EXT1): the `#[verilog(...)]` binding
//! attribute, and the check that a reference model stays private to its
//! instance.
//!
//! A binding struct's `step` method is a Sinter **reference model** of an
//! existing Verilog module. The elaborator inlines the model like any other
//! sub-state, so the simulator runs it unchanged, and records an
//! [`ir::Instance`] so the Verilog backend can emit the real module in its
//! place. The model's registers and next-state wires therefore exist only in
//! the simulator, and `check_isolation` rejects any other logic that reads
//! them.

use std::collections::HashSet;
use std::path::PathBuf;

use sinter_syntax::ast::{AttrValue, Pos, StructItem};

use crate::error::CompileError;
use crate::ir;

/// The attribute that binds a struct to a Verilog module.
pub(crate) const ATTR: &str = "verilog";

/// A parsed `#[verilog(module = "...", src = "...", clk = "...", rst_n = "...")]`.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    /// The Verilog module name.
    pub module: String,
    /// The Verilog source file, as written (relative to the `.snt` file).
    pub src: Option<PathBuf>,
    /// The module's clock port name.
    pub clk: String,
    /// The module's reset port name, or `None` for `rst_n = none`.
    pub rst_n: Option<String>,
    /// A `#[module]` struct: Sinter generates the module from the model.
    pub generated: bool,
}

/// The attribute that makes a struct its own Verilog module.
pub(crate) const MODULE_ATTR: &str = "module";

/// The Verilog module name for `#[module]` struct `name`: its name, with a
/// module path's `::` (a struct in another source module) as `__`.
pub(crate) fn module_name(name: &str) -> String {
    name.replace("::", "__")
}

/// The binding on struct `s`, if it carries `#[verilog(...)]`.
///
/// # Errors
/// A malformed binding: a missing or non-string `module`, an unknown key, or a
/// non-string value.
pub(crate) fn binding_of(s: &StructItem) -> Result<Option<Binding>, CompileError> {
    let generated = s.attrs.iter().find(|a| a.name == MODULE_ATTR);
    let foreign = s.attrs.iter().find(|a| a.name == ATTR);
    if let (Some(g), Some(_)) = (generated, foreign) {
        return Err(CompileError::semantic(
            &s.name,
            g.pos,
            "a struct is either `#[module]` (Sinter writes its Verilog) or `#[verilog]` (bound to \
             existing Verilog), not both",
        ));
    }
    if generated.is_some() {
        return Ok(Some(Binding {
            module: module_name(&s.name),
            src: None,
            clk: "clk".to_string(),
            rst_n: Some("rst_n".to_string()),
            generated: true,
        }));
    }
    let Some(attr) = foreign else {
        return Ok(None);
    };
    let bad = |msg: String| CompileError::semantic(&s.name, attr.pos, msg);
    let mut module = None;
    let mut binding = Binding {
        module: String::new(),
        src: None,
        clk: "clk".to_string(),
        rst_n: Some("rst_n".to_string()),
        generated: false,
    };
    for arg in &attr.args {
        let text = match &arg.value {
            Some(AttrValue::Str(v)) => Some(v.clone()),
            _ => None,
        };
        match (arg.key.as_str(), text) {
            ("module", Some(v)) => module = Some(v),
            ("src", Some(v)) => binding.src = Some(PathBuf::from(v)),
            ("clk", Some(v)) => binding.clk = v,
            ("rst_n", Some(v)) => binding.rst_n = Some(v),
            ("rst_n", None) if matches!(&arg.value, Some(AttrValue::Ident(v)) if v == "none") => {
                binding.rst_n = None;
            }
            ("module" | "src" | "clk" | "rst_n", None) => {
                return Err(bad(format!(
                    "`#[verilog]` argument `{}` takes a string, e.g. `{} = \"...\"`",
                    arg.key, arg.key
                )));
            }
            (key, _) => {
                return Err(bad(format!(
                    "unknown `#[verilog]` argument `{key}`; expected `module`, `src`, `clk`, \
                     or `rst_n`"
                )));
            }
        }
    }
    binding.module = module.ok_or_else(|| {
        bad("`#[verilog]` needs the Verilog module name: `module = \"...\"`".to_string())
    })?;
    Ok(Some(binding))
}

/// Reject logic outside a reference model that reads the model's state.
///
/// The model's registers and next-state wires are absent from the emitted
/// Verilog (the real module replaces them), so anything that survives into
/// the Verilog must not reference them. That covers every wire except the
/// model-only ones and the instance outputs (whose model logic Verilog also
/// replaces), plus output drives, the other registers' next state, memory
/// ports, and sim-only asserts (which `--faults` turns into hardware).
///
/// # Errors
/// Names the first model-only signal read from outside its model.
pub(crate) fn check_isolation(m: &ir::Module, ctx: &str, pos: Pos) -> Result<(), CompileError> {
    if m.instances.is_empty() {
        return Ok(());
    }
    let private: HashSet<&str> = m
        .instances
        .iter()
        .flat_map(|i| {
            i.model_regs
                .iter()
                .chain(&i.model_lets)
                .chain(&i.model_mem_regs)
        })
        .map(String::as_str)
        .collect();

    let mut visible: Vec<&ir::Expr> = Vec::new();
    for l in &m.lets {
        if !m.is_model_only(&l.name) && m.instance_output(&l.name).is_none() {
            visible.push(&l.value);
        }
    }
    visible.extend(m.out_drives.iter().map(|(_, e)| e));
    visible.extend(
        m.reg_updates
            .iter()
            .filter(|(r, _)| !m.is_model_only(r))
            .map(|(_, e)| e),
    );
    // A `#[module]` model's own memories are model-only too.
    visible.extend(
        m.mem_reads
            .iter()
            .filter(|r| !m.is_model_only(&r.mem))
            .map(|r| &r.addr),
    );
    for w in m.mem_writes.iter().filter(|w| !m.is_model_only(&w.mem)) {
        visible.extend([&w.we, &w.addr, &w.data]);
    }
    // An assert inside a `#[module]` model is simulation-only (`--faults`
    // rejects designs with `#[module]` instances), so only a design without
    // them needs its asserts kept to visible signals.
    if !m.instances.iter().any(|i| i.generated) {
        visible.extend(m.asserts.iter().map(|a| &a.cond));
    }

    for e in visible {
        if let Some(name) = first_ident_in(e, &private) {
            let inst = m.instances.iter().find(|i| {
                i.model_regs
                    .iter()
                    .chain(&i.model_lets)
                    .chain(&i.model_mem_regs)
                    .any(|n| n == name)
            });
            let msg = match inst {
                Some(i) if i.generated => format!(
                    "`{}` is internal to the `#[module]` `{}`, whose state its parent can't \
                     read; add what you need to its outputs",
                    source_name(i, name),
                    i.model
                ),
                i => format!(
                    "`{name}` is internal state of the `#[verilog]` model `{}`, which exists \
                     only in simulation; read the module's outputs instead",
                    i.map_or("?", |i| i.model.as_str())
                ),
            };
            return Err(CompileError::semantic(ctx, pos, msg));
        }
    }
    Ok(())
}

/// The first identifier in `e` that is in `names`.
fn first_ident_in<'a>(e: &'a ir::Expr, names: &HashSet<&str>) -> Option<&'a str> {
    use ir::ExprKind as K;
    match &e.kind {
        K::Ident(n) => names.contains(n.as_str()).then_some(n.as_str()),
        K::Literal(_) => None,
        K::Unary { expr, .. }
        | K::Cast { expr, .. }
        | K::Shl { expr, .. }
        | K::Shr { expr, .. } => first_ident_in(expr, names),
        K::Binary { lhs, rhs, .. } => {
            first_ident_in(lhs, names).or_else(|| first_ident_in(rhs, names))
        }
        K::VarShift { expr, amount, .. } => {
            first_ident_in(expr, names).or_else(|| first_ident_in(amount, names))
        }
        K::If { cond, then, els } => first_ident_in(cond, names)
            .or_else(|| first_ident_in(then, names))
            .or_else(|| first_ident_in(els, names)),
    }
}

/// `name`, a model-only signal of instance `inst`, as the source spells it:
/// a state leaf `slow_count` is `self.slow.count`, and the next-state wire
/// `u_slow__next_count` is `slow.count` of the value `step` returned.
fn source_name(inst: &ir::Instance, name: &str) -> String {
    let field = inst.name.trim_start_matches("u_");
    let dotted = |s: &str| s.replace('_', ".");
    if let Some(leaf) = name.strip_prefix(&format!("{}__next_", inst.name)) {
        format!("{}.{}", dotted(field), dotted(leaf))
    } else if inst.model_regs.iter().any(|r| r == name) {
        format!("self.{}", dotted(name))
    } else {
        name.to_string()
    }
}
