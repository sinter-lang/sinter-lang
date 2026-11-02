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
