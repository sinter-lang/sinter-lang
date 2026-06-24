#!/usr/bin/env bash
# Release build of the workspace. Mirrors CI's "Build release" step and
# exercises the stricter release profile (thin LTO) used for the emitted
# simulator path.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

step "cargo build --release"
cargo build --release
