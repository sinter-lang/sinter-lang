# Sinter

An experimental register-transfer-level (RTL) hardware description language,
Rust- and Rhai-inspired, that lowers **one source** to portable Verilog and a
cycle-accurate Rust simulator. The two lowerings are required to agree
bit-for-bit, cycle-for-cycle — that equivalence is the language's built-in
correctness oracle.

See [`DESIGN.md`](DESIGN.md) for the full language design (the source of truth)
and [`CLAUDE.md`](CLAUDE.md) for the coding standards every change must follow.

> **Status: bootstrap / M1–M3 vertical slice.** This repository currently
> implements the *minimal synchronous core*: clocked modules with `uN`/`iN`
> registers, combinational next-state, and single-driver + acyclicity checks,
> wired end-to-end through the Verilog and simulator backends and tested against
> **Yosys** (synthesis) and **Verilator** (simulation). Fixed-point, width
> inference, the `pipeline`/`multiplex` function colors, clock-domain phantom
> types, and memories are designed in `DESIGN.md` but not yet implemented.

## Workspace layout

| Crate | Role |
|---|---|
| `crates/sinter-syntax` | `.snt` grammar (pest), AST, and parser |
| `crates/sinter-compiler` | semantic checks + backends (Verilog, Rust sim, Verilator harness) |
| `crates/sinter-lang` | the `sinter` CLI binary (published crate: `sinter-lang`) |

`examples/` holds example `.snt` programs; the conformance oracle lives in
`crates/sinter-compiler/tests/conformance.rs`.

## Install

To use `sinter` as a build tool in another project, install the CLI with
`cargo install`. The published crate is `sinter-lang`; the installed binary is
`sinter`.

```sh
# From this public repository (no SSH key needed). `--locked` pins the exact
# dependency versions from the committed Cargo.lock for a reproducible build.
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --locked

# Reproducible builds: pin to a specific commit (or a tag, once tagged).
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --rev <sha> --locked

# Updating a git-installed copy to a newer commit needs --force.
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --locked --force

# Or install from a local checkout of this repo.
cargo install --path crates/sinter-lang --locked
```

This puts `sinter` on your `PATH` (via `~/.cargo/bin`). From another project you
can then compile a design to SystemVerilog + a Verilator harness:

```sh
sinter check   src/cpu.snt
sinter build   src/cpu.snt --emit all --out build/rtl
sinter sim     src/cpu.snt --cycles 64
```

`sinter build` needs only Rust; the `--emit verilog` output is portable
SystemVerilog. Yosys/Verilator are needed only to *run* the correctness oracle
(below), not to install or use the compiler.

## Quick start

Working inside this repository (devcontainer or any machine with Rust +
`oss-cad-suite`), run the CLI straight from the workspace without installing:

```sh
# Parse and type-check a source file.
cargo run -p sinter-lang -- check examples/counter.snt

# Emit SystemVerilog + a Verilator C++ harness into ./build.
cargo run -p sinter-lang -- build examples/counter.snt

# Run the cycle-accurate reference simulator over a deterministic stimulus.
cargo run -p sinter-lang -- sim examples/counter.snt --cycles 16
```

## The correctness oracle

For each example the conformance harness:

1. compiles the `.snt` to SystemVerilog and a Verilator testbench,
2. runs the Rust reference model over a fixed stimulus to produce a trace,
3. synthesizes the SystemVerilog with **Yosys**, and
4. simulates it with **Verilator** over the *same* stimulus, asserting the
   output trace matches the reference bit-for-bit.

```sh
# Auto-skips steps 3–4 if Yosys/Verilator are absent.
cargo test -p sinter-compiler --test conformance

# Require the tools to be present (used in CI).
SINTER_REQUIRE_TOOLS=1 cargo test -p sinter-compiler --test conformance
```

## A taste of `.snt`

```rust
// An 8-bit free-running counter with a synchronous enable.
#[clock(domain = sys)]
module counter {
    in en: u1;
    out count: u8;

    reg value: u8 = 0;

    value <= if en { value + 1 } else { value };
    count = value;
}
```

## Development

The [`.devcontainer`](.devcontainer) provides Rust (stable + nightly),
`oss-cad-suite` (Yosys + Verilator), and [`just`](https://just.systems). The
skill submodules under [`.claude/skills`](.claude/skills) carry the Rust/Verilog
coding standards; run `git submodule update --init --recursive` to populate them
(needs SSH access).

### Task runner

All checks and builds are exposed as [`just`](https://just.systems) recipes,
each a thin wrapper over a script in [`scripts/`](scripts). CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) drives the same
recipes, so `just ci` reproduces the GitHub pipeline locally, step for step.

```sh
just              # list every recipe
just check        # fast inner-loop gate: fmt + clippy + test
just ci           # reproduce the CI pipeline exactly (conformance enforced)
just verify       # full pre-push gate: ci + docs + license/advisory checks
just fmt-fix      # apply rustfmt formatting in place
```

Individual checks are recipes too — `just fmt`, `just clippy`, `just test`,
`just conformance`, `just build`, `just doc`, `just deny`, `just audit`.

## License

Licensed under either of MIT or Apache-2.0 at your option.
