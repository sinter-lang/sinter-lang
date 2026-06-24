#!/usr/bin/env bash
# Check dependency licenses, advisories, bans, and sources against deny.toml.
# Part of the CLAUDE.md verify sequence ("when available"): cargo-deny is
# installed in the devcontainer but optional elsewhere, so its absence is a
# warning, not a failure.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

if ! have cargo-deny; then
    warn "cargo-deny not installed — skipping (install: cargo install --locked cargo-deny)"
    exit 0
fi

step "cargo deny check"
cargo deny check
