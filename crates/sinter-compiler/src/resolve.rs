//! S2: cross-module name resolution + visibility (DESIGN §10).
//!
//! Takes the [`ModuleTree`] produced by the loader and lowers it to a single
//! flat [`Program`]: every item is given a globally-unique *canonical* name
//! (its module path joined with `::`) and every cross-module reference is
//! rewritten to that canonical name, enforcing `pub` / private visibility along
//! the way. The flat program then feeds the existing single-file checker (F1)
//! and elaborator unchanged — the module structure erases here, so F1–F4 and the
//! conformance oracle stay frozen and a multi-file design elaborates to the same
//! IR as the equivalent single file.
//!
//! Scope of this milestone (S2):
//! - `mod` tree + `use` imports (bare = root-relative, plus `self` / `super` /
//!   `root` qualifiers, and `::*` globs) and qualified path references.
//! - private-by-default visibility: an item is reachable from module `M` if it
//!   is `pub`, or its defining module is an ancestor of (or equal to) `M`.
//! - same-named items in different modules (disambiguated by canonical name).
//!
//! Deferred: `pub use` re-export *outward* (here `pub use` imports into the
//! current module like `use`, but does not extend the module's public surface
//! for third parties); full Rust pub-*path* visibility (intermediate-module
//! privacy); `pub(in …)`.

use std::collections::HashMap;

use sinter_syntax::ast::{
    Block, Expr, Func, ImplItem, Item, ModItem, Module, PathQualifier, Pattern, Pos, Program, Stmt,
    TypeRef, UseItem, UsePath, Visibility,
};
use sinter_syntax::ModuleTree;

use crate::error::CompileError;

/// A module's path from the root (root = empty).
type ModPath = Vec<String>;

/// Join a module path and item name into a canonical key (root items stay bare,
/// so the clock-root state struct keeps its plain Verilog module name).
fn canonical(path: &[String], name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{}::{}", path.join("::"), name)
    }
}

/// A locally-defined item (its canonical name and visibility).
struct LocalItem {
    canonical: String,
    vis: Visibility,
}

/// Per-module resolution data.
struct ModInfo {
    children: HashMap<String, ModPath>,
    locals: HashMap<String, LocalItem>,
    /// Definition items (`const`/`struct`/`enum`/`impl`/`fn`), cloned for rewrite.
    defs: Vec<Item>,
    /// Legacy `module` declarations carried through verbatim.
    legacy: Vec<Module>,
    /// Raw `use` declarations, resolved in a later pass.
    use_decls: Vec<UseItem>,
    /// Imported name → canonical, filled by [`Resolver::resolve_uses`].
    uses: HashMap<String, String>,
    /// Source file label for diagnostics.
    source: String,
}

#[derive(Default)]
struct Resolver {
    mods: HashMap<ModPath, ModInfo>,
    /// Module paths in deterministic (tree DFS) order.
    order: Vec<ModPath>,
}

/// Resolve a loaded project into a single flat [`Program`] (DESIGN §10).
///
/// # Errors
/// Returns a [`CompileError`] on a duplicate item / import, an unresolved path,
/// or an access to a private item.
pub fn resolve_project(tree: &ModuleTree) -> Result<Program, CompileError> {
    let mut r = Resolver::default();
    r.collect(tree, &[])?;
    r.resolve_uses()?;
    r.flatten()
}

/// A leading-qualifier interpretation of a raw `::`-path (an expression /
/// pattern path), plus the segments after the qualifier.
struct Qualified<'a> {
    qualifier: PathQualifier,
    segments: &'a [String],
}

fn split_qualifier(segs: &[String]) -> Qualified<'_> {
    match segs.first().map(String::as_str) {
        Some("root") => Qualified {
            qualifier: PathQualifier::Root,
            segments: &segs[1..],
        },
        Some("self") => Qualified {
            qualifier: PathQualifier::SelfMod,
            segments: &segs[1..],
        },
        Some("super") => {
            let n = segs.iter().take_while(|s| s.as_str() == "super").count();
            Qualified {
                qualifier: PathQualifier::Super(n),
                segments: &segs[n..],
            }
        }
        _ => Qualified {
            qualifier: PathQualifier::None,
            segments: segs,
        },
    }
}

impl Resolver {
    /// Walk the tree, recording each module's items, children, and `use` decls.
    fn collect(&mut self, tree: &ModuleTree, path: &[String]) -> Result<(), CompileError> {
        let mut children = HashMap::new();
        for c in &tree.children {
            let mut child_path = path.to_vec();
            child_path.push(c.name.clone());
            children.insert(c.name.clone(), child_path);
        }

        let mut locals: HashMap<String, LocalItem> = HashMap::new();
        let mut defs = Vec::new();
        let mut use_decls = Vec::new();
        for item in &tree.items {
            match item {
                Item::Use(u) => use_decls.push(u.clone()),
                Item::Const(_) | Item::Struct(_) | Item::Enum(_) | Item::Func(_) => {
                    let (name, vis, pos) = item_head(item);
                    if locals
                        .insert(
                            name.to_string(),
                            LocalItem {
                                canonical: canonical(path, name),
                                vis,
                            },
                        )
                        .is_some()
                    {
                        return Err(CompileError::semantic(
                            &tree.source,
                            pos,
                            format!("`{name}` is defined more than once in this module"),
                        ));
                    }
                    defs.push(item.clone());
                }
                // `impl` is not a named item; methods belong to their type.
                Item::Impl(_) => defs.push(item.clone()),
                // The loader lifts `mod` into `children`; none should remain here.
                Item::Mod(ModItem { name, pos, .. }) => {
                    return Err(CompileError::semantic(
                        &tree.source,
                        *pos,
                        format!("unexpected nested `mod {name}` after loading"),
                    ));
                }
            }
        }

        // The legacy `module` form is single-file only (DESIGN §10); a submodule
        // file holding one would flatten to a duplicate top-level Verilog module.
        if !path.is_empty() {
            if let Some(m) = tree.modules.first() {
                return Err(CompileError::semantic(
                    &tree.source,
                    m.pos,
                    format!(
                        "legacy `module {}` is not allowed in a submodule (functional surface only)",
                        m.name
                    ),
                ));
            }
        }

        // A submodule and an item cannot share a name in one parent, or the path
        // `parent::name` would be ambiguous (and `walk_modules` would shadow the
        // item with the module).
        for child in children.keys() {
            if let Some(item) = locals.get(child) {
                return Err(CompileError::semantic(
                    &tree.source,
                    Pos::default(),
                    format!(
                        "`{child}` is both a submodule and an item `{}`; rename one",
                        item.canonical
                    ),
                ));
            }
        }

        self.mods.insert(
            path.to_vec(),
            ModInfo {
                children,
                locals,
                defs,
                legacy: tree.modules.clone(),
                use_decls,
                uses: HashMap::new(),
                source: tree.source.clone(),
            },
        );
        self.order.push(path.to_vec());

        for c in &tree.children {
            let mut child_path = path.to_vec();
            child_path.push(c.name.clone());
            self.collect(c, &child_path)?;
        }
        Ok(())
    }

    /// Resolve every module's `use` declarations into its `uses` map, checking
    /// that each imported item is accessible.
    fn resolve_uses(&mut self) -> Result<(), CompileError> {
        // Resolve against an immutable view, then commit, to satisfy the borrow
        // checker (resolution reads `self.mods`).
        let mut resolved: Vec<(ModPath, Vec<(String, String)>)> = Vec::new();
        for path in &self.order {
            let info = &self.mods[path];
            let mut binds: Vec<(String, String)> = Vec::new();
            // Pass 1: explicit `use` imports — a duplicate, or a clash with a
            // local item, is an error.
            for u in info.use_decls.iter().filter(|u| !u.path.glob) {
                for (name, canon) in self.resolve_use(path, &u.path, &info.source)? {
                    if binds.iter().any(|(n, _)| n == &name) || info.locals.contains_key(&name) {
                        return Err(CompileError::semantic(
                            &info.source,
                            u.pos,
                            format!("`{name}` is imported or defined more than once"),
                        ));
                    }
                    binds.push((name, canon));
                }
            }
            // Pass 2: glob (`::*`) imports yield to locals and explicit imports
            // (Rust glob-shadowing precedence); a glob name already bound is just
            // skipped, never an error.
            for u in info.use_decls.iter().filter(|u| u.path.glob) {
                for (name, canon) in self.resolve_use(path, &u.path, &info.source)? {
                    if binds.iter().any(|(n, _)| n == &name) || info.locals.contains_key(&name) {
                        continue;
                    }
                    binds.push((name, canon));
                }
            }
            resolved.push((path.clone(), binds));
        }
        for (path, binds) in resolved {
            let info = self.mods.get_mut(&path).expect("module exists");
            for (name, canon) in binds {
                info.uses.insert(name, canon);
            }
        }
        Ok(())
    }

    /// Resolve one `use` path to its imported `(name, canonical)` bindings (a
    /// glob yields several).
    fn resolve_use(
        &self,
        cur: &[String],
        up: &UsePath,
        src: &str,
    ) -> Result<Vec<(String, String)>, CompileError> {
        let start = self.start_module(cur, up.qualifier, src)?;
        let (m, rest) = self.walk_modules(start, &up.segments);
        if up.glob {
            if !rest.is_empty() {
                return Err(CompileError::semantic(
                    src,
                    Pos::default(),
                    format!("glob `use` path segment `{}` is not a module", rest[0]),
                ));
            }
            let info = self
                .mods
                .get(&m)
                .expect("walk_modules only descends into existing modules");
            let mut out = Vec::new();
            for (name, item) in &info.locals {
                if accessible(item.vis, &m, cur) {
                    out.push((name.clone(), item.canonical.clone()));
                }
            }
            // Deterministic order so the flat program does not depend on HashMap
            // iteration order.
            out.sort();
            Ok(out)
        } else {
            match rest {
                [item] => {
                    let canon = self.member(&m, item, cur, src, Pos::default())?;
                    Ok(vec![(item.clone(), canon)])
                }
                [] => Err(CompileError::semantic(
                    src,
                    Pos::default(),
                    "`use` path names a module, not an item (add `::*` to glob it)".to_string(),
                )),
                _ => Err(CompileError::semantic(
                    src,
                    Pos::default(),
                    format!("unresolved `use` path segment `{}`", rest[0]),
                )),
            }
        }
    }

    /// The module a path resolves *from*, given its leading qualifier.
    fn start_module(
        &self,
        cur: &[String],
        qualifier: PathQualifier,
        src: &str,
    ) -> Result<ModPath, CompileError> {
        match qualifier {
            // A bare path is root-relative (the project root plays Rust's "crate").
            PathQualifier::None | PathQualifier::Root => Ok(Vec::new()),
            PathQualifier::SelfMod => Ok(cur.to_vec()),
            PathQualifier::Super(n) => {
                if n > cur.len() {
                    return Err(CompileError::semantic(
                        src,
                        Pos::default(),
                        "`super` goes above the project root".to_string(),
                    ));
                }
                Ok(cur[..cur.len() - n].to_vec())
            }
        }
    }

    /// Consume leading segments that name child modules, returning the module
    /// reached and the remaining (non-module) segments.
    fn walk_modules<'a>(&self, start: ModPath, segs: &'a [String]) -> (ModPath, &'a [String]) {
        let mut m = start;
        let mut i = 0;
        while i < segs.len() {
            let Some(info) = self.mods.get(&m) else { break };
            if let Some(child) = info.children.get(&segs[i]) {
                m = child.clone();
                i += 1;
            } else {
                break;
            }
        }
        (m, &segs[i..])
    }

    /// Resolve a member `name` of module `m`, accessible from `from`.
    fn member(
        &self,
        m: &[String],
        name: &str,
        from: &[String],
        src: &str,
        pos: Pos,
    ) -> Result<String, CompileError> {
        let info = self.mods.get(m).ok_or_else(|| {
            CompileError::semantic(src, pos, format!("no module `{}`", m.join("::")))
        })?;
        let item = info.locals.get(name).ok_or_else(|| {
            CompileError::semantic(
                src,
                pos,
                format!("no item `{name}` in module `{}`", mod_label(m)),
            )
        })?;
        if accessible(item.vis, m, from) {
            Ok(item.canonical.clone())
        } else {
            Err(CompileError::semantic(
                src,
                pos,
                format!("`{name}` in module `{}` is private", mod_label(m)),
            ))
        }
    }

    /// Look up a bare name in `cur`'s scope (its own items, then imports).
    fn scope_lookup(&self, cur: &[String], name: &str) -> Option<String> {
        let info = self.mods.get(cur)?;
        if let Some(local) = info.locals.get(name) {
            return Some(local.canonical.clone());
        }
        info.uses.get(name).cloned()
    }

    // --- flatten + rewrite -------------------------------------------------

    fn flatten(&self) -> Result<Program, CompileError> {
        let mut items = Vec::new();
        let mut modules = Vec::new();
        for path in &self.order {
            let info = &self.mods[path];
            modules.extend(info.legacy.iter().cloned());
            for def in &info.defs {
                let mut d = def.clone();
                self.rewrite_item(&mut d, path, &info.source)?;
                items.push(d);
            }
        }
        Ok(Program { modules, items })
    }

    fn rewrite_item(&self, item: &mut Item, cur: &[String], src: &str) -> Result<(), CompileError> {
        match item {
            Item::Const(c) => {
                c.name = canonical(cur, &c.name);
                self.rewrite_typeref(&mut c.ty, cur, src)?;
                self.rewrite_expr(&mut c.value, cur, &mut Locals::new(), src)?;
            }
            Item::Struct(s) => {
                s.name = canonical(cur, &s.name);
                for f in &mut s.fields {
                    self.rewrite_typeref(&mut f.ty, cur, src)?;
                    if let Some(d) = &mut f.default {
                        self.rewrite_expr(d, cur, &mut Locals::new(), src)?;
                    }
                }
            }
            Item::Enum(e) => {
                e.name = canonical(cur, &e.name);
                for v in &mut e.variants {
                    if let Some(d) = &mut v.discriminant {
                        self.rewrite_expr(d, cur, &mut Locals::new(), src)?;
                    }
                }
            }
            Item::Func(f) => {
                f.name = canonical(cur, &f.name);
                self.rewrite_fn(f, cur, src)?;
            }
            Item::Impl(im) => {
                if let Some(canon) = self.scope_lookup(cur, &im.type_name) {
                    im.type_name = canon;
                }
                // The `#[clock]` root's state struct must live in the project
                // ROOT module — otherwise its canonical name carries a `::` and
                // becomes an invalid Verilog module name (DESIGN §10).
                if impl_has_clock(im) && im.type_name.contains("::") {
                    return Err(CompileError::semantic(
                        src,
                        im.pos,
                        format!(
                            "the `#[clock]` root struct `{}` must be defined in the project \
                             root module (DESIGN §10)",
                            im.type_name
                        ),
                    ));
                }
                for m in &mut im.methods {
                    self.rewrite_fn(m, cur, src)?;
                }
            }
            Item::Mod(_) | Item::Use(_) => {}
        }
        Ok(())
    }

    fn rewrite_fn(&self, f: &mut Func, cur: &[String], src: &str) -> Result<(), CompileError> {
        let mut locals = Locals::new();
        locals.enter();
        for p in &mut f.params {
            match p {
                sinter_syntax::ast::Param::Typed { name, ty, .. } => {
                    self.rewrite_typeref(ty, cur, src)?;
                    locals.bind(name.clone());
                }
                sinter_syntax::ast::Param::SelfRef { .. } => locals.bind("self".to_string()),
            }
        }
        self.rewrite_typeref(&mut f.ret, cur, src)?;
        self.rewrite_block(&mut f.body, cur, &mut locals, src)?;
        locals.exit();
        Ok(())
    }

    fn rewrite_typeref(
        &self,
        ty: &mut TypeRef,
        cur: &[String],
        src: &str,
    ) -> Result<(), CompileError> {
        match ty {
            TypeRef::Named(name) => {
                if let Some(canon) = self.scope_lookup(cur, name) {
                    *name = canon;
                }
            }
            TypeRef::Array { elem, len } => {
                self.rewrite_typeref(elem, cur, src)?;
                // The length is a const expression; resolve its const refs (and
                // surface any visibility/resolution error, with the right file).
                self.rewrite_expr(len, cur, &mut Locals::new(), src)?;
            }
            TypeRef::Tuple(ts) => {
                for t in ts {
                    self.rewrite_typeref(t, cur, src)?;
                }
            }
            TypeRef::Scalar(_) | TypeRef::Fixed { .. } => {}
        }
        Ok(())
    }

    fn rewrite_block(
        &self,
        b: &mut Block,
        cur: &[String],
        locals: &mut Locals,
        src: &str,
    ) -> Result<(), CompileError> {
        locals.enter();
        for stmt in &mut b.stmts {
            let Stmt::Let {
                pattern, ty, value, ..
            } = stmt;
            // The initializer is resolved BEFORE the bindings come into scope.
            self.rewrite_expr(value, cur, locals, src)?;
            if let Some(t) = ty {
                self.rewrite_typeref(t, cur, src)?;
            }
            bind_pattern(pattern, locals);
        }
        self.rewrite_expr(&mut b.tail, cur, locals, src)?;
        locals.exit();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn rewrite_expr(
        &self,
        e: &mut Expr,
        cur: &[String],
        locals: &mut Locals,
        src: &str,
    ) -> Result<(), CompileError> {
        match e {
            Expr::Literal(_) => {}
            Expr::Ident(name) => {
                if !locals.contains(name) {
                    if let Some(canon) = self.scope_lookup(cur, name) {
                        *name = canon;
                    }
                }
            }
            Expr::Unary { expr, .. } => self.rewrite_expr(expr, cur, locals, src)?,
            Expr::Binary { lhs, rhs, .. } => {
                self.rewrite_expr(lhs, cur, locals, src)?;
                self.rewrite_expr(rhs, cur, locals, src)?;
            }
            Expr::If { cond, then, els } => {
                self.rewrite_expr(cond, cur, locals, src)?;
                self.rewrite_expr(then, cur, locals, src)?;
                self.rewrite_expr(els, cur, locals, src)?;
            }
            Expr::Path(segs) => {
                let owned = segs.clone();
                *e = self.resolve_path(cur, &owned, src, Pos::default())?;
            }
            Expr::Field { base, .. } => self.rewrite_expr(base, cur, locals, src)?,
            Expr::Call { func, args } => {
                self.rewrite_callee(func, cur, locals, src)?;
                for a in args {
                    self.rewrite_expr(a, cur, locals, src)?;
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.rewrite_expr(receiver, cur, locals, src)?;
                for a in args {
                    self.rewrite_expr(a, cur, locals, src)?;
                }
            }
            Expr::Index { base, index } => {
                self.rewrite_expr(base, cur, locals, src)?;
                self.rewrite_expr(index, cur, locals, src)?;
            }
            Expr::StructLit { name, fields } => {
                if let Some(canon) = self.scope_lookup(cur, name) {
                    *name = canon;
                }
                for f in fields {
                    self.rewrite_expr(&mut f.value, cur, locals, src)?;
                }
            }
            Expr::Tuple(es) | Expr::Array(es) => {
                for x in es {
                    self.rewrite_expr(x, cur, locals, src)?;
                }
            }
            Expr::Cast { expr, ty, .. } => {
                self.rewrite_expr(expr, cur, locals, src)?;
                self.rewrite_typeref(ty, cur, src)?;
            }
            Expr::Match { scrutinee, arms } => {
                self.rewrite_expr(scrutinee, cur, locals, src)?;
                for arm in arms {
                    locals.enter();
                    self.rewrite_pattern(&mut arm.pattern, cur, locals, src)?;
                    self.rewrite_expr(&mut arm.body, cur, locals, src)?;
                    locals.exit();
                }
            }
            Expr::Block(b) => self.rewrite_block(b, cur, locals, src)?,
        }
        Ok(())
    }

    /// Rewrite a call target: a bare fn name (in scope, not shadowed by a local)
    /// or a qualified path.
    fn rewrite_callee(
        &self,
        func: &mut Expr,
        cur: &[String],
        locals: &Locals,
        src: &str,
    ) -> Result<(), CompileError> {
        match func {
            Expr::Ident(name) => {
                // A local of the same name shadows the function (it is then not
                // callable — F1 will diagnose it; we must not rebind to the fn).
                if !locals.contains(name) {
                    if let Some(canon) = self.scope_lookup(cur, name) {
                        *name = canon;
                    }
                }
                Ok(())
            }
            Expr::Path(segs) => {
                let owned = segs.clone();
                *func = self.resolve_path(cur, &owned, src, Pos::default())?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Resolve an expression `::`-path to a rewritten [`Expr`] — either an
    /// `Ident(canonical)` (a qualified value, e.g. a const) or a two-segment
    /// `Path([canonical_enum, variant])` (an enum variant).
    fn resolve_path(
        &self,
        cur: &[String],
        segs: &[String],
        src: &str,
        pos: Pos,
    ) -> Result<Expr, CompileError> {
        let q = split_qualifier(segs);
        match q.qualifier {
            PathQualifier::None => {
                // Bare path: only enum variants `Type::Variant` (Type in scope).
                if segs.len() == 2 {
                    if let Some(canon) = self.scope_lookup(cur, &segs[0]) {
                        return Ok(Expr::Path(vec![canon, segs[1].clone()]));
                    }
                }
                // Unresolved here — leave it for F1 to diagnose precisely.
                Ok(Expr::Path(segs.to_vec()))
            }
            qualifier => {
                let start = self.start_module(cur, qualifier, src)?;
                let (m, rest) = self.walk_modules(start, q.segments);
                match rest {
                    [item] => {
                        let canon = self.member(&m, item, cur, src, pos)?;
                        Ok(Expr::Ident(canon))
                    }
                    [ty, variant] => {
                        let canon = self.member(&m, ty, cur, src, pos)?;
                        Ok(Expr::Path(vec![canon, variant.clone()]))
                    }
                    _ => Err(CompileError::semantic(
                        src,
                        pos,
                        format!("unresolved path `{}`", segs.join("::")),
                    )),
                }
            }
        }
    }

    fn rewrite_pattern(
        &self,
        pat: &mut Pattern,
        cur: &[String],
        locals: &mut Locals,
        src: &str,
    ) -> Result<(), CompileError> {
        match pat {
            Pattern::Wildcard | Pattern::Literal(_) => {}
            Pattern::Binding(name) => locals.bind(name.clone()),
            Pattern::Path(segs) => {
                // An enum-variant pattern `Type::Variant`; rewrite the type part.
                if segs.len() == 2 {
                    if let Some(canon) = self.scope_lookup(cur, &segs[0]) {
                        segs[0] = canon;
                    }
                } else {
                    let resolved = self.resolve_path(cur, segs, src, Pos::default())?;
                    if let Expr::Path(new) = resolved {
                        *segs = new;
                    }
                }
            }
            Pattern::Tuple(ps) => {
                for p in ps {
                    self.rewrite_pattern(p, cur, locals, src)?;
                }
            }
        }
        Ok(())
    }
}

/// Whether an item of visibility `vis` defined in module `def_mod` is reachable
/// from module `from`: `pub`, or `from` is `def_mod` or a descendant of it.
fn accessible(vis: Visibility, def_mod: &[String], from: &[String]) -> bool {
    vis == Visibility::Public || from.starts_with(def_mod)
}

/// Whether an `impl` block carries the `#[clock]` transition root.
fn impl_has_clock(im: &ImplItem) -> bool {
    im.methods
        .iter()
        .any(|m| m.attrs.iter().any(|a| a.name == "clock"))
}

fn mod_label(path: &[String]) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.join("::")
    }
}

/// The name, visibility, and position of a named item.
fn item_head(item: &Item) -> (&str, Visibility, Pos) {
    match item {
        Item::Const(c) => (&c.name, c.vis, c.pos),
        Item::Struct(s) => (&s.name, s.vis, s.pos),
        Item::Enum(e) => (&e.name, e.vis, e.pos),
        Item::Func(f) => (&f.name, f.vis, f.pos),
        Item::Impl(im) => (&im.type_name, Visibility::Private, im.pos),
        Item::Mod(m) => (&m.name, m.vis, m.pos),
        Item::Use(u) => ("use", u.vis, u.pos),
    }
}

/// Add the bound names of a pattern to the current local scope.
fn bind_pattern(pat: &Pattern, locals: &mut Locals) {
    match pat {
        Pattern::Binding(name) => locals.bind(name.clone()),
        Pattern::Tuple(ps) => {
            for p in ps {
                bind_pattern(p, locals);
            }
        }
        Pattern::Wildcard | Pattern::Literal(_) | Pattern::Path(_) => {}
    }
}

/// A stack of lexical scopes holding bound local names (params, `let`s, match
/// bindings) so a local is never mistaken for a module-level reference.
struct Locals {
    scopes: Vec<Vec<String>>,
}

impl Locals {
    fn new() -> Self {
        Locals {
            scopes: vec![Vec::new()],
        }
    }
    fn enter(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn exit(&mut self) {
        self.scopes.pop();
    }
    fn bind(&mut self, name: String) {
        if let Some(top) = self.scopes.last_mut() {
            top.push(name);
        }
    }
    fn contains(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|s| s.iter().any(|n| n == name))
    }
}
