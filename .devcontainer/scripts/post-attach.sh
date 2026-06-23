#!/usr/bin/env bash
# Runs each time a session attaches to the container.
set -euo pipefail

echo "Sinter dev container ready."
echo "  Yosys:     $(yosys --version 2>/dev/null | head -1 || echo 'not found')"
echo "  Verilator: $(verilator --version 2>/dev/null | head -1 || echo 'not found')"
echo "  Rust:      $(rustc --version 2>/dev/null || echo 'not found')"
echo
echo "Try:  cargo run -p sinter-lang -- build examples/counter.snt --emit verilog"
