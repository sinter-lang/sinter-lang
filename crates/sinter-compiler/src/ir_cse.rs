//! IR-level common-subexpression elimination for multiplies (DESIGN §3.2).
//!
//! The multiplex desugar ([`crate::multiplex_desugar`]) emits a single shared
//! `let shared = … * …;`, but the functional elaborator inlines every `let` by
//! value, so each *read* of `shared` duplicates the whole multiply subtree — one
//! source multiply would become N physical multipliers. This pass restores the
//! one-multiplier guarantee at the IR level: it hoists each *distinct*
//! [`ir::ExprKind::Binary`] multiply into one [`ir::Let`] (a single wire, emitted
//! once by the Verilog backend and evaluated once by the simulator) and rewrites
//! every occurrence to reference it by name. It is behaviour-preserving — the
//! lets are evaluated in topological order — so the sim↔Verilog trace is
//! unchanged; only the multiply *count* drops.
//!
//! It is run only for designs that contain a `multiplex fn` (so other designs'
//! lowerings stay byte-identical), but it is generic over the module.

use std::collections::HashMap;

use sinter_syntax::ast::{BinOp, Type};

use crate::ir;

/// Hoist every distinct multiply in `m` into a shared `ir::Let`, so each becomes
/// exactly one physical multiplier.
pub fn share_muls(m: &mut ir::Module) {
    let mut cse = Cse {
        seen: HashMap::new(),
        hoisted: Vec::new(),
        next: 0,
    };
    // Visit the existing wires in order, placing each wire's newly hoisted
    // multiplies just before it, so `m.lets` stays topologically ordered (a
    // hoisted subtree reads only signals defined before the wire it came from).
    // A `#[verilog]` model's wires are left alone: hoisting out of them would
    // put model-only logic in the emitted Verilog (DESIGN §6.15).
    let existing = std::mem::take(&mut m.lets);
    for mut l in existing {
        let model = m.is_model_only(&l.name) || m.instance_output(&l.name).is_some();
        if !model {
            cse.visit(&mut l.value);
        }
        m.lets.append(&mut cse.hoisted);
        m.lets.push(l);
    }
    for (name, e) in &mut m.reg_updates {
        if !m.instances.iter().any(|i| i.model_regs.contains(name)) {
            cse.visit(e);
        }
    }
    for (_, e) in &mut m.out_drives {
        cse.visit(e);
    }
    // Multiplies hoisted from next-states and outputs read only wires, so they
    // go after every existing wire.
    m.lets.append(&mut cse.hoisted);
}

struct Cse {
    /// Canonical key of a hoisted multiply → its wire name.
    seen: HashMap<String, String>,
    hoisted: Vec<ir::Let>,
    next: usize,
}

impl Cse {
    /// Bottom-up: CSE the children, then — if this node is a multiply — hoist its
    /// non-leaf operands and itself into shared wires.
    fn visit(&mut self, e: &mut ir::Expr) {
        match &mut e.kind {
            ir::ExprKind::Unary { expr, .. }
            | ir::ExprKind::Cast { expr, .. }
            | ir::ExprKind::Shl { expr, .. }
            | ir::ExprKind::Shr { expr, .. } => self.visit(expr),
            ir::ExprKind::Binary { lhs, rhs, .. } => {
                self.visit(lhs);
                self.visit(rhs);
            }
            ir::ExprKind::VarShift { expr, amount, .. } => {
                self.visit(expr);
                self.visit(amount);
            }
            ir::ExprKind::If { cond, then, els } => {
                self.visit(cond);
                self.visit(then);
                self.visit(els);
            }
            ir::ExprKind::Literal(_) | ir::ExprKind::Ident(_) => {}
        }
        if matches!(e.kind, ir::ExprKind::Binary { op: BinOp::Mul, .. }) {
            // Hoist non-leaf operands first: the Verilog backend widens a multiply
            // operand with `W'(…)`, and `W'(<ternary>)` trips Verilator's
            // WIDTHEXPAND (the mux arms are context-widened); `W'(<wire>)` does not.
            if let ir::ExprKind::Binary { lhs, rhs, .. } = &mut e.kind {
                self.hoist_operand(lhs);
                self.hoist_operand(rhs);
            }
            let name = self.intern(e, "__mul");
            e.kind = ir::ExprKind::Ident(name);
        }
    }

    /// Hoist a multiply operand into its own wire unless it is already a leaf
    /// (a `VARREF`/literal the backend can widen directly).
    fn hoist_operand(&mut self, e: &mut ir::Expr) {
        if matches!(e.kind, ir::ExprKind::Literal(_) | ir::ExprKind::Ident(_)) {
            return;
        }
        let name = self.intern(e, "__mop");
        e.kind = ir::ExprKind::Ident(name);
    }

    /// Intern `e` as a shared wire (reusing one for an identical subtree), pushing
    /// the `ir::Let` inner-first so `m.lets` stays topologically ordered.
    fn intern(&mut self, e: &ir::Expr, prefix: &str) -> String {
        let k = key(e);
        self.seen.get(&k).cloned().unwrap_or_else(|| {
            let name = format!("{prefix}{}", self.next);
            self.next += 1;
            self.hoisted.push(ir::Let {
                name: name.clone(),
                ty: e.ty,
                frac_bits: 0,
                value: e.clone(),
            });
            self.seen.insert(k, name.clone());
            name
        })
    }
}

/// A canonical structural key for an IR expression (children already CSE'd), so
/// two identical multiply subtrees share one wire.
fn key(e: &ir::Expr) -> String {
    match &e.kind {
        ir::ExprKind::Literal(v) => format!("L{v}:{}", tystr(e.ty)),
        ir::ExprKind::Ident(n) => format!("I{n}:{}", tystr(e.ty)),
        ir::ExprKind::Unary { op, expr } => format!("U{op:?}({})", key(expr)),
        ir::ExprKind::Binary { op, lhs, rhs } => {
            format!("B{op:?}({},{})", key(lhs), key(rhs))
        }
        ir::ExprKind::If { cond, then, els } => {
            format!("F({},{},{})", key(cond), key(then), key(els))
        }
        ir::ExprKind::Cast { expr, mode } => format!("C{mode:?}:{}({})", tystr(e.ty), key(expr)),
        ir::ExprKind::Shl { expr, amount } => format!("SL{amount}({})", key(expr)),
        ir::ExprKind::Shr { expr, amount } => format!("SR{amount}({})", key(expr)),
        ir::ExprKind::VarShift { expr, amount, left } => {
            format!("VS{left}({},{})", key(expr), key(amount))
        }
    }
}

fn tystr(t: Type) -> String {
    format!("{t}")
}
