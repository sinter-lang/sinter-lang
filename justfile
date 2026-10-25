# Sinter task runner (https://just.systems).
#
# Every recipe is a thin wrapper over a script in scripts/, so the same task
# runs identically from a developer shell, the devcontainer, and CI. The CI
# workflow (.github/workflows/ci.yml) drives these very recipes, which means
# `just ci` reproduces the GitHub pipeline locally, step for step.
#
#   just            # list all recipes
#   just check      # fast inner-loop gate: fmt + clippy + test
#   just ci         # reproduce the GitHub CI pipeline exactly
#   just verify     # full pre-push gate: ci + docs + license/advisory checks
#
# Treat rustc/rustdoc warnings as errors everywhere, matching CI's RUSTFLAGS.
# Export RUSTFLAGS / RUSTDOCFLAGS before invoking just to override.
export RUSTFLAGS := env_var_or_default("RUSTFLAGS", "-D warnings")
export RUSTDOCFLAGS := env_var_or_default("RUSTDOCFLAGS", "-D warnings")

# Run recipe lines under bash with strict flags.
set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Show the recipe list (default when `just` is run with no arguments).
default:
    @just --list

# --- Code-quality checks ---------------------------------------------------

# Check formatting without modifying files (CI: Format check).
fmt:
    scripts/fmt.sh

# Apply rustfmt formatting in place (developer convenience).
fmt-fix:
    scripts/fmt-fix.sh

# Lint with Clippy, denying all warnings (CI: Clippy).
clippy:
    scripts/clippy.sh

# Build the API docs; broken doc links fail the build (CLAUDE.md verify).
doc:
    scripts/doc.sh

# Check dependency licenses + advisories via deny.toml (skips if cargo-deny absent).
deny:
    scripts/deny.sh

# Audit dependencies for security advisories (skips if cargo-audit absent).
audit:
    scripts/audit.sh

# --- Build + test ----------------------------------------------------------

# Run the unit + integration test suite (CI: Unit + integration tests).
test:
    scripts/test.sh

# Run the conformance oracle. require = 1 | 0 | auto (default).
conformance require="auto":
    scripts/conformance.sh {{ require }}

# Fuzz: put `programs` random programs from `seed` through the whole oracle
# (needs Yosys + Verilator).
fuzz programs="200" seed="1":
    scripts/fuzz.sh {{ programs }} {{ seed }}

# Release build of the workspace (CI: Build release).
build:
    scripts/build.sh

# Emit SystemVerilog for the example .snt files into build/verilog/ (git-ignored,
# observable in the dev container). emit = verilog (default) | harness | all.
verilog emit="verilog":
    scripts/verilog.sh {{ emit }}

# Emit the board-top Verilog wrapper + nextpnr PCF for each pin-mapped example
# into build/board/ (git-ignored). Bridges the portless functional surface to
# physical pins (DESIGN §7.6).
board:
    scripts/board.sh

# --- Aggregates (these invoke the recipes above) ---------------------------

# Fast inner-loop gate: format, lint, and test.
check: fmt clippy test

# Reproduce the GitHub CI pipeline exactly (conformance enforced; needs the tools).
ci: fmt clippy test (conformance "1") build

# Full pre-push gate: ci checks (conformance in auto mode) plus docs + dep policy.
verify: fmt clippy test conformance build doc deny audit
