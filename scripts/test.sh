#!/usr/bin/env bash
# Run the unit + integration test suite for the whole workspace.
# Mirrors CI's "Unit + integration tests" step. This also exercises the
# conformance oracle, which auto-skips Yosys/Verilator when they are absent;
# scripts/conformance.sh runs it again with the tools enforced.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo test --workspace"
cargo test --workspace
