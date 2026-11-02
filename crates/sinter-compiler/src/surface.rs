//! Checks on the program as written, before any desugar rewrites it.
//!
//! - [`check_attributes`]: every attribute is one the compiler knows, in a
//!   place it applies, with arguments it takes. Without it a misspelled
//!   attribute (`#[storag(bram)]`) would be silently ignored, and a new
//!   attribute could change the meaning of an existing program. Argument
//!   *values* are checked where each attribute is used.
//! - [`check_pub_internal_state`]: no clock root's `pub` field exposes the
//!   registers a desugar synthesizes (a channel's, a crossing's, or a staged function's),
//!   whose names are the compiler's, as output ports.
//! - [`check_no_reset`]: `#[no_reset]` (RST1) is only on state whose reset
//!   isn't part of a contract: not a channel's, a crossing's, a staged
//!   function's, or an instance's.

use std::collections::{HashMap, HashSet};

use sinter_syntax::ast::{Attr, FnColor, Item, Program, StructItem, TypeRef, Visibility};

use crate::error::CompileError;

/// Where an attribute is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    Struct,
    Field,
    Enum,
    Method,
    Func,
}

impl Site {
    fn describe(self) -> &'static str {
        match self {
            Site::Struct => "a struct",
            Site::Field => "a struct field",
            Site::Enum => "an enum",
            Site::Method => "a method",
            Site::Func => "a free function",
        }
    }
}

/// A known attribute: its name, where it may appear, and its argument keys
/// (`None`: any bare key, checked where the attribute is used).
struct Known {
    name: &'static str,
    site: Site,
    keys: Option<&'static [&'static str]>,
    usage: &'static str,
}

const KNOWN: &[Known] = &[
    Known {
        name: "clock",
        site: Site::Method,
        keys: Some(&["domain", "period"]),
        usage: "`#[clock(domain = sys)]` on a method",
    },
    Known {
        name: "domain",
        site: Site::Field,
        keys: None,
        usage: "`#[domain(d)]` on a state field",
    },
    Known {
        name: "encoding",
        site: Site::Enum,
        keys: Some(&["binary", "onehot"]),
        usage: "`#[encoding(binary)]` or `#[encoding(onehot)]` on an enum",
    },
    Known {
        name: "storage",
        site: Site::Field,
        keys: Some(&["regs", "lutram", "bram"]),
        usage: "`#[storage(regs)]`, `#[storage(lutram)]`, or `#[storage(bram)]` on a `fifo`, `mem`, or `rom` field",
    },
    Known {
        name: "no_reset",
        site: Site::Field,
        keys: Some(&[]),
        usage: "`#[no_reset]` on a state field",
    },
    Known {
        name: "module",
        site: Site::Struct,
        keys: Some(&[]),
        usage: "`#[module]` on a struct",
    },
    Known {
        name: "verilog",
        site: Site::Struct,
        keys: Some(&["module", "src", "clk", "rst_n"]),
        usage: "`#[verilog(module = \"name\", ...)]` on a struct",
    },
];

/// The attributes only the compiler's own desugars write.
const RESERVED: &[&str] = &["cdc", "cdc_sync", "cdc_side", "cdc_ram"];

/// An error at `pos`, inside the item named `ctx`.
fn sem(ctx: &str, pos: sinter_syntax::ast::Pos, msg: impl Into<String>) -> CompileError {
    CompileError::semantic(ctx, pos, msg)
}

fn check(attrs: &[Attr], site: Site, ctx: &str) -> Result<(), CompileError> {
    for attr in attrs {
        let name = attr.name.as_str();
        if RESERVED.contains(&name) {
            return Err(sem(
                ctx,
                attr.pos,
                format!(
                    "`#[{name}]` is reserved for the compiler's clock-domain crossings; use \
                     `sync`, `pulse`, `mailbox`, or `async_fifo`"
                ),
            ));
        }
        let Some(known) = KNOWN.iter().find(|k| k.name == name) else {
            let names: Vec<String> = KNOWN.iter().map(|k| format!("`{}`", k.name)).collect();
            return Err(sem(
                ctx,
                attr.pos,
                format!(
                    "unknown attribute `#[{name}]`; the attributes are {}",
                    names.join(", ")
                ),
            ));
        };
        if known.site != site {
            return Err(sem(
                ctx,
                attr.pos,
                format!(
                    "`#[{name}]` doesn't apply to {}; it is written {}",
                    site.describe(),
                    known.usage
                ),
            ));
        }
        if let Some(keys) = known.keys {
            if let Some(arg) = attr.args.iter().find(|a| !keys.contains(&a.key.as_str())) {
                return Err(sem(
                    ctx,
                    attr.pos,
                    format!(
                        "`#[{name}]` doesn't take `{}`; it is written {}",
                        arg.key, known.usage
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Check every attribute in `program`.
///
/// # Errors
/// A [`CompileError`] for the first unknown, misplaced, or malformed attribute.
pub fn check_attributes(program: &Program) -> Result<(), CompileError> {
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                check(&s.attrs, Site::Struct, &s.name)?;
                let has = |n: &str| s.attrs.iter().find(|a| a.name == n);
                if let (Some(m), Some(_)) = (has("module"), has("verilog")) {
                    return Err(sem(
                        &s.name,
                        m.pos,
                        "a struct is either `#[module]` (Sinter writes its Verilog) or \
                         `#[verilog]` (bound to existing Verilog), not both",
                    ));
                }
                for f in &s.fields {
                    check(&f.attrs, Site::Field, &s.name)?;
                }
            }
            Item::Enum(e) => check(&e.attrs, Site::Enum, &e.name)?,
            Item::Impl(im) => {
                for m in &im.methods {
                    check(&m.attrs, Site::Method, &m.name)?;
                }
            }
            Item::Func(f) => check(&f.attrs, Site::Func, &f.name)?,
            Item::Const(_) | Item::Mod(_) | Item::Use(_) => {}
        }
    }
    Ok(())
}

/// What a type holds that a desugar builds from compiler-named registers, if
/// anything, described for a diagnostic. `seen` breaks cycles through
/// (invalid) recursive structs.
fn internal_state<'a>(
    ty: &TypeRef,
    structs: &HashMap<&'a str, &'a StructItem>,
    staged: &HashSet<&str>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    match ty {
        TypeRef::Channel { .. } => Some("a channel".to_string()),
        TypeRef::Cdc { .. } => Some("a clock-domain crossing".to_string()),
        TypeRef::Option(inner) | TypeRef::Array { elem: inner, .. } => {
            internal_state(inner, structs, staged, seen)
        }
        TypeRef::Tuple(items) => items
            .iter()
            .find_map(|t| internal_state(t, structs, staged, seen)),
        TypeRef::Named(name) if staged.contains(name.as_str()) => {
            Some(format!("the staged function `{name}`"))
        }
        TypeRef::Named(name) => {
            let s = structs.get(name.as_str())?;
            if !seen.insert(name.clone()) {
                return None;
            }
            s.fields.iter().find_map(|f| {
                internal_state(&f.ty, structs, staged, seen)
                    .map(|what| format!("{what} (in field `{}` of `{name}`)", f.name))
            })
        }
        TypeRef::Scalar(_) | TypeRef::Fixed { .. } | TypeRef::Mem { .. } => None,
    }
}

/// Reject a `pub` field of a clock-root struct whose type holds a channel, a
/// clock-domain crossing, or a staged function instance: its ports would be
/// compiler-internal registers, which aren't a stable interface.
///
/// # Errors
/// A [`CompileError`] naming the first such field.
pub fn check_pub_internal_state(program: &Program) -> Result<(), CompileError> {
    let mut structs = HashMap::new();
    let mut staged = HashSet::new();
    let mut roots = HashSet::new();
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                structs.insert(s.name.as_str(), s);
            }
            Item::Func(f) if f.color != FnColor::Comb => {
                staged.insert(f.name.as_str());
            }
            Item::Impl(im)
                if im
                    .methods
                    .iter()
                    .any(|m| m.attrs.iter().any(|a| a.name == "clock")) =>
            {
                roots.insert(im.type_name.as_str());
            }
            _ => {}
        }
    }
    // Only the clock root's `pub` fields become ports (with everything under
    // them, whatever its own visibility); `pub` elsewhere is module visibility.
    for s in roots.iter().filter_map(|r| structs.get(r)) {
        for f in s.fields.iter().filter(|f| f.vis == Visibility::Public) {
            if let Some(what) = internal_state(&f.ty, &structs, &staged, &mut HashSet::new()) {
                return Err(sem(
                    &s.name,
                    f.pos,
                    format!(
                        "`pub` field `{}` holds {what}, whose registers are internal to the \
                         compiler and can't be output ports; make the field private, and \
                         output the values you need (such as `len()` or `front()`) through \
                         a `pub` register or the clock root's extra outputs",
                        f.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// The first `#[module]` / `#[verilog]` instance `ty` holds, by struct name.
fn instance_in<'a>(
    ty: &TypeRef,
    structs: &HashMap<&'a str, &'a StructItem>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    let is_binding = |s: &StructItem| {
        s.attrs
            .iter()
            .any(|a| a.name == "module" || a.name == "verilog")
    };
    match ty {
        TypeRef::Option(inner) | TypeRef::Array { elem: inner, .. } => {
            instance_in(inner, structs, seen)
        }
        TypeRef::Tuple(items) => items.iter().find_map(|t| instance_in(t, structs, seen)),
        TypeRef::Named(name) => {
            let s = structs.get(name.as_str())?;
            if is_binding(s) {
                return Some(format!("the instance `{name}`"));
            }
            if !seen.insert(name.clone()) {
                return None;
            }
            s.fields.iter().find_map(|f| {
                instance_in(&f.ty, structs, seen)
                    .map(|what| format!("{what} (in field `{}` of `{name}`)", f.name))
            })
        }
        _ => None,
    }
}

/// Reject `#[no_reset]` (RST1, DESIGN §9.15) where a reset is part of a
/// contract: on a field holding a channel, a clock-domain crossing, a staged
/// function, or a `#[module]` / `#[verilog]` instance, and inside a
/// `#[verilog]` model, whose registers the real module's reset governs.
///
/// # Errors
/// A [`CompileError`] naming the first such field.
pub fn check_no_reset(program: &Program) -> Result<(), CompileError> {
    let mut structs = HashMap::new();
    let mut staged = HashSet::new();
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                structs.insert(s.name.as_str(), s);
            }
            Item::Func(f) if f.color != FnColor::Comb => {
                staged.insert(f.name.as_str());
            }
            _ => {}
        }
    }
    for s in program.items.iter().filter_map(|it| match it {
        Item::Struct(s) => Some(s),
        _ => None,
    }) {
        let model = s.attrs.iter().any(|a| a.name == "verilog");
        for f in &s.fields {
            let Some(attr) = f.attrs.iter().find(|a| a.name == "no_reset") else {
                continue;
            };
            if model {
                return Err(sem(
                    &s.name,
                    attr.pos,
                    format!(
                        "`#[no_reset]` on `{}`: `{}` is a `#[verilog]` model, and the real \
                         module decides what its reset does",
                        f.name, s.name
                    ),
                ));
            }
            let what = internal_state(&f.ty, &structs, &staged, &mut HashSet::new())
                .or_else(|| instance_in(&f.ty, &structs, &mut HashSet::new()));
            if let Some(what) = what {
                return Err(sem(
                    &s.name,
                    attr.pos,
                    format!(
                        "`#[no_reset]` on `{}`, which holds {what}: its reset is part of how \
                         it works; put `#[no_reset]` on the plain registers instead",
                        f.name
                    ),
                ));
            }
        }
    }
    Ok(())
}
