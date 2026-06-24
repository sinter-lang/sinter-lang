#!/usr/bin/env bash
# Apply rustfmt formatting in place. Developer convenience; not run in CI
# (CI only verifies formatting via scripts/fmt.sh).
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo fmt --all"
cargo fmt --all
