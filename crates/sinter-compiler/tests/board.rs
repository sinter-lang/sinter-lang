//! Tests for the board-top wrapper + PCF generator (DESIGN §7.6, milestone F5).

use std::path::Path;
use std::process::Command;

use sinter_compiler::board::emit_board;
use sinter_compiler::ir::Module;
use sinter_compiler::pinmap::PinMap;
use sinter_compiler::{compile_str, verilog};

const GO_BOARD_SNT: &str = include_str!("../../../examples/go_board_counter.snt");
const GO_BOARD_PINS: &str = include_str!("../../../examples/go_board.pins");

/// Compile the Go Board example to its single elaborated module.
fn go_board_module() -> Module {
    compile_str(GO_BOARD_SNT)
        .expect("go_board_counter.snt should compile")
        .modules
        .into_iter()
        .next()
        .expect("one module")
}

/// Emit the board for a given pin-map source against the Go Board module.
fn emit(pins: &str) -> Result<sinter_compiler::board::Board, String> {
    let module = go_board_module();
    let map = PinMap::parse(pins, "test.pins").map_err(|e| e.to_string())?;
    emit_board(&module, &map, "go_board").map_err(|e| e.to_string())
}

fn reject(pins: &str, needle: &str) {
    match emit(pins) {
        Ok(_) => panic!("expected rejection containing {needle:?}, but it generated"),
        Err(msg) => assert!(
            msg.contains(needle),
            "error did not contain {needle:?}.\n  got: {msg}"
        ),
    }
}

/// Compile a custom functional source to its single elaborated module.
fn module_from(src: &str) -> Module {
    compile_str(src)
        .expect("source should compile")
        .modules
        .into_iter()
        .next()
        .expect("one module")
}

/// Emit a board for a custom (source, pin-map) pair with wrapper name `name`.
fn board_for(src: &str, pins: &str, name: &str) -> Result<sinter_compiler::board::Board, String> {
    let module = module_from(src);
    let map = PinMap::parse(pins, "test.pins").map_err(|e| e.to_string())?;
    emit_board(&module, &map, name).map_err(|e| e.to_string())
}

/// A 2-field state whose second field `a_3` collides textually with bit 3 of the
/// first field `a` once a path is flattened (`a[3]` folds to `a_3`).
const COLLIDE_SNT: &str = "
    struct St { pub a: u8 = 0, pub a_3: u1 = 0 }
    impl St {
        #[clock(domain = sys)]
        fn tick(self) -> St { self }
    }
";

#[test]
fn bit_index_taps_the_leaf_register_not_a_sibling() {
    // `a[3]` must address bit 3 of register `a` (output `a_q`), NOT the unrelated
    // sibling register `a_3` that the textual fold collides with (DESIGN §7.6).
    let board = board_for(COLLIDE_SNT, "set_io 15 clock sys\nset_io 3 out a[3]", "brd")
        .expect("should generate");
    assert!(
        board.verilog.contains("assign a_3 = a_q[3];"),
        "a[3] must tap a_q bit 3, got:\n{}",
        board.verilog
    );
}

#[test]
fn whole_one_bit_output_is_not_bit_selected() {
    // A width-1 observation wire is scalar, so the drive must be `a_3_q`, never
    // the illegal scalar select `a_3_q[0]` (which fails Verilator -Wall).
    let board = board_for(COLLIDE_SNT, "set_io 15 clock sys\nset_io 3 out a_3", "brd")
        .expect("should generate");
    assert!(
        board.verilog.contains("assign a_3 = a_3_q;"),
        "1-bit output must drive whole, got:\n{}",
        board.verilog
    );
    assert!(
        !board.verilog.contains("a_3_q[0]"),
        "must not bit-select a scalar wire:\n{}",
        board.verilog
    );
}

#[test]
fn input_flattening_to_rst_n_is_rejected_at_elaboration() {
    // A tick parameter whose leaf flattens to the reserved `rst_n` port would
    // duplicate-declare; the elaborator must reject it.
    let src = "
        struct R { n: u1 }
        struct St { pub x: u1 = 0 }
        impl St {
            #[clock(domain = sys)]
            fn tick(self, rst: R) -> St { St { x: rst.n } }
        }
    ";
    let err = compile_str(src).expect_err("input `rst_n` should be rejected");
    assert!(
        err.to_string().contains("reserved clock/reset port"),
        "got: {err}"
    );
}

#[test]
fn input_flattening_to_reserved_wrapper_signal_is_rejected() {
    // `por_count` is a generated wrapper-internal net; an input pin bound to it
    // must be rejected (else the wrapper duplicate-declares).
    let src = "
        struct St { pub x: u1 = 0 }
        impl St {
            #[clock(domain = sys)]
            fn tick(self, por_count: u1) -> St { St { x: por_count } }
        }
    ";
    let err = board_for(src, "set_io 15 clock sys\nset_io 3 in por_count", "brd")
        .expect_err("should reject reserved name");
    assert!(
        err.contains("reserved wrapper signal `por_count`"),
        "got: {err}"
    );
}

#[test]
fn duplicate_physical_pin_is_rejected() {
    reject(
        "set_io 15 clock sys\nset_io 15 in btn.inc\nset_io 51 in btn.dec",
        "physical pin `15` is already bound",
    );
}

#[test]
fn more_than_one_clock_pin_is_rejected_with_its_own_message() {
    // The dedicated diagnostic must fire, not the generic port-name collision.
    reject(
        "set_io 15 clock sys\nset_io 16 clock sys",
        "more than one clock pin",
    );
}

#[test]
fn output_generation_is_deterministic() {
    let a = emit(GO_BOARD_PINS).expect("generate");
    let b = emit(GO_BOARD_PINS).expect("generate");
    assert_eq!(a.verilog, b.verilog, "wrapper SV must be deterministic");
    assert_eq!(a.pcf, b.pcf, "PCF must be deterministic");
}

/// State + multi-bit input, used to exercise bit-by-bit input assembly, an
/// active-low input, and a partially-tapped output.
const MULTIBIT_SNT: &str = "
    struct St { pub acc: u2 = 0 }
    impl St {
        #[clock(domain = sys)]
        fn tick(self, data: u2) -> St { St { acc: data } }
    }
";

const MULTIBIT_PINS: &str =
    "set_io 15 clock sys\nset_io 3 in data[0]\nset_io 4 in data[1] low\nset_io 5 out acc[0]";

#[test]
fn multibit_input_is_assembled_with_polarity_and_partial_output_sunk() {
    let board = board_for(MULTIBIT_SNT, MULTIBIT_PINS, "brd").expect("should generate");
    let sv = &board.verilog;
    // Bit-by-bit assembly into an internal wire, active-low bit inverted.
    assert!(sv.contains("logic [1:0] data_int;"), "{sv}");
    assert!(sv.contains("assign data_int[0] = data_0;"), "{sv}");
    assert!(sv.contains("assign data_int[1] = ~data_1;"), "{sv}");
    assert!(sv.contains(".data (data_int)"), "{sv}");
    // Tapped output bit + the untapped bit sunk.
    assert!(sv.contains("assign acc_0 = acc_q[0];"), "{sv}");
    assert!(sv.contains("_unused_ok") && sv.contains("acc_q[1]"), "{sv}");
}

#[test]
fn go_board_wrapper_and_pcf_generate() {
    let board = emit(GO_BOARD_PINS).expect("go_board should generate");
    assert_eq!(board.module_name, "go_board");

    let sv = &board.verilog;
    // Header, one port per pin, the inner instance, polarity inversion, sink.
    assert!(sv.contains("module go_board ("));
    assert!(sv.contains("input  wire clk,"));
    assert!(sv.contains("input  wire btn_inc,"));
    assert!(sv.contains("output wire seg_0_0,"));
    assert!(
        sv.contains("output wire seg_1_6 "),
        "last port has no comma"
    );
    assert!(sv.contains("Top u_top ("));
    assert!(sv.contains(".btn_inc (btn_inc)"));
    assert!(sv.contains(".seg_0_q (seg_0_q)"));
    // Active-low segments are inverted at the boundary.
    assert!(sv.contains("assign seg_0_0 = ~seg_0_q[0];"));
    assert!(sv.contains("assign seg_1_6 = ~seg_1_q[6];"));
    // Power-on reset (no reset pin) and the lint-clean unused sink.
    assert!(sv.contains("assign rst_n = (por_count == 4'd15);"));
    assert!(sv.contains("_unused_ok"));
    // An unpinned output (`led`, which go_board.pins doesn't map) is sunk.
    assert!(sv.contains("wire _unused_ok = &{1'b0, led_q};"));

    let pcf = &board.pcf;
    assert!(pcf.contains("set_io clk 15"));
    assert!(pcf.contains("set_io btn_inc 53"));
    assert!(pcf.contains("set_io seg_0_6 2"));
    assert!(pcf.contains("set_io seg_1_6 7"));
}

#[test]
fn clock_domain_mismatch_is_rejected() {
    reject(
        "set_io 15 clock other\nset_io 53 in btn.inc\nset_io 51 in btn.dec",
        "clock pin binds domain `other`",
    );
}

#[test]
fn missing_clock_is_rejected() {
    reject(
        "set_io 53 in btn.inc\nset_io 51 in btn.dec",
        "no `clock` pin",
    );
}

#[test]
fn unbound_inner_input_is_rejected() {
    // `btn.dec` is left unmapped, so the inner `btn_dec` port has no driver.
    reject(
        "set_io 15 clock sys\nset_io 53 in btn.inc",
        "inner input `btn_dec` has no pin binding",
    );
}

#[test]
fn unresolved_output_path_is_rejected() {
    reject(
        "set_io 15 clock sys\nset_io 53 in btn.inc\nset_io 51 in btn.dec\nset_io 3 out nope[0]",
        "resolves to no state register",
    );
}

#[test]
fn duplicate_wrapper_port_is_rejected() {
    reject(
        "set_io 15 clock sys\nset_io 3 out seg[0][0]\nset_io 4 out seg[0][0]",
        "pin port `seg_0_0` already defined",
    );
}

#[test]
fn out_of_range_bit_is_rejected() {
    // `seg[0]` is a u7 register; bit 9 is out of range.
    reject(
        "set_io 15 clock sys\nset_io 53 in btn.inc\nset_io 51 in btn.dec\nset_io 3 out seg[0][9]",
        "bit 9 is out of range",
    );
}

// --- Synthesizability + lint of the generated wrapper (tools permitting) -----

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn require_tools() -> bool {
    std::env::var("SINTER_REQUIRE_TOOLS").is_ok_and(|v| v == "1")
}

#[test]
fn wrapper_synthesizes_and_lints() {
    let module = go_board_module();
    let map = PinMap::parse(GO_BOARD_PINS, "go_board.pins").expect("parse pins");
    let board = emit_board(&module, &map, "go_board").expect("emit board");
    check_synth_lint(&module, &board, "sinter-board-go");
}

#[test]
fn multibit_wrapper_synthesizes_and_lints() {
    // Exercises assembled multi-bit input, active-low inversion, and a partially
    // tapped output through the real Yosys + Verilator -Wall gate.
    let module = module_from(MULTIBIT_SNT);
    let map = PinMap::parse(MULTIBIT_PINS, "test.pins").expect("parse pins");
    let board = emit_board(&module, &map, "mb_board").expect("emit board");
    check_synth_lint(&module, &board, "sinter-board-mb");
}

/// Write the inner module + wrapper and run Yosys synth + Verilator `-Wall` lint
/// (the `claude-skill-verilog` gate), skipping when the tools are absent unless
/// `SINTER_REQUIRE_TOOLS=1` enforces them.
fn check_synth_lint(module: &Module, board: &sinter_compiler::board::Board, workdir: &str) {
    let have_yosys = tool_available("yosys");
    let have_verilator = tool_available("verilator");
    if require_tools() {
        assert!(have_yosys, "SINTER_REQUIRE_TOOLS=1 but yosys is missing");
        assert!(
            have_verilator,
            "SINTER_REQUIRE_TOOLS=1 but verilator is missing"
        );
    }
    if !have_yosys && !have_verilator {
        eprintln!("skipping board synth/lint: neither yosys nor verilator found");
        return;
    }

    let dir = std::env::temp_dir().join(workdir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create workdir");
    let inner = dir.join(format!("{}.sv", module.name));
    let wrapper = dir.join(format!("{}.sv", board.module_name));
    std::fs::write(&inner, verilog::emit_module(module)).expect("write inner");
    std::fs::write(&wrapper, &board.verilog).expect("write wrapper");

    if have_yosys {
        run_yosys(&[&inner, &wrapper], &board.module_name);
    }
    if have_verilator {
        lint_verilator(&[&wrapper, &inner], &board.module_name);
    }
}

/// Synthesize the wrapper hierarchy with Yosys to confirm it is synthesizable.
fn run_yosys(files: &[&Path], top: &str) {
    let reads: Vec<String> = files
        .iter()
        .map(|f| format!("read_verilog -sv {}", f.display()))
        .collect();
    let script = format!(
        "{}; hierarchy -check -top {top}; proc; opt; synth -top {top}",
        reads.join("; ")
    );
    let out = Command::new("yosys")
        .args(["-q", "-p", &script])
        .output()
        .expect("spawn yosys");
    assert!(
        out.status.success(),
        "yosys failed for {top}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Lint the wrapper with Verilator `-Wall` (the conformance flag set), with no
/// suppression pragmas — the `claude-skill-verilog` standard.
fn lint_verilator(files: &[&Path], top: &str) {
    let mut cmd = Command::new("verilator");
    cmd.args(["--lint-only", "-Wall", "--top-module", top]);
    for f in files {
        cmd.arg(f);
    }
    let out = cmd.output().expect("spawn verilator");
    assert!(
        out.status.success(),
        "verilator lint failed for {top}:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
