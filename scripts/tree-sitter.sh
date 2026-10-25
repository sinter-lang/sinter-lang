#!/usr/bin/env bash
# Regenerate the tree-sitter parser (editors/tree-sitter-sinter/src/) from
# grammar.js, with the pinned tree-sitter CLI and its built-in JS runtime.
#
# Usage: scripts/tree-sitter.sh [--check]
#   --check   regenerate, then fail if the committed parser was stale (CI)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
die() { warn "$*"; exit 1; }

# Keep in sync with .devcontainer/Dockerfile and .github/workflows/ci.yml
# (TREE_SITTER_VERSION): another CLI version generates a different parser.c.
want="0.27.1"
have tree-sitter || die "tree-sitter CLI not found (install v$want)"
got="$(tree-sitter --version | awk '{print $2}')"
[ "$got" = "$want" ] || die "tree-sitter CLI is v$got; the committed parser uses v$want"

cd "$(dirname "${BASH_SOURCE[0]}")/../editors/tree-sitter-sinter"
step "tree-sitter generate (editors/tree-sitter-sinter)"
tree-sitter generate --js-runtime native

if [ "${1:-}" = "--check" ]; then
    git diff --exit-code -- src/ \
        || die "the committed parser is stale: run scripts/tree-sitter.sh and commit src/"
    info "the committed parser matches grammar.js"
fi
