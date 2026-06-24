#!/usr/bin/env bash
# Check formatting without modifying any files.
# Mirrors CI's "Format check" step (.github/workflows/ci.yml).
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo fmt --all --check"
cargo fmt --all --check
