//! The generated module's port interface (docs/generated-verilog.md).
//!
//! Hand-written Verilog instantiates the emitted module by port name, so the
//! naming, ordering, and typing rules below are a compatibility contract:
//! these tests pin every one. A failure here means the documented interface
//! changed — update docs/generated-verilog.md and the CHANGELOG with it.

use sinter_compiler::{compile_str, compile_str_faults, verilog, FaultMode, Module};
use sinter_syntax::ast::{Dir, Type};

fn module(src: &str) -> Module {
    let design = compile_str(src).expect("source compiles");
    design.modules.into_iter().next().expect("one module")
}

/// The ports as `(direction, name, type)`, in declaration order.
fn ports(m: &Module) -> Vec<(Dir, &str, Type)> {
    m.ports
        .iter()
        .map(|p| (p.dir, p.name.as_str(), p.ty))
        .collect()
}

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

/// One design that exercises every flattening rule: struct, tuple, enum,
/// `Option`, and fixed-point inputs; array, `Option`, enum, fixed-point, and
/// memory state; and a tuple of extra outputs.
const ALL_RULES: &str = "
enum Mode { Idle, Run, Halt }

struct Pair {
    lo: u4,
    hi: u4,
}

struct Top {
    count: u8 = 0,
    mode: Mode = Mode::Idle,
    hist: [u4; 2] = [0, 0],
    last: Option<u4>,
    gain: i2.2 = 0,
    ram: mem<u8, 4> = 0,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, p: Pair, flags: (u1, u1), m: Mode, maybe: Option<u4>, g: i2.2) -> (Top, (u8, u1)) {
        let rd = self.ram.value();
        let (we, flag) = flags;
        (
            Top {
                count: (self.count + 1) as u8 wrap,
                mode: m,
                hist: [p.lo, p.hi],
                last: maybe,
                gain: g,
                ram: self.ram.next(p.lo[1:0], we, p.hi[1:0], self.count),
            },
            (rd, flag),
        )
    }
}
";

#[test]
fn every_flattening_rule_names_ports_as_documented() {
    use Dir::{In, Out};
    use Type::{Int, Uint};

    let m = module(ALL_RULES);
    assert_eq!(m.name, "Top", "the module is named after the state struct");
    assert_eq!(
        ports(&m),
        vec![
            // Inputs: one per scalar leaf of each `tick` parameter, in
            // parameter order, path segments joined with `_`.
            (In, "p_lo", Uint(4)),
            (In, "p_hi", Uint(4)),
            (In, "flags_0", Uint(1)),
            (In, "flags_1", Uint(1)),
            (In, "m", Uint(2)),
            (In, "maybe_is_some", Uint(1)),
            (In, "maybe_payload", Uint(4)),
            (In, "g", Int(4)),
            // `<register>_q`: every state register's current value, in field
            // order; a memory contributes its read-data register.
            (Out, "count_q", Uint(8)),
            (Out, "mode_q", Uint(2)),
            (Out, "hist_0_q", Uint(4)),
            (Out, "hist_1_q", Uint(4)),
            (Out, "last_is_some_q", Uint(1)),
            (Out, "last_payload_q", Uint(4)),
            (Out, "gain_q", Int(4)),
            (Out, "ram_rdata_q", Uint(8)),
            // `out_*`: the leaves of the tick's extra return value.
            (Out, "out_0", Uint(8)),
            (Out, "out_1", Uint(1)),
        ]
    );
}

#[test]
fn verilog_header_has_clock_and_reset_first() {
    let sv = verilog::emit_module(&module(ALL_RULES));
    assert!(sv.starts_with("`default_nettype none\n"));
    let header: Vec<&str> = sv
        .lines()
        .skip_while(|l| !l.starts_with("module Top ("))
        .skip(1)
        .take(3)
        .map(str::trim)
        .collect();
    assert_eq!(header[0], "input  wire         clk,    // sys clock");
    assert_eq!(
        header[1],
        "input  wire         rst_n,  // active-low asynchronous reset"
    );
    assert_eq!(header[2], "input  wire  [3:0] p_lo,");
    // Signed (and signed fixed-point) ports are declared `signed`.
    assert!(sv.contains("input  wire  signed [3:0] g,"));
    assert!(sv.contains("output logic signed [3:0] gain_q,"));
}

#[test]
fn a_single_extra_output_is_named_out() {
    let m = module(
        "struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> (S, u8) { (S { v: x }, self.v) }
         }",
    );
    let names: Vec<_> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["v_q", "out"]);
}

#[test]
fn a_struct_extra_output_is_named_by_field() {
    let m = module(
        "struct Pair { lo: u4, hi: u4, }
         struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> (S, Pair) {
                 (S { v: x }, Pair { lo: x[3:0], hi: x[7:4] })
             }
         }",
    );
    let names: Vec<_> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["v_q", "out_lo", "out_hi"]);
}

#[test]
fn no_extra_output_means_only_register_outputs() {
    let m = module(
        "struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S { S { v: x } }
         }",
    );
    let names: Vec<_> = m.outputs().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["v_q"]);
}

#[test]
fn enum_width_follows_its_encoding() {
    let m = module(
        "#[encoding(onehot)]
         enum Mode { Idle, Run, Halt }
         struct S { mode: Mode = Mode::Idle, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, m: Mode) -> S { S { mode: m } }
         }",
    );
    assert_eq!(
        ports(&m),
        vec![
            (Dir::In, "m", Type::Uint(3)),
            (Dir::Out, "mode_q", Type::Uint(3))
        ]
    );
}

#[test]
fn faults_on_appends_the_fault_code_sideband() {
    let src = "struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, x: u8) -> S {
                 assert!(x != 0, \"zero\");
                 S { v: x }
             }
         }";
    let off = compile_str_faults(src, FaultMode::Off).expect("compiles");
    let on = compile_str_faults(src, FaultMode::On).expect("compiles");
    let names = |m: &Module| m.ports.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&off.modules[0]), ["x", "v_q"]);
    assert_eq!(names(&on.modules[0]), ["x", "v_q", "fault_code"]);
}

#[test]
fn reserved_and_colliding_names_are_rejected() {
    compile_err(
        "struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, clk: u1) -> S { S { v: 0 } }
         }",
        "collides with the reserved clock/reset port",
    );
    compile_err(
        "struct S { v: u8 = 0, }
         impl S {
             #[clock(domain = sys)]
             fn tick(self, v_q: u8) -> S { S { v: v_q } }
         }",
        "collides with another port",
    );
}
