//! Hierarchy: a `#[module]` struct becomes its own Verilog module, which its
//! parent instantiates. The oracle co-simulates `examples/fn_hier.snt`, the flat
//! simulator against the hierarchical Verilog.

use sinter_compiler::{compile_str, compile_str_faults, verilog, FaultMode};

const HIER: &str = include_str!("../../../examples/fn_hier.snt");

fn compile_err(src: &str, needle: &str) {
    match compile_str(src) {
        Ok(_) => panic!("expected an error containing {needle:?}, but it compiled"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(needle),
                "error did not contain {needle:?}.\n  got: {msg}"
            );
        }
    }
}

/// A `#[module]` counter and a parent `tick` body around it.
fn with_counter(tick_body: &str) -> String {
    format!(
        "#[module]
         struct Cnt {{ n: u8 = 0 }}
         struct CntOut {{ n: u8 }}
         impl Cnt {{
             fn peek(self) -> u8 {{ self.n }}
             fn step(self, en: u1) -> (Self, CntOut) {{
                 (Self {{ n: if en == 1 {{ (self.n + 1) as u8 wrap }} else {{ self.n }} }}, CntOut {{ n: self.n }})
             }}
         }}
         struct Top {{ c: Cnt, pub v: u8 = 0 }}
         impl Top {{
             #[clock(domain = sys)]
             fn tick(self, en: u1) -> Top {{ {tick_body} }}
         }}"
    )
}

#[test]
fn each_module_struct_is_its_own_verilog_module() {
    let design = compile_str(HIER).expect("compiles");
    let names: Vec<&str> = design.modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        ["Top", "Pair", "Delay"],
        "the top first, each child once"
    );

    let top = verilog::emit_module(&design.modules[0]);
    assert!(top.contains("Pair u_pair ("), "{top}");
    // The top's simulator inlines the children; its Verilog doesn't.
    assert!(
        !design.modules[0].mems.is_empty(),
        "the flat model holds the RAMs"
    );
    assert!(!top.contains("logic [7:0] pair_first_ram"), "{top}");

    let pair = verilog::emit_module(&design.modules[1]);
    assert!(pair.contains("Delay u_first (") && pair.contains("Delay u_second ("));
    // Ports follow the top-module rules: parameters in, `out_<field>` out.
    assert!(pair.contains("input  wire  [7:0] s_v"), "{pair}");
    assert!(pair.contains("output logic [7:0] out_taps_1"), "{pair}");

    let delay = verilog::emit_module(&design.modules[2]);
    assert!(delay.contains("logic [7:0] ram [0:15];"), "{delay}");
}

#[test]
fn a_child_steps_like_a_foreign_instance() {
    compile_str(&with_counter(
        "let (c, o) = self.c.step(en); Top { c: c, v: o.n }",
    ))
    .expect("stepped once, its outputs read");
    compile_err(
        &with_counter("let (c, o) = self.c.step(en); Top { c: c, v: c.n }"),
        "internal to the `#[module]` `Cnt`",
    );
    compile_err(
        &with_counter("Top { c: self.c, v: self.c.peek() }"),
        "only its step method",
    );
    compile_err(
        &with_counter("Top { c: self.c, v: 0 }"),
        "`#[module]` instance `self.c` (`Cnt`) is never stepped",
    );
    compile_err(
        &with_counter(
            "if en == 1 { let (c, o) = self.c.step(en); Top { c: c, v: o.n } } else { self }",
        ),
        "must be stepped every cycle",
    );
}

#[test]
fn module_shape_rejections() {
    compile_err(
        "#[module] #[verilog(module = \"m\")]
         struct A { n: u8 = 0 }
         struct Top { pub v: u8 = 0 }
         impl Top { #[clock(domain = sys)] fn tick(self) -> Top { self } }",
        "either `#[module]`",
    );
    compile_err(
        "#[module]
         struct A { n: u8 = 0 }
         impl A { fn step(self, x: u8) -> (Self, u8) { (Self { n: x }, self.n) } }
         struct Top { a: A, pub v: u8 = 0 }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> Top { let (a, o) = self.a.step(x); Top { a: a, v: o } }
         }",
        "`Out` a struct",
    );
}

#[test]
fn faults_dont_cover_module_instances_yet() {
    match compile_str_faults(HIER, FaultMode::On) {
        Ok(_) => panic!("`--faults` with `#[module]` instances should be rejected"),
        Err(e) => assert!(e.to_string().contains("doesn't support `#[module]`"), "{e}"),
    }
}
