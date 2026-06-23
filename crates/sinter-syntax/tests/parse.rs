//! Parser tests for `sinter-syntax`.

use sinter_syntax::ast::{AssignKind, Dir, Type};
use sinter_syntax::parse_program;

#[test]
fn parses_counter_example() {
    let src = include_str!("../../../examples/counter.snt");
    let program = parse_program(src).expect("counter.snt should parse");
    assert_eq!(program.modules.len(), 1);

    let m = &program.modules[0];
    assert_eq!(m.name, "counter");
    assert_eq!(m.clock_domain, "sys");
    assert_eq!(m.ports.len(), 2);
    assert_eq!(m.regs.len(), 1);
    assert_eq!(m.assigns.len(), 2);

    assert_eq!(m.ports[0].dir, Dir::In);
    assert_eq!(m.ports[0].name, "en");
    assert_eq!(m.ports[0].ty, Type::Uint(1));
    assert_eq!(m.ports[1].dir, Dir::Out);
    assert_eq!(m.ports[1].ty, Type::Uint(8));

    assert_eq!(m.regs[0].name, "value");
    assert_eq!(m.regs[0].ty, Type::Uint(8));

    assert_eq!(m.assigns[0].kind, AssignKind::NextState);
    assert_eq!(m.assigns[1].kind, AssignKind::Drive);
}

#[test]
fn parses_accumulator_example() {
    let src = include_str!("../../../examples/accumulator.snt");
    let program = parse_program(src).expect("accumulator.snt should parse");
    let m = &program.modules[0];
    assert_eq!(m.name, "accumulator");
    assert_eq!(m.ports.len(), 3);
    assert_eq!(m.regs[0].ty, Type::Int(16));
}

#[test]
fn default_clock_domain_is_sys() {
    let src = "module m { in a: u1; out b: u1; b = a; }";
    let program = parse_program(src).unwrap();
    assert_eq!(program.modules[0].clock_domain, "sys");
}

#[test]
fn rejects_zero_width_type() {
    let src = "module m { in a: u0; out b: u1; b = a; }";
    assert!(parse_program(src).is_err());
}

#[test]
fn rejects_garbage() {
    assert!(parse_program("module {").is_err());
}
