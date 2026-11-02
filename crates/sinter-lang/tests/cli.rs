//! End-to-end tests of the `sinter` binary.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

const COUNTER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/fn_counter.snt");

/// A reader that closes the pipe early (`sinter sim ... | head`) ends the
/// output quietly: no panic, and the run's own status.
#[test]
fn sim_stops_quietly_when_stdout_closes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sinter"))
        .args(["sim", COUNTER, "--cycles", "200000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run sinter");
    let stdout = child.stdout.take().expect("stdout");
    let mut first = String::new();
    BufReader::new(stdout)
        .read_line(&mut first)
        .expect("read the header");
    assert!(first.starts_with("cycle |"), "header: {first:?}");
    // Dropping the reader closes the pipe while the simulation is still printing.
    let out = child.wait_with_output().expect("wait for sinter");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "stderr: {stderr}");
    assert!(
        out.status.success(),
        "status {:?}, stderr: {stderr}",
        out.status
    );
}

const GO_BOARD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/go_board_counter.snt"
);
const ECP5_PINS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/go_board_ecp5.pins"
);

/// A fresh, empty directory under the target dir.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn build_board(
    out: &std::path::Path,
    constraints: Option<&std::path::Path>,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sinter"));
    cmd.args([
        "build", GO_BOARD, "--emit", "verilog", "--pins", ECP5_PINS, "--out",
    ])
    .arg(out);
    if let Some(c) = constraints {
        cmd.arg("--constraints").arg(c);
    }
    cmd.output().expect("run sinter")
}

/// `--constraints F.lpf` writes an ECP5 LPF there (LPF1, DESIGN §9.15).
#[test]
fn constraints_extension_picks_an_lpf() {
    let dir = scratch("cli-lpf");
    let lpf = dir.join("board.lpf");
    let out = build_board(&dir, Some(&lpf));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&lpf).expect("read lpf");
    assert!(text.contains("FREQUENCY PORT \"clk\" 25 MHZ;"), "{text}");
    assert!(!dir.join("go_board_ecp5.pcf").exists(), "no PCF alongside");
}

/// Without `--constraints` the PCF goes to the out dir as before, with a
/// warning that the map's `io_type=` / `freq=` settings are dropped.
#[test]
fn a_pcf_warns_that_lpf_settings_are_dropped() {
    let dir = scratch("cli-pcf");
    let out = build_board(&dir, None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("a PCF can't carry"), "stderr: {stderr}");
    let pcf = std::fs::read_to_string(dir.join("go_board_ecp5.pcf")).expect("read pcf");
    assert!(pcf.contains("set_io clk G2"), "{pcf}");
}

#[test]
fn an_unknown_constraints_extension_is_rejected() {
    let dir = scratch("cli-xdc");
    let out = build_board(&dir, Some(&dir.join("board.xdc")));
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the extension picks the format"),
        "stderr: {stderr}"
    );
}

const PARAMS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/fn_params.snt");

/// `-D` overrides a `pub const` on `check`, `build`, and `sim`, and
/// `--defines` reads a file of them, with a `-D` winning (DEF1).
#[test]
fn defines_override_a_pub_const() {
    let dir = scratch("cli-defines");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .args(args)
            .output()
            .expect("run sinter")
    };
    let out = dir.to_str().expect("utf-8 path");
    let built = run(&[
        "build", PARAMS, "--emit", "verilog", "--out", out, "-D", "DEPTH=64",
    ]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let sv = std::fs::read_to_string(dir.join("Params.sv")).expect("read sv");
    assert!(sv.contains("ram [0:63]"), "{sv}");

    let file = dir.join("wide.defs");
    std::fs::write(&file, "# wide build\nDEPTH = 32\nLANES = 8\n").expect("write defines");
    let file = file.to_str().expect("utf-8 path");
    let built = run(&[
        "build",
        PARAMS,
        "--emit",
        "verilog",
        "--out",
        out,
        "--defines",
        file,
        "-D",
        "DEPTH=128",
    ]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let sv = std::fs::read_to_string(dir.join("Params.sv")).expect("read sv");
    assert!(sv.contains("ram [0:127]") && sv.contains("lanes_7"), "{sv}");

    let sim = run(&["sim", PARAMS, "--cycles", "2", "-D", "STEP=9"]);
    assert!(
        sim.status.success(),
        "{}",
        String::from_utf8_lossy(&sim.stderr)
    );

    let bad = run(&["check", PARAMS, "-D", "NOPE=1"]);
    assert!(!bad.status.success());
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("-D NOPE=1: no constant `NOPE`"),
        "stderr: {stderr}"
    );
}

fn verilator_present() -> bool {
    let present = Command::new("verilator")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if std::env::var("SINTER_REQUIRE_TOOLS").is_ok_and(|v| v == "1") {
        assert!(present, "SINTER_REQUIRE_TOOLS=1 but verilator is missing");
    }
    present
}

/// `sinter sim --cosim` passes on an example with a foreign module, and
/// reports a reference model that disagrees with it (COSIM1).
#[test]
fn cosim_reports_a_broken_foreign_model() {
    if !verilator_present() {
        return;
    }
    let example =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/verilog_acc");
    let sim = |main: &std::path::Path, dir: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_sinter"))
            .arg("sim")
            .arg(main)
            .args(["--cycles", "40", "--cosim", "--cosim-dir"])
            .arg(dir)
            .output()
            .expect("run sinter")
    };
    let good = sim(&example.join("main.snt"), &scratch("cli-cosim-good"));
    let stderr = String::from_utf8_lossy(&good.stderr);
    assert!(good.status.success(), "{stderr}");
    assert!(
        stderr.contains("the Verilog matches the simulator over 40 cycle(s)"),
        "{stderr}"
    );

    let dir = scratch("cli-cosim-broken");
    std::fs::create_dir_all(dir.join("rtl")).expect("create rtl dir");
    std::fs::copy(example.join("rtl/acc16.v"), dir.join("rtl/acc16.v")).expect("copy acc16.v");
    let main = std::fs::read_to_string(example.join("main.snt")).expect("read main.snt");
    let wrong = main.replace("sum: self.acc", "sum: next");
    assert_ne!(main, wrong, "the mutation must apply");
    std::fs::write(dir.join("main.snt"), wrong).expect("write main.snt");
    let bad = sim(&dir.join("main.snt"), &dir.join("cosim"));
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(!bad.status.success(), "{stderr}");
    assert!(
        stderr.contains("co-simulation mismatch (cycle ") && stderr.contains("in the Verilog"),
        "{stderr}"
    );
}
