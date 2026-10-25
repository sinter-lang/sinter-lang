#!/usr/bin/env bash
# Check that the published crates (sinter-syntax, sinter-compiler, sinter-lang)
# package and build from their packaged sources, in dependency order, without
# uploading anything. The real upload is `cargo publish --workspace` at release
# time (README "Releasing").
#
# `--allow-dirty` lets this run on a working tree with uncommitted changes; it
# checks packaging, not the tree.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo publish --workspace --dry-run --locked"
cargo publish --workspace --dry-run --locked --allow-dirty
