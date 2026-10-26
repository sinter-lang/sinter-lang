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
