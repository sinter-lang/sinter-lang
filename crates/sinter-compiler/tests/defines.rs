//! DEF1 build-time constant overrides (DESIGN §9.15): `-D NAME=VALUE`
//! replaces an integer `pub const` before folding, so the design is
//! specialized to the new value; with the tools present, both values of
//! `fn_params.snt` pass the oracle.

#[path = "support/oracle.rs"]
mod oracle;

use std::path::Path;

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, Simulator};
use sinter_compiler::{compile_project_with, compile_str_with, Define, FaultMode};

const PARAMS: &str = include_str!("../../../examples/fn_params.snt");

fn defs(specs: &[&str]) -> Vec<Define> {
    specs
        .iter()
        .map(|s| Define::parse(s).expect("define"))
        .collect()
}

fn module_with(src: &str, specs: &[&str]) -> Module {
    compile_str_with(src, FaultMode::Off, &defs(specs))
        .expect("compile")
        .modules
        .into_iter()
        .next()
        .expect("one module")
}

fn err_with(src: &str, specs: &[&str]) -> String {
    compile_str_with(src, FaultMode::Off, &defs(specs))
        .expect_err("expected an error")
        .to_string()
}

fn lanes(m: &Module) -> usize {
    m.regs
        .iter()
        .filter(|r| r.name.starts_with("lanes_"))
        .count()
}

#[test]
fn an_override_reshapes_the_design() {
    let base = module_with(PARAMS, &[]);
    assert_eq!((lanes(&base), base.mems[0].depth), (4, 16));
    let wide = module_with(PARAMS, &["LANES=8", "DEPTH=64"]);
    assert_eq!((lanes(&wide), wide.mems[0].depth), (8, 64));
}

#[test]
fn constants_computed_from_an_override_follow_it() {
    // `LAST = LANES - 1`: with 8 lanes the pointer wraps after lane 7.
    let m = module_with(PARAMS, &["LANES=8"]);
    let mut sim = Simulator::new(&m);
    let ptrs: Vec<u64> = (0..10)
        .map(|_| {
            sim.step(&Default::default());
            sim.signal("ptr").expect("ptr")
        })
        .collect();
    assert_eq!(ptrs, [0, 1, 2, 3, 4, 5, 6, 7, 0, 1]);
}

#[test]
fn the_last_override_of_a_name_wins() {
    let m = module_with(PARAMS, &["LANES=2", "LANES=6"]);
    assert_eq!(lanes(&m), 6);
}

#[test]
fn values_may_be_hex_binary_or_signed() {
    let src = "pub const A: u8 = 0; pub const B: i8 = 0;
               struct T { pub a: u8 = A, pub b: i8 = B, }
               impl T { #[clock(domain = sys)] fn tick(self) -> T { self } }";
    let m = module_with(src, &["A=0x1_f", "B=-3"]);
    let reset: Vec<u64> = m.regs.iter().map(|r| r.reset).collect();
    assert_eq!(reset, [0x1f, 0xfd]);
    let m = module_with(src, &["A=0b101"]);
    assert_eq!(m.regs[0].reset, 5);
}

#[test]
fn an_unknown_name_lists_the_parameters() {
    let err = err_with(PARAMS, &["LANE=8"]);
    assert_eq!(
        err,
        "-D LANE=8: no constant `LANE`; the integer `pub const`s are `DEPTH`, `LANES`, `STEP`"
    );
}

#[test]
fn a_private_constant_is_rejected() {
    let err = err_with(PARAMS, &["LAST=2"]);
    assert!(err.contains("`LAST` isn't `pub`"), "{err}");
}

#[test]
fn a_value_that_doesnt_fit_is_rejected() {
    let err = err_with(PARAMS, &["LANES=256"]);
    assert!(
        err.contains("256 doesn't fit `LANES`'s type `u8` (0 to 255)"),
        "{err}"
    );
    let err = err_with(PARAMS, &["LANES=-1"]);
    assert!(err.contains("doesn't fit"), "{err}");
}

#[test]
fn a_malformed_value_or_spec_is_rejected() {
    let err = err_with(PARAMS, &["LANES=four"]);
    assert!(err.contains("`four` isn't an integer"), "{err}");
    let err = Define::parse("LANES").expect_err("no =").to_string();
    assert_eq!(err, "-D LANES: expected `NAME=VALUE`, such as `N_MUL=2`");
    let err = Define::parse("=4").expect_err("no name").to_string();
    assert!(err.contains("expected `NAME=VALUE`"), "{err}");
}

#[test]
fn an_override_that_breaks_the_design_is_reported_by_the_checks() {
    // A non-power-of-two memory depth is the memory's own error.
    let err = err_with(PARAMS, &["DEPTH=10"]);
    assert!(err.contains("power of two"), "{err}");
}

#[test]
fn a_defines_file_has_one_override_per_line() {
    let ds =
        Define::parse_file("# widths\nLANES = 8\n\nDEPTH=64  # deeper\n", "p.defs").expect("parse");
    assert_eq!(
        ds.iter()
            .map(|d| (d.name.as_str(), d.value.as_str()))
            .collect::<Vec<_>>(),
        [("LANES", "8"), ("DEPTH", "64")]
    );
    assert_eq!(ds[1].origin, "p.defs:4");
    let err = Define::parse_file("LANES 8\n", "p.defs")
        .expect_err("bad line")
        .to_string();
    assert!(err.starts_with("p.defs:1: expected `NAME=VALUE`"), "{err}");
}

#[test]
fn a_submodule_constant_is_named_by_its_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/defines_sub/main.snt");
    let m = |specs: &[&str]| {
        compile_project_with(&root, FaultMode::Off, &defs(specs))
            .expect("compile")
            .modules
            .remove(0)
    };
    let module = m(&["setup::N=42"]);
    let mut sim = Simulator::new(&module);
    sim.step(&Default::default());
    assert_eq!(sim.step(&Default::default())["v_q"], 42);
    let err = compile_project_with(&root, FaultMode::Off, &defs(&["N=42"]))
        .expect_err("bare name")
        .to_string();
    assert!(
        err.contains("no constant `N`; the integer `pub const`s are `setup::N`"),
        "{err}"
    );
}

/// Both parameter sets of `fn_params.snt` synthesize and co-simulate. The
/// lint is `Lint::Design`: the `u8` address can't narrow with `DEPTH` (a type
/// can't take a constant), so a shallow memory leaves high address bits unread.
#[test]
fn both_parameter_sets_pass_the_oracle() {
    let tools = oracle::tools();
    for (name, specs) in [
        ("defines-base", &[][..]),
        ("defines-wide", &["LANES=8", "DEPTH=64", "STEP=5"][..]),
    ] {
        let m = module_with(PARAMS, specs);
        let stimulus = gen_stimulus(&m, 96, 11);
        oracle::co_simulate(&tools, name, &m, &[], &stimulus, oracle::Lint::Design);
    }
}
