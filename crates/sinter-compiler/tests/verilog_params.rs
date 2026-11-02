//! EXT2 (DESIGN §9.15): `#[params(NAME = value, ...)]` on a `#[verilog]`
//! binding sets the module's Verilog parameters. The values render as Verilog
//! (`#(.NAME(value))`), a constant's value follows `-D`, and, with the tools
//! present, the oracle checks the reference model against the real module at
//! several parameter values (`examples/verilog_params/`).

#[path = "support/oracle.rs"]
mod oracle;

use std::path::{Path, PathBuf};

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference, stimulus_to_bytes};
use sinter_compiler::{compile_project_with, compile_str, verilator, verilog, Define, FaultMode};

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/verilog_params")
}

fn compile_with(root: &Path, specs: &[&str]) -> Module {
    let defines: Vec<Define> = specs
        .iter()
        .map(|s| Define::parse(s).expect("define"))
        .collect();
    compile_project_with(root, FaultMode::Off, &defines)
        .expect("compile")
        .modules
        .remove(0)
}

/// A combinational model bound to `m`, with `attrs` on the binding.
fn bound(attrs: &str) -> String {
    format!(
        "pub const W: u8 = 18;
         pub const NEG: i8 = -3;
         #[verilog(module = \"m\")]
         {attrs}
         struct M {{}}
         struct MOut {{ y: u8, }}
         impl M {{ fn step(self, a: u8) -> (M, MOut) {{ (self, MOut {{ y: a }}) }} }}
         struct T {{ pub r: u8 = 0, m: M, }}
         impl T {{
             #[clock(domain = sys)]
             fn tick(self, a: u8) -> T {{
                 let (_m, o) = self.m.step(a);
                 T {{ r: o.y, m: self.m }}
             }}
         }}"
    )
}

fn params_of(src: &str) -> Vec<(String, String)> {
    compile_str(src).expect("compile").modules[0].instances[0]
        .params
        .clone()
}

fn compile_err(src: &str, needle: &str) {
    let msg = compile_str(src).expect_err("expected an error").to_string();
    assert!(
        msg.contains(needle),
        "error did not contain {needle:?}.\n  got: {msg}"
    );
}

#[test]
fn values_render_as_verilog() {
    let p = params_of(&bound(
        "#[params(WIDTH = 18, MODE = \"CLK0\", BIG = 0x1_0000_0000, FROM = W, OFFSET = NEG)]",
    ));
    let p: Vec<(&str, &str)> = p.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    assert_eq!(
        p,
        [
            ("WIDTH", "18"),
            ("MODE", "\"CLK0\""),
            ("BIG", "64'd4294967296"),
            ("FROM", "18"),
            ("OFFSET", "-3"),
        ]
    );
}

#[test]
fn the_instance_gets_named_overrides() {
    let design = compile_str(&bound("#[params(WIDTH = 18, MODE = \"CLK0\")]")).expect("compile");
    let sv = verilog::emit_module(&design.modules[0]);
    assert!(
        sv.contains("    m #(\n        .WIDTH (18),\n        .MODE  (\"CLK0\")\n    ) u_m_0 ("),
        "{sv}"
    );
}

#[test]
fn without_params_the_instance_is_unchanged() {
    let design = compile_str(&bound("")).expect("compile");
    let sv = verilog::emit_module(&design.modules[0]);
    assert!(sv.contains("    m u_m_0 ("), "{sv}");
}

#[test]
fn params_need_a_verilog_binding() {
    compile_err(
        "#[params(N = 1)]
         struct S { r: u8 = 0, }
         impl S { #[clock(domain = sys)] fn tick(self) -> S { self } }",
        "`#[params(..)]` sets a `#[verilog]` module's parameters; this struct isn't bound to one",
    );
}

#[test]
fn a_param_needs_a_value() {
    compile_err(
        &bound("#[params(WIDTH)]"),
        "`#[params]` argument `WIDTH` needs a value",
    );
}

#[test]
fn a_param_is_set_once() {
    compile_err(
        &bound("#[params(N = 1, N = 2)]"),
        "`#[params]` sets `N` twice",
    );
}

#[test]
fn a_named_value_must_be_an_integer_constant() {
    compile_err(
        &bound("#[params(N = NOPE)]"),
        "`#[params]` value of `N`: `NOPE` names no integer constant",
    );
}

#[test]
fn a_constant_param_follows_its_override() {
    let root = example().join("main.snt");
    for (specs, stages) in [(&[][..], "2"), (&["STAGES=3"][..], "3")] {
        let m = compile_with(&root, specs);
        assert_eq!(
            m.instances[0].params,
            [("STAGES".to_string(), stages.to_string())]
        );
    }
}

/// The model and the real `mul_pipe` agree at 1, 2, and 3 stages.
#[test]
fn the_model_matches_the_module_at_each_parameter_value() {
    let tools = oracle::tools();
    let root = example().join("main.snt");
    for stages in 1..=3 {
        let m = compile_with(&root, &[&format!("STAGES={stages}")]);
        let stimulus = gen_stimulus(&m, 64, 0xDEAD_BEEF);
        oracle::co_simulate(
            &tools,
            &format!("verilog_params-{stages}"),
            &m,
            &[],
            &stimulus,
            oracle::Lint::Strict,
        );
    }
}

/// A model that ignores its parameter fails the oracle: with the Verilog's
/// `STAGES` fixed at 1, the 3-stage model's trace differs.
#[test]
fn a_model_that_ignores_the_parameter_fails_the_oracle() {
    if !oracle::tools().verilator {
        return;
    }
    let dir = oracle::workdir("verilog_params-wrong");
    std::fs::create_dir_all(dir.join("rtl")).expect("create rtl dir");
    std::fs::copy(example().join("rtl/mul_pipe.v"), dir.join("rtl/mul_pipe.v")).expect("copy rtl");
    let main = std::fs::read_to_string(example().join("main.snt")).expect("read main.snt");
    let wrong = main.replace("#[params(STAGES = STAGES)]", "#[params(STAGES = 1)]");
    assert_ne!(main, wrong, "the mutation must apply");
    std::fs::write(dir.join("main.snt"), wrong).expect("write main.snt");

    let m = compile_with(&dir.join("main.snt"), &["STAGES=3"]);
    let sv = dir.join(format!("{}.sv", m.name));
    std::fs::write(&sv, verilog::emit_module(&m)).expect("write sv");
    let harness = dir.join(format!("{}_tb.cpp", m.name));
    std::fs::write(&harness, verilator::emit_cpp_harness(&m)).expect("write harness");
    let stimulus = gen_stimulus(&m, 64, 0xDEAD_BEEF);
    let stim_path = dir.join("stimulus.bin");
    std::fs::write(&stim_path, stimulus_to_bytes(&stimulus)).expect("write stimulus");
    let dut = oracle::run_verilator(
        &dir,
        &sv,
        &oracle::foreign_sources(&m),
        &harness,
        &m.name,
        &stim_path,
        64,
    );
    assert_ne!(
        run_reference(&m, &stimulus),
        dut,
        "a model that ignores the parameter must disagree with the module"
    );
}
