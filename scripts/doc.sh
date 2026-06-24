#!/usr/bin/env bash
# Build the workspace API documentation. Part of the CLAUDE.md verify sequence.
# RUSTDOCFLAGS=-D warnings (set by the justfile) turns broken intra-doc links
# and other rustdoc warnings into errors.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo doc --no-deps --workspace"
cargo doc --no-deps --workspace
