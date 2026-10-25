#!/usr/bin/env bash
# Run a fuzzing campaign (crates/sinter-compiler/tests/fuzz.rs): generate
# PROGRAMS random, well-typed Sinter programs from seed SEED on, and put every
# one through the whole oracle — compile, the generated Rust simulator, Yosys
# synthesis, Verilator lint, and Verilator co-simulation. Yosys and Verilator
# are required.
#
# Usage: scripts/fuzz.sh [PROGRAMS] [SEED]   (defaults: 200, 1)
#
# Each program is saved as target/tmp/fuzz/seed-<seed>.snt. To rerun one (or a
# hand-reduced copy) through the oracle:
#   SINTER_FUZZ_FILE=path/to/prog.snt SINTER_FUZZ_SEED=<seed> \
#       cargo test -p sinter-compiler --test fuzz
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

export SINTER_FUZZ_PROGRAMS="${1:-200}"
export SINTER_FUZZ_SEED="${2:-1}"
export SINTER_REQUIRE_TOOLS=1

step "fuzz: ${SINTER_FUZZ_PROGRAMS} programs from seed ${SINTER_FUZZ_SEED}"
cargo test -p sinter-compiler --test fuzz -- --nocapture
