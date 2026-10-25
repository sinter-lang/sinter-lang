//! A generator of random, well-typed Sinter programs for the fuzzer.
//!
//! Generation is type-directed: [`Gen::expr`] builds an expression of exactly
//! the type it is asked for, inserting a `wrap` or `sat` cast wherever an
//! operator's result type differs, so every program should type-check. One
//! that doesn't is a bug in this generator or in the compiler.
//!
//! A program is one state struct with scalar fields, an optional register file
//! (`[uN; M]`) and memory (`mem<uN, D>`), up to two helper `fn`s, and one
//! `#[clock]` tick with inputs, `let` wires, and extra outputs. Expressions mix
//! every scalar operator: arithmetic, bitwise, comparisons, `if`, unary ops,
//! casts, literal and runtime shifts, slices, concatenation, helper calls, and
//! register-file and memory reads. Widths favour the edges (1, 63, 64, and
//! either side of a byte).

use std::fmt;
use std::fmt::Write as _;

/// A small deterministic PRNG (SplitMix64).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// True with probability `pct`%.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// A scalar type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ty {
    pub signed: bool,
    pub w: u32,
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", if self.signed { "i" } else { "u" }, self.w)
    }
}

const fn u(w: u32) -> Ty {
    Ty { signed: false, w }
}

/// Widths, weighted toward the interesting ones.
const WIDTHS: &[u32] = &[
    1, 1, 2, 3, 4, 5, 7, 8, 8, 9, 12, 15, 16, 17, 24, 31, 32, 33, 48, 63, 64,
];

struct Func {
    name: String,
    params: Vec<Ty>,
    ret: Ty,
}

struct Gen {
    rng: Rng,
    /// Readable scalars in scope: their source text and type.
    vars: Vec<(String, Ty)>,
    funcs: Vec<Func>,
    /// The register file's element type and index width, if any.
    rf: Option<(Ty, u32)>,
    /// The memory's element type and address width, if any.
    mem: Option<(Ty, u32)>,
}

/// A random program for `seed`.
pub fn program(seed: u64) -> String {
    let mut g = Gen {
        rng: Rng::new(seed),
        vars: Vec::new(),
        funcs: Vec::new(),
        rf: None,
        mem: None,
    };
    g.program(seed)
}

impl Gen {
    fn ty(&mut self) -> Ty {
        let w = *self.rng.pick(WIDTHS);
        // `iN` needs a sign bit plus at least one value bit to be useful.
        Ty {
            signed: w >= 2 && self.rng.chance(40),
            w,
        }
    }

    fn uty(&mut self) -> Ty {
        u(*self.rng.pick(WIDTHS))
    }

    /// A literal of type `t`, as a bit pattern with a width suffix.
    fn lit(&mut self, t: Ty) -> String {
        let all = if t.w >= 64 {
            u64::MAX
        } else {
            (1u64 << t.w) - 1
        };
        let bits = match self.rng.below(6) {
            0 => 0,
            1 => 1,
            2 => all,
            // The sign bit alone: the most negative value of a signed type.
            3 => 1u64 << (t.w - 1),
            _ => self.rng.next() & all,
        };
        format!("0x{bits:x}_{t}")
    }

    fn leaf(&mut self, t: Ty) -> String {
        let exact: Vec<String> = self
            .vars
            .iter()
            .filter(|(_, vt)| *vt == t)
            .map(|(n, _)| n.clone())
            .collect();
        if !exact.is_empty() && self.rng.chance(70) {
            return self.rng.pick(&exact).clone();
        }
        if !self.vars.is_empty() && self.rng.chance(60) {
            let (n, _) = self.rng.pick(&self.vars).clone();
            return self.cast(n, t);
        }
        self.lit(t)
    }

    /// `e` (of some other type) as type `t`.
    fn cast(&mut self, e: String, t: Ty) -> String {
        let mode = if self.rng.chance(70) { "wrap" } else { "sat" };
        format!("({e} as {t} {mode})")
    }

    /// `e` of type `from` as type `t`: unchanged when the types already match.
    fn fit(&mut self, e: String, from: Ty, t: Ty) -> String {
        if from == t {
            e
        } else {
            self.cast(e, t)
        }
    }

    /// An expression of exactly type `t`, at most `depth` operators deep.
    fn expr(&mut self, t: Ty, depth: u32) -> String {
        if depth == 0 || self.rng.chance(20) {
            return self.leaf(t);
        }
        let d = depth - 1;
        match self.rng.below(15) {
            0 => {
                let s = self.ty();
                let e = self.expr(s, d);
                self.fit(e, s, t)
            }
            1 => {
                let e = self.expr(t, d);
                if t == u(1) && self.rng.chance(50) {
                    format!("(!{e})")
                } else if t.signed && self.rng.chance(50) {
                    format!("(-{e})")
                } else {
                    format!("(~{e})")
                }
            }
            2 | 3 => self.arith(t, d),
            4 => {
                let op = *self.rng.pick(&["&", "|", "^"]);
                let (a, b) = (self.expr(t, d), self.expr(t, d));
                format!("({a} {op} {b})")
            }
            5 => {
                let s = self.ty();
                let op = *self.rng.pick(&["==", "!=", "<", ">", "<=", ">="]);
                let (a, b) = (self.expr(s, d), self.expr(s, d));
                self.fit(format!("({a} {op} {b})"), u(1), t)
            }
            6 => {
                let c = self.expr(u(1), d);
                let (a, b) = (self.expr(t, d), self.expr(t, d));
                format!("(if {c} == 1 {{ {a} }} else {{ {b} }})")
            }
            7 => self.const_shift(t, d),
            8 => {
                let amount = u(1 + self.rng.below(7) as u32);
                let op = if self.rng.chance(50) { "<<" } else { ">>" };
                let (a, k) = (self.expr(t, d), self.expr(amount, d));
                format!("({a} {op} {k})")
            }
            9 => {
                let s = self.uty();
                let lo = self.rng.below(u64::from(s.w)) as u32;
                let hi = lo + self.rng.below(u64::from(s.w - lo)) as u32;
                let e = self.expr(s, d);
                self.fit(format!("{e}[{hi}:{lo}]"), u(hi - lo + 1), t)
            }
            10 => {
                let wa = 1 + self.rng.below(32) as u32;
                let wb = 1 + self.rng.below(u64::from(64 - wa).min(32)) as u32;
                let (a, b) = (self.expr(u(wa), d), self.expr(u(wb), d));
                self.fit(format!("({a} @ {b})"), u(wa + wb), t)
            }
            11 if !self.funcs.is_empty() => {
                let k = self.rng.below(self.funcs.len() as u64) as usize;
                let (name, params, ret) = {
                    let f = &self.funcs[k];
                    (f.name.clone(), f.params.clone(), f.ret)
                };
                let args: Vec<String> = params.iter().map(|&p| self.expr(p, d)).collect();
                self.fit(format!("{name}({})", args.join(", ")), ret, t)
            }
            12 if self.rf.is_some() => {
                let (elem, ib) = self.rf.unwrap_or((u(1), 1));
                let i = self.expr(u(ib), d);
                self.fit(format!("self.rf[{i}]"), elem, t)
            }
            13 if self.mem.is_some() => {
                let (elem, _) = self.mem.unwrap_or((u(1), 1));
                self.fit("self.m.value()".to_string(), elem, t)
            }
            _ => self.leaf(t),
        }
    }

    /// `+`, `-`, or `*` over operands sized so the result fits 64 bits.
    fn arith(&mut self, t: Ty, d: u32) -> String {
        let signed = if self.rng.chance(70) {
            t.signed
        } else {
            self.rng.chance(50)
        };
        let min = if signed { 2 } else { 1 };
        let op = *self.rng.pick(&["+", "-", "*"]);
        let (wa, wb, w) = if op == "*" {
            let wa = min + self.rng.below(u64::from(64 - 2 * min).min(40)) as u32;
            let wb = min + self.rng.below(u64::from(64 - wa - min + 1)) as u32;
            (wa, wb, wa + wb)
        } else {
            let wa = min + self.rng.below(u64::from(63 - min + 1)) as u32;
            let wb = min + self.rng.below(u64::from(63 - min + 1)) as u32;
            (wa, wb, wa.max(wb) + 1)
        };
        let (ta, tb) = (Ty { signed, w: wa }, Ty { signed, w: wb });
        let (a, b) = (self.expr(ta, d), self.expr(tb, d));
        self.fit(format!("({a} {op} {b})"), Ty { signed, w }, t)
    }

    /// `x << k` (widening) or `x >> k` (narrowing) by a literal `k`.
    fn const_shift(&mut self, t: Ty, d: u32) -> String {
        let s = self.ty();
        if self.rng.chance(50) && s.w < 64 {
            let k = self.rng.below(u64::from(64 - s.w) + 1) as u32;
            let e = self.expr(s, d);
            self.fit(format!("({e} << {k})"), Ty { w: s.w + k, ..s }, t)
        } else if s.w > 1 {
            let k = self.rng.below(u64::from(s.w)) as u32;
            let e = self.expr(s, d);
            self.fit(format!("({e} >> {k})"), Ty { w: s.w - k, ..s }, t)
        } else {
            self.leaf(t)
        }
    }

    fn program(&mut self, seed: u64) -> String {
        let mut out = format!("// Fuzzer program, seed {seed}.\n\n");

        // Helpers see only their own parameters (and earlier helpers).
        for k in 0..self.rng.below(3) {
            let params: Vec<Ty> = (0..1 + self.rng.below(3)).map(|_| self.ty()).collect();
            let ret = self.ty();
            self.vars = params
                .iter()
                .enumerate()
                .map(|(i, &t)| (format!("p{i}"), t))
                .collect();
            let body = self.expr(ret, 3);
            let sig: Vec<String> = params
                .iter()
                .enumerate()
                .map(|(i, t)| format!("p{i}: {t}"))
                .collect();
            let _ = writeln!(
                out,
                "fn h{k}({}) -> {ret} {{\n    {body}\n}}\n",
                sig.join(", ")
            );
            self.funcs.push(Func {
                name: format!("h{k}"),
                params,
                ret,
            });
        }

        // State.
        let fields: Vec<(Ty, bool)> = (0..1 + self.rng.below(5))
            .map(|_| (self.ty(), self.rng.chance(75)))
            .collect();
        if self.rng.chance(35) {
            let ib = 1 + self.rng.below(3) as u32;
            self.rf = Some((self.uty(), ib));
        }
        if self.rng.chance(30) {
            let ab = 1 + self.rng.below(5) as u32;
            self.mem = Some((self.uty(), ab));
        }
        out.push_str("struct Top {\n");
        for (k, &(t, public)) in fields.iter().enumerate() {
            let reset = self.lit(t);
            let vis = if public { "pub " } else { "" };
            let _ = writeln!(out, "    {vis}f{k}: {t} = {reset},");
        }
        if let Some((elem, ib)) = self.rf {
            let n = 1u32 << ib;
            let init: Vec<String> = (0..n).map(|_| self.lit(elem)).collect();
            let vis = if self.rng.chance(50) { "pub " } else { "" };
            let _ = writeln!(out, "    {vis}rf: [{elem}; {n}] = [{}],", init.join(", "));
        }
        if let Some((elem, ab)) = self.mem {
            let _ = writeln!(out, "    m: mem<{elem}, {}> = 0,", 1u32 << ab);
        }
        out.push_str("}\n\n");

        // The tick.
        let inputs: Vec<Ty> = (0..self.rng.below(5)).map(|_| self.ty()).collect();
        self.vars = inputs
            .iter()
            .enumerate()
            .map(|(k, &t)| (format!("i{k}"), t))
            .chain(
                fields
                    .iter()
                    .enumerate()
                    .map(|(k, &(t, _))| (format!("self.f{k}"), t)),
            )
            .collect();
        let extra: Vec<Ty> = (0..self.rng.below(3)).map(|_| self.ty()).collect();
        let ret = match extra.as_slice() {
            [] => "Top".to_string(),
            [a] => format!("(Top, {a})"),
            [a, b] => format!("(Top, ({a}, {b}))"),
            _ => unreachable!("at most two extra outputs"),
        };
        let params: Vec<String> = inputs
            .iter()
            .enumerate()
            .map(|(k, t)| format!("i{k}: {t}"))
            .collect();
        let sep = if params.is_empty() { "" } else { ", " };
        let _ = writeln!(
            out,
            "impl Top {{\n    #[clock(domain = sys)]\n    fn tick(self{sep}{}) -> {ret} {{",
            params.join(", ")
        );
        for k in 0..self.rng.below(6) {
            let t = self.ty();
            let e = self.expr(t, 4);
            let _ = writeln!(out, "        let l{k}: {t} = {e};");
            self.vars.push((format!("l{k}"), t));
        }
        let mut inits: Vec<String> = Vec::new();
        for (k, &(t, _)) in fields.iter().enumerate() {
            let e = self.expr(t, 4);
            inits.push(format!("f{k}: {e}"));
        }
        if let Some((elem, ib)) = self.rf {
            let (i, v) = (self.expr(u(ib), 3), self.expr(elem, 3));
            let set = format!("self.rf.set({i}, {v})");
            inits.push(if self.rng.chance(50) {
                let c = self.expr(u(1), 2);
                format!("rf: if {c} == 1 {{ {set} }} else {{ self.rf }}")
            } else {
                format!("rf: {set}")
            });
        }
        if let Some((elem, ab)) = self.mem {
            let ra = self.expr(u(ab), 3);
            let we = self.expr(u(1), 2);
            let wa = self.expr(u(ab), 3);
            let wd = self.expr(elem, 3);
            inits.push(format!("m: self.m.next({ra}, {we}, {wa}, {wd})"));
        }
        let state = format!("Top {{ {} }}", inits.join(", "));
        let outs: Vec<String> = extra.iter().map(|&t| self.expr(t, 4)).collect();
        let tail = match outs.as_slice() {
            [] => state,
            [a] => format!("({state}, {a})"),
            [a, b] => format!("({state}, ({a}, {b}))"),
            _ => unreachable!("at most two extra outputs"),
        };
        let _ = writeln!(out, "        {tail}\n    }}\n}}");
        out
    }
}
