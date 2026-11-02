//! Tests for the pin-map (`.pins`) parser (DESIGN §7.6).

use sinter_compiler::pinmap::{PinDir, PinMap};

fn parse(src: &str) -> Result<PinMap, String> {
    PinMap::parse(src, "test.pins").map_err(|e| e.to_string())
}

fn reject(src: &str, needle: &str) {
    match parse(src) {
        Ok(_) => panic!("expected rejection containing {needle:?}, but it parsed"),
        Err(msg) => assert!(
            msg.contains(needle),
            "error did not contain {needle:?}.\n  got: {msg}"
        ),
    }
}

#[test]
fn go_board_pins_parses() {
    let src = include_str!("../../../examples/go_board.pins");
    let map = parse(src).expect("go_board.pins should parse");
    // 1 clock + 2 inputs + 14 segment outputs.
    assert_eq!(map.bindings.len(), 17);

    let clk = &map.bindings[0];
    assert_eq!(clk.dir, PinDir::Clock);
    assert_eq!(clk.pin, "15");
    assert_eq!(clk.path, "sys");
    assert!(!clk.active_low);

    let inc = &map.bindings[1];
    assert_eq!(inc.dir, PinDir::In);
    assert_eq!(inc.path, "btn.inc");
    assert!(!inc.active_low);

    // All 14 segment lines are active-low outputs.
    let segs: Vec<_> = map
        .bindings
        .iter()
        .filter(|b| b.dir == PinDir::Out)
        .collect();
    assert_eq!(segs.len(), 14);
    assert!(segs.iter().all(|b| b.active_low));
    assert_eq!(segs[0].path, "seg[0][0]");
    assert_eq!(segs[0].pin, "3");
}

#[test]
fn comments_and_blank_lines_are_skipped() {
    let src = "# a comment\n\n  \nset_io 7 out led[0]  # trailing comment\n";
    let map = parse(src).expect("should parse");
    assert_eq!(map.bindings.len(), 1);
    assert_eq!(map.bindings[0].path, "led[0]");
    assert_eq!(map.bindings[0].pin, "7");
}

#[test]
fn active_low_flag_is_parsed() {
    let map = parse("set_io 3 out seg[0][0] low").expect("should parse");
    assert!(map.bindings[0].active_low);
}

#[test]
fn non_set_io_directive_is_rejected() {
    reject("frequency clk 25", "expected a `set_io`");
}

#[test]
fn unknown_direction_is_rejected() {
    reject("set_io 3 inout foo", "unknown direction `inout`");
}

#[test]
fn too_few_tokens_is_rejected() {
    reject("set_io 3 out", "needs `<pin#> <dir> <path> [low]");
}

#[test]
fn unexpected_trailing_token_is_rejected() {
    reject(
        "set_io 3 out seg[0][0] high",
        "unexpected trailing token `high`",
    );
}

#[test]
fn clock_cannot_be_active_low() {
    reject("set_io 15 clock sys low", "cannot be marked `low`");
}

// --- LPF1: ECP5-only extras (DESIGN §9.15) ---------------------------------

#[test]
fn io_type_and_freq_are_parsed() {
    let map = parse(
        "set_io G2 clock sys io_type=LVCMOS33 freq=25MHz\n\
         set_io R1 in btn.inc low io_type=LVCMOS25\n\
         set_io B1 clock sys freq=12.5mhz",
    )
    .expect("parse");
    let b = &map.bindings;
    assert_eq!(b[0].io_type.as_deref(), Some("LVCMOS33"));
    assert_eq!(b[0].freq_mhz.as_deref(), Some("25"));
    assert!(b[1].active_low);
    assert_eq!(b[1].io_type.as_deref(), Some("LVCMOS25"));
    assert_eq!(b[1].freq_mhz, None);
    assert_eq!(b[2].freq_mhz.as_deref(), Some("12.5"));
}

#[test]
fn ecp5_example_pins_parse() {
    let map = parse(include_str!("../../../examples/go_board_ecp5.pins")).expect("parse");
    assert_eq!(map.bindings.len(), 17);
    assert!(map
        .bindings
        .iter()
        .all(|b| b.io_type.as_deref() == Some("LVCMOS33")));
    assert_eq!(map.bindings[0].freq_mhz.as_deref(), Some("25"));
}

#[test]
fn freq_only_on_a_clock_pin() {
    reject(
        "set_io R1 in btn.inc freq=25MHz",
        "only a `clock` pin takes `freq=`",
    );
}

#[test]
fn bad_freq_is_rejected() {
    for f in ["25", "MHz", "0MHz", "1.2.3MHz", ".5MHz", "25kHz", "-5MHz"] {
        reject(
            &format!("set_io G2 clock sys freq={f}"),
            "`freq=` takes a frequency in MHz",
        );
    }
}

#[test]
fn bad_io_type_is_rejected() {
    reject(
        "set_io R1 in btn.inc io_type=",
        "`io_type=` takes an I/O standard",
    );
    reject(
        "set_io R1 in btn.inc io_type=LV\"CMOS",
        "`io_type=` takes an I/O standard",
    );
}
