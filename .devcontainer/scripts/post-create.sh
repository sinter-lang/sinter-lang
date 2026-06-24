#!/usr/bin/env bash
# Runs once when the container is created.
set -euo pipefail

echo "==> Sinter post-create"

# Activate skill submodules if SSH access is available (best effort).
echo "==> Activating skill submodules"
"$(dirname "$0")/init-skills.sh" || echo "    (skills unavailable here — continuing)"
git submodule update --init --recursive 2>/dev/null || true

# Install the Rust verification tooling referenced by the Rust skill.
echo "==> Installing cargo-deny / cargo-audit (best effort)"
cargo install --locked cargo-deny || echo "    (cargo-deny install skipped)"
cargo install --locked cargo-audit || echo "    (cargo-audit install skipped)"

echo "==> Warming the build cache"
cargo fetch || true

echo "==> Tool versions"
rustc --version || true
yosys --version 2>/dev/null | head -1 || echo "yosys: not found"
verilator --version 2>/dev/null | head -1 || echo "verilator: not found"

echo "==> post-create complete"
