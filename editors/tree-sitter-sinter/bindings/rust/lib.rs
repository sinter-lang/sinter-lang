//! The Sinter grammar for tree-sitter (DESIGN §7.1).
//!
//! The compiler parses with its pest grammar; this grammar is for editors. It
//! follows the pest grammar and is tested against the same corpus.
//!
//! ```
//! let mut parser = tree_sitter::Parser::new();
//! parser
//!     .set_language(&tree_sitter_sinter::LANGUAGE.into())
//!     .expect("load the Sinter grammar");
//! let tree = parser.parse("fn f(x: u8) -> u8 { x }", None).expect("parse");
//! assert!(!tree.root_node().has_error());
//! ```

// The workspace forbids unsafe code; this crate is the one exception, since
// loading the generated C parser takes one FFI call.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use tree_sitter_language::LanguageFn;

#[allow(unsafe_code)]
extern "C" {
    fn tree_sitter_sinter() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for Sinter.
// SAFETY: `tree_sitter_sinter` is the generated parser's entry point, which
// returns a pointer to its static `TSLanguage`.
#[allow(unsafe_code)]
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_sinter) };

/// The syntax-highlighting query.
pub const HIGHLIGHTS_QUERY: &str = include_str!("../../queries/highlights.scm");

/// The node types, as JSON.
pub const NODE_TYPES: &str = include_str!("../../src/node-types.json");
