#!/usr/bin/env bash
# Audit the dependency tree (Cargo.lock) for known security advisories.
# Part of the CLAUDE.md verify sequence ("when available"): cargo-audit is
# installed in the devcontainer but optional elsewhere, so its absence is a
# warning, not a failure.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

if ! have cargo-audit; then
    warn "cargo-audit not installed — skipping (install: cargo install --locked cargo-audit)"
    exit 0
fi

step "cargo audit"
cargo audit
