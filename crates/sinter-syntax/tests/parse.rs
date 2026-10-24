//! Parser tests for `sinter-syntax`.

use sinter_syntax::ast::{Item, Param};
use sinter_syntax::parse_program;

#[test]
fn rejects_zero_width_type() {
    let src = "struct S { pub v: u0 = 0, }";
    assert!(parse_program(src).is_err());
}

#[test]
fn rejects_garbage() {
    assert!(parse_program("struct {").is_err());
}

#[test]
fn rejects_the_removed_module_form() {
    let src = "module m { in a: u1; out b: u1; b = a; }";
    assert!(parse_program(src).is_err());
}

#[test]
fn parses_go_board_example() {
    let src = include_str!("../../../examples/go_board_counter.snt");
    let program = parse_program(src).expect("go_board_counter.snt should parse");

    // 1 const + 5 structs + 1 enum + 4 impls + 1 free fn = 12 items.
    assert_eq!(program.items.len(), 12);

    let consts = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Const(_)))
        .count();
    let structs = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Struct(_)))
        .count();
    let enums = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Enum(_)))
        .count();
    let impls = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Impl(_)))
        .count();
    let funcs = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Func(_)))
        .count();
    assert_eq!((consts, structs, enums, impls, funcs), (1, 5, 1, 4, 1));

    // The `Cmd` enum carries an `#[encoding(binary)]` attribute.
    let cmd = program
        .items
        .iter()
        .find_map(|i| match i {
            Item::Enum(e) if e.name == "Cmd" => Some(e),
            _ => None,
        })
        .expect("Cmd enum");
    assert_eq!(cmd.variants.len(), 3);
    assert_eq!(cmd.attrs.len(), 1);
    assert_eq!(cmd.attrs[0].name, "encoding");

    // The clock root `Top::tick` carries `#[clock(domain = sys)]`, takes
    // `self` plus a `btn` parameter, and returns the new state.
    let top = program
        .items
        .iter()
        .find_map(|i| match i {
            Item::Impl(im) if im.type_name == "Top" => Some(im),
            _ => None,
        })
        .expect("impl Top");
    let tick = top
        .methods
        .iter()
        .find(|m| m.name == "tick")
        .expect("Top::tick");
    assert_eq!(tick.attrs.len(), 1);
    assert_eq!(tick.attrs[0].name, "clock");
    assert!(matches!(tick.params.first(), Some(Param::SelfRef { .. })));
    assert_eq!(tick.params.len(), 2);
}
