//! The checks on a program as written (`surface.rs`): attributes, `impl`
//! targets, and `pub` fields that would expose compiler-internal registers.

use sinter_compiler::compile_str;

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

/// A clock root around `fields` (the struct body) whose tick returns `self`.
fn root(fields: &str) -> String {
    format!(
        "struct T {{ {fields} }}
         impl T {{ #[clock(domain = sys)] fn tick(self) -> T {{ self }} }}"
    )
}

#[test]
fn unknown_attributes_are_rejected() {
    compile_err(
        &format!("#[inline] {}", root("pub v: u8 = 0")),
        "unknown attribute `#[inline]`",
    );
    // A misspelling used to be ignored silently.
    compile_err(
        "struct Q { #[storag(bram)] q: fifo<u8, 4> }
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { q: self.q.next(self.q.sender().no_push(), 0) } } }",
        "unknown attribute `#[storag]`",
    );
}

#[test]
fn misplaced_attributes_are_rejected() {
    compile_err(
        "#[clock(domain = sys)] fn f(x: u8) -> u8 { x }",
        "`#[clock]` doesn't apply to a free function",
    );
    compile_err(
        "struct T { pub v: u8 = 0 }
         impl T { #[domain(sys)] #[clock(domain = sys)] fn tick(self) -> T { self } }",
        "`#[domain]` doesn't apply to a method",
    );
    compile_err(
        "struct T { pub v: u8 = 0 }
         impl T { #[clock(domain = sys, rate = 2)] fn tick(self) -> T { self } }",
        "`#[clock]` doesn't take `rate`",
    );
}

#[test]
fn unimplemented_encodings_are_rejected() {
    for scheme in ["gray", "johnson"] {
        compile_err(
            &format!(
                "#[encoding({scheme})] enum E {{ A, B, C }} {}",
                root("pub v: u8 = 0")
            ),
            &format!("`#[encoding]` doesn't take `{scheme}`"),
        );
    }
}

#[test]
fn impl_on_an_enum_is_rejected() {
    compile_err(
        &format!(
            "enum E {{ A, B }} impl E {{ fn f(self) -> u1 {{ 1 }} }} {}",
            root("pub v: u8 = 0")
        ),
        "`impl` blocks are for structs, and `E` is an enum",
    );
}

#[test]
fn a_method_without_self_is_rejected() {
    compile_err(
        "struct T { pub v: u8 = 0 }
         impl T {
             fn make() -> T { T { v: 0 } }
             #[clock(domain = sys)] fn tick(self) -> T { self }
         }",
        "method `T::make` must take `self` first",
    );
}

#[test]
fn pub_internal_state_on_the_root_is_rejected() {
    let channel = "`pub` field `q` holds a channel";
    compile_err(
        "struct Q { pub q: fifo<u8, 4> }
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { q: self.q.next(self.q.sender().no_push(), 0) } } }",
        channel,
    );
    // Through a nested struct: everything under a root `pub` field is a port.
    compile_err(
        "struct Inner { q: ring<u8, 4> }
         struct Q { pub inner: Inner }
         impl Q { #[clock(domain = sys)] fn tick(self) -> Q { Q { inner: Inner { q: self.inner.q.next(None, 0) } } } }",
        "`pub` field `inner` holds a channel (in field `q` of `Inner`)",
    );
    compile_err(
        "pipeline fn f(a: u8) -> u8 { let b: u8 = a; yield; b }
         struct Top { pub inst: f }
         impl Top {
             #[clock(domain = sys)]
             fn tick(self, a: u8) -> (Top, u8) {
                 let (inst, r, v) = self.inst.advance(a, 1);
                 (Top { inst: inst }, r)
             }
         }",
        "`pub` field `inst` holds the staged function `f`",
    );
}

/// Off the clock root, `pub` is only module visibility, so it stays allowed.
#[test]
fn pub_internal_state_off_the_root_is_allowed() {
    compile_str(
        "struct Inner { pub q: fifo<u8, 4> }
         struct Q { inner: Inner }
         impl Q {
             #[clock(domain = sys)]
             fn tick(self) -> (Q, Option<u8>) {
                 let q = self.inner.q;
                 (Q { inner: Inner { q: q.next(q.sender().no_push(), 1) } }, q.front())
             }
         }",
    )
    .expect("`pub` on a nested struct's channel compiles");
}
