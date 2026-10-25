#!/usr/bin/env bash
# Build the Sinter book (docs/book/) with the pinned mdBook into build/book/.
# Its code samples are compiled by `cargo test`
# (crates/sinter-compiler/tests/book.rs), so this checks that it renders.
#
# Usage: scripts/book.sh [serve]
#   serve   build, then serve on http://localhost:3000 and rebuild on change
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
die() { warn "$*"; exit 1; }

# Keep in sync with .devcontainer/Dockerfile and .github/workflows/ci.yml
# (MDBOOK_VERSION).
want="0.5.4"
have mdbook || die "mdbook not found (install v$want)"
got="$(mdbook --version | awk '{print $2}')"
[ "$got" = "v$want" ] || warn "mdbook is $got; CI builds the book with v$want"

if [ "${1:-}" = "serve" ]; then
    step "mdbook serve docs/book"
    mdbook serve docs/book
else
    step "mdbook build docs/book -> build/book"
    mdbook build docs/book
fi
