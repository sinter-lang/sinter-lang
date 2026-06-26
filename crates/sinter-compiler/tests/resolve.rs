//! S2 tests: cross-module name resolution + visibility (DESIGN §10).
//!
//! Each fixture under `tests/fixtures/<name>/main.snt` is a small multi-file
//! project. We drive `compile_project`, which loads the module tree, resolves
//! cross-module names + visibility into a flat program, and runs the F1 checks.

use std::path::{Path, PathBuf};

use sinter_compiler::{compile_project, resolve};
use sinter_syntax::load_project;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .join("main.snt")
}

fn compile(name: &str) -> Result<(), String> {
    compile_project(&fixture(name))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn reject(name: &str, needle: &str) {
    match compile(name) {
        Ok(()) => panic!("{name}: expected rejection containing {needle:?}, but it compiled"),
        Err(msg) => assert!(
            msg.contains(needle),
            "{name}: error did not contain {needle:?}.\n  got: {msg}"
        ),
    }
}

#[test]
fn cross_module_use_type_checks() {
    compile("ok_project").expect("a pub struct + pub fn used across files should check");
}

#[test]
fn inline_qualified_path_resolves() {
    compile("qualified").expect("`root::math::add(..)` should resolve and check");
}

#[test]
fn same_name_in_two_modules_is_fine() {
    compile("same_name").expect("`a::Foo` and `b::Foo` should coexist");
}

#[test]
fn descendant_reaches_ancestor_private() {
    // `outer::inner` reads `super::SECRET`, a *private* const of `outer` — allowed
    // because `inner` is a descendant of `outer`.
    compile("super_priv").expect("descendant should access ancestor's private item");
}

#[test]
fn glob_import_brings_public_items_into_scope() {
    compile("glob").expect("`use root::m::*` should import the pub items");
}

#[test]
fn cross_module_enum_and_variants_resolve() {
    compile("enum_cross").expect("an imported enum + its variants should resolve");
}

#[test]
fn self_qualifier_resolves_in_current_module() {
    compile("self_qual").expect("`self::K` should resolve to the local const");
}

#[test]
fn importing_a_private_item_is_rejected() {
    reject("private_item", "private");
}

#[test]
fn qualified_path_to_private_item_is_rejected() {
    reject("qual_private", "private");
}

#[test]
fn importing_a_missing_item_is_rejected() {
    reject("unresolved", "no item `nope`");
}

// --- struct-field visibility (DESIGN §10) ----------------------------------

#[test]
fn reading_a_private_field_cross_module_is_rejected() {
    reject(
        "field_priv_read",
        "field `secret` of struct `m::Wid` is private",
    );
}

#[test]
fn constructing_with_a_private_field_cross_module_is_rejected() {
    reject(
        "field_priv_construct",
        "field `secret` of struct `m::Wid` is private",
    );
}

#[test]
fn cross_module_impl_is_rejected() {
    // An `impl` on a `use`d foreign type would otherwise type-check its bodies
    // with the *type's* module as the accessing module, bypassing field privacy
    // (review #1/#3). Require `impl` to be co-located with its type.
    reject("foreign_impl", "cross-module `impl` is not supported");
}

#[test]
fn reading_a_pub_field_cross_module_is_allowed() {
    compile("field_pub_read").expect("a `pub` field is readable from another module");
}

#[test]
fn descendant_reads_ancestor_struct_private_field() {
    // `outer::inner` reads a private field of a struct defined in `outer` — the
    // same descendant rule as item visibility.
    compile("field_descendant").expect("a descendant module may read an ancestor's private field");
}

// --- regression tests for the S2 adversarial-review findings ---------------

#[test]
fn clock_root_outside_root_module_is_rejected() {
    // A `#[clock]` root struct in a submodule would canonicalize to `sub::Top`,
    // an invalid Verilog module name (review #4/#8).
    reject("clock_in_sub", "must be defined in the project root module");
}

#[test]
fn legacy_module_in_submodule_is_rejected() {
    // Two submodule `module counter` blocks would flatten to duplicate Verilog
    // modules (review #5).
    reject("legacy_dup", "not allowed in a submodule");
}

#[test]
fn submodule_and_item_name_collision_is_rejected() {
    // `mod x;` plus `fn x()` makes the path `x` ambiguous (review #3/#6/#9).
    reject("name_collision", "both a submodule and an item");
}

#[test]
fn local_shadowing_a_function_name_is_not_rebound() {
    // A param named like an in-scope fn must NOT be silently rebound to the fn;
    // F1 then rejects the non-callable local (review #2).
    reject("callee_local", "unknown function");
}

#[test]
fn glob_import_yields_to_explicit_import() {
    // `use a::Foo` + `use b::*` (b also exports Foo): the explicit import wins,
    // the glob `Foo` is skipped rather than erroring (review #1/#10).
    compile("glob_shadow").expect("glob should yield to the explicit import");
}

#[test]
fn glob_over_own_module_is_a_no_op() {
    // `use self::*` over the module's own items must not be a duplicate error.
    compile("self_glob").expect("self-glob should be harmless");
}

#[test]
fn array_length_resolution_error_is_surfaced() {
    // A private const in an array-length position must error, not be swallowed
    // (review #7).
    reject("array_len_priv", "private");
}

#[test]
fn resolution_canonicalizes_names() {
    // The flat program renames items to their module-qualified canonical names;
    // root-module items stay bare.
    let tree = load_project(&fixture("ok_project")).expect("load");
    let program = resolve::resolve_project(&tree).expect("resolve");
    let names: Vec<&str> = program
        .items
        .iter()
        .filter_map(|i| match i {
            sinter_syntax::ast::Item::Struct(s) => Some(s.name.as_str()),
            sinter_syntax::ast::Item::Func(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"math::Pair"), "names: {names:?}");
    assert!(names.contains(&"math::add"), "names: {names:?}");
    assert!(
        names.contains(&"top"),
        "root item stays bare; names: {names:?}"
    );
}
