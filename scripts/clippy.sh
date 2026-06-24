#!/usr/bin/env bash
# Lint with Clippy across all targets and features, denying every warning.
# Mirrors CI's "Clippy" step; the trailing `-D warnings` also fails on
# Clippy-specific lints (RUSTFLAGS only covers rustc warnings).
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo clippy --all-targets --all-features -- -D warnings"
cargo clippy --all-targets --all-features -- -D warnings
