#!/usr/bin/env bash
# Run the conformance oracle (DESIGN §7.4): compile each example to
# SystemVerilog + a Verilator harness, run the Rust reference model, and — when
# Yosys/Verilator are available — synthesize and trace-diff against it.
#
# Usage: scripts/conformance.sh [REQUIRE]
#   REQUIRE = 1     fail if Yosys/Verilator are missing (CI behaviour)
#           = 0     never enforce; the oracle steps auto-skip when absent
#           = auto  enforce iff both tools are present (default; dev-friendly)
#
# A pre-set SINTER_REQUIRE_TOOLS=1 in the environment is always honoured.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

require="${1:-auto}"
case "$require" in
    1)
        export SINTER_REQUIRE_TOOLS=1
        ;;
    0)
        unset SINTER_REQUIRE_TOOLS
        ;;
    auto)
        if [ "${SINTER_REQUIRE_TOOLS:-}" = "1" ]; then
            info "SINTER_REQUIRE_TOOLS=1 (from environment) — enforcing the oracle"
        elif have yosys && have verilator; then
            export SINTER_REQUIRE_TOOLS=1
            info "yosys + verilator present — enforcing the oracle"
        else
            warn "yosys/verilator not found — oracle synth/sim steps will auto-skip"
        fi
        ;;
    *)
        warn "unknown REQUIRE '$require' (expected 1|0|auto); treating as auto"
        ;;
esac

step "cargo test -p sinter-compiler --test conformance -- --nocapture"
cargo test -p sinter-compiler --test conformance -- --nocapture
