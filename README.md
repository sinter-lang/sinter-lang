# Sinter

An experimental register-transfer-level (RTL) hardware description language,
Rust- and Rhai-inspired, that lowers **one source** to portable Verilog and a
cycle-accurate Rust simulator. The two lowerings are required to agree
bit-for-bit, cycle-for-cycle — that equivalence is the language's built-in
correctness oracle.

See [`DESIGN.md`](DESIGN.md) for the full language design (the source of truth)
and [`CLAUDE.md`](CLAUDE.md) for the coding standards every change must follow.

> **Status: v0.1 — experimental, pre-1.0.** The language and CLI may still
> change incompatibly between releases. Sinter is usable today for small-to-medium
> **single-clock** datapaths and controllers that you instantiate from
> hand-written Verilog and verify with your own testbench.

**Implemented** — every feature below lowers to both backends and passes the
sim-vs-Verilog oracle (Yosys synthesis + Verilator co-simulation) in CI:

- **Functional surface** (DESIGN §2.1): `struct` state with inline reset values,
  `impl` transition methods, a `#[clock]` root; `enum` / `match`, tuples, arrays,
  `const`s; width-inferred `uN` / `iN` arithmetic and fixed-point `uI.F` / `iI.F`;
  explicit narrowing casts `as T wrap | sat | round`.
- **Modules**: multi-file `mod` / `use` with `pub` visibility (Rust-2018 layout).
- **Staged functions**: `pipeline fn` (free-running, valid bubbles, global stall)
  and `multiplex fn` (time-folded onto one shared multiplier).
- **Storage**: `mem<T, N>` 1R1W block RAM, `rom<T, N>` (inline or
  `include_hex!`), runtime-indexed `[T; N]` register files.
- **Validity & channels**: `Option<T>` + `?`, and the register-backed
  `fifo<T, N>` / `ring<T, N>` / `flow<T>` channel family.
- **Bit manipulation**: `x[hi:lo]` slices, `a @ b` concat, constant and runtime
  (barrel) shifts.
- **Faults**: sim-only `assert!` / `debug_assert!` with a static inlined
  backtrace, and an opt-in silicon fault latch (`sinter build --faults`).
- **Boards**: a `.pins` pin map that emits a board-top wrapper + nextpnr PCF.

**Known limitations** (designed in `DESIGN.md`, not yet implemented):

- `sinter sim` drives seeded-random inputs only; there are no user-supplied
  stimulus vectors and no VCD/FST waveform output. Directed verification means
  a Verilator / cocotb testbench around the emitted SystemVerilog.
- One clock domain per design (no clock-domain types / CDC primitives).
- No way to instantiate external Verilog (PLLs, vendor primitives, IP) from
  Sinter; instead, instantiate the emitted module from a Verilog top.
- No BRAM-backed deep FIFOs, byte-enable / multi-port memories, elastic
  ready/valid `Stream<T>`, or `Sender` / `Receiver` channel views.
- No editor support (LSP / syntax highlighting) and no package / dependency
  layer yet.

The legacy `module { reg …; }` surface (`counter.snt`, `accumulator.snt`) is
frozen; new designs should use the functional surface.

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

# Reproducible builds: pin to a release tag (v0.1.1+) or a specific commit.
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --tag v0.1.1 --locked
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

### In a Docker image / devcontainer

To add `sinter` to another project's container, build it in a throwaway stage
and copy only the binary into your image. That way your image does not carry a
Rust toolchain. Pin `SINTER_REF` to a release tag so builds are reproducible.
Use `v0.1.1` or later: `v0.1.0`'s submodules use SSH URLs, so a git install of
it fails without a GitHub SSH key.

```dockerfile
# --- Sinter HDL compiler (build stage) -------------------------------------
FROM rust:1-slim-bookworm AS sinter
ARG SINTER_REF=v0.1.1
RUN cargo install --locked --root /opt/sinter \
        --git https://github.com/sinter-lang/sinter-lang.git --tag ${SINTER_REF} \
        sinter-lang

# --- your project image ----------------------------------------------------
FROM ubuntu:24.04
COPY --from=sinter /opt/sinter/bin/sinter /usr/local/bin/sinter
# ... the rest of your image (e.g. oss-cad-suite for Yosys / Verilator) ...
```

The binary needs only glibc. Building on Debian bookworm keeps it compatible
with Ubuntu 22.04+ and Debian 12+ base images. If your Dockerfile has a
single `FROM`, put the `sinter` stage above it and add the `COPY --from=sinter`
line to your stage.

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
// An 8-bit counter with a synchronous enable. The state is a struct; one call
// of the `#[clock]` method is one clock cycle.
struct Counter {
    value: u8 = 0,
}

impl Counter {
    #[clock(domain = sys)]
    fn tick(self, en: u1) -> Counter {
        Counter {
            // `value + 1` widens to u9; narrowing back to u8 must name a mode.
            value: if en == 1 { (self.value + 1) as u8 wrap } else { self.value },
        }
    }
}
```

See [`examples/`](examples) for more (`fn_*.snt` are functional-surface
examples; [`go_board_counter.snt`](examples/go_board_counter.snt) is a complete
board design).

## Development

The [`.devcontainer`](.devcontainer) provides Rust (stable + nightly),
`oss-cad-suite` (Yosys + Verilator), and [`just`](https://just.systems). The
skill submodules under [`.claude/skills`](.claude/skills) carry the Rust/Verilog
coding standards; run `git submodule update --init --recursive` to populate them.

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
