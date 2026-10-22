//! S3: multi-file project end-to-end + refactor-invariance (DESIGN §10).
//!
//! The headline acceptance for the module system: the Go Board example split
//! across files must elaborate to the **same** flat IR as the single-file
//! version — splitting a design across modules is a pure refactor with zero
//! effect on the emitted Verilog (and hence the conformance trace).

use std::path::{Path, PathBuf};

use sinter_compiler::ir::Module;
use sinter_compiler::sim::{gen_stimulus, run_reference};
use sinter_compiler::{compile_project, compile_str, verilog};

const SINGLE: &str = include_str!("../../../examples/go_board_counter.snt");

fn split_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go_board_split/main.snt")
}

fn top(modules: Vec<Module>) -> Module {
    modules
        .into_iter()
        .find(|m| m.name == "Top")
        .expect("a `Top` module")
}

#[test]
fn split_go_board_elaborates_bit_for_bit_identical() {
    let single = top(compile_str(SINGLE)
        .expect("single-file Go Board compiles")
        .modules);
    let multi = top(compile_project(&split_root())
        .expect("split Go Board project compiles")
        .modules);

    // The clock-root struct keeps its bare name in both, so the Verilog module
    // name and every leaf register / wire / driver are identical.
    let single_sv = verilog::emit_module(&single);
    let multi_sv = verilog::emit_module(&multi);
    assert_eq!(
        single_sv, multi_sv,
        "the split project must elaborate to byte-identical SystemVerilog"
    );

    // And the cycle-accurate reference model agrees bit-for-bit, cycle-for-cycle
    // over a shared stimulus (the oracle's reference side).
    let cycles = 64;
    let stim = gen_stimulus(&single, cycles, 0xDEAD_BEEF);
    assert_eq!(
        run_reference(&single, &stim),
        run_reference(&multi, &stim),
        "the split project must produce an identical simulation trace"
    );
}

/// A semantic error in a submodule names that file, quotes the offending line,
/// and puts a caret under the position (a tab in the line keeps its width).
#[test]
fn semantic_errors_name_the_file_and_quote_the_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/located_error/main.snt");
    let err = compile_project(&root)
        .expect_err("the submodule has a type error")
        .to_string();
    let counter = root.with_file_name("counter.snt");
    let expected = format!(
        "{}:7:13: type mismatch: expected `u8`, found `u9`\n  \
         |\n\
         7 | \tif en == 1 {{\n  \
         | \t           ^\n  \
         = in `bump`",
        counter.display()
    );
    assert_eq!(err, expected);
}
