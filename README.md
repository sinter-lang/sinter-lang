# Sinter

An experimental register-transfer-level (RTL) hardware description language,
Rust- and Rhai-inspired, that lowers **one source** to portable Verilog and a
cycle-accurate Rust simulator. The two lowerings are required to agree
bit-for-bit, cycle-for-cycle — that equivalence is the language's built-in
correctness oracle.

See [`DESIGN.md`](DESIGN.md) for the full language design (the source of truth)
and [`CLAUDE.md`](CLAUDE.md) for the coding standards every change must follow.

> **Status: v0.5 — experimental, pre-1.0.** The language and CLI may still
> change incompatibly between releases. Sinter is usable today for small-to-medium
> datapaths and controllers, on one clock or several, that you instantiate from
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
- **Storage**: `mem<T, N, R>` block RAM with `R` read ports and byte-enable
  writes, `rom<T, N, R>` (inline or `include_hex!`), runtime-indexed
  `[T; N]` register files.
- **Validity & channels**: `Option<T>` + `?`, and the `fifo<T, N>` /
  `ring<T, N>` / `flow<T>` channel family. A `fifo` is pushed through its
  `Sender` view, so it can't be overfilled, and a deep one (depth ≥ 64, or
  `#[storage(bram)]`) lives in block RAM.
- **Clock domains**: `#[domain(d)]` state fields with one
  `#[clock(domain = d, period = p)]` tick per domain; reading another
  domain's register is a compile error. Crossings go through `sync`,
  `pulse`, `mailbox`, or a dual-clock `async_fifo`, and the simulator and
  Verilator replay the same multi-clock edge schedule.
- **Bit manipulation**: `x[hi:lo]` slices, `a @ b` concat, constant and runtime
  (barrel) shifts.
- **Faults**: sim-only `assert!` / `debug_assert!` with a static inlined
  backtrace, and an opt-in silicon fault latch (`sinter build --faults`).
- **Boards**: a `.pins` pin map that emits a board-top wrapper + nextpnr PCF.
- **Foreign Verilog**: a `#[verilog(module = "...", src = "...")]` binding
  instantiates existing Verilog IP. Its Sinter reference model runs in
  simulation, and the oracle checks it against the real module
  (`examples/verilog_acc/`).
- **RV32I cores**: [`examples/rv32/`](examples/rv32/README.md) has a
  fetch-ahead core and a 5-stage pipelined core implementing all of RV32I,
  each a few hundred lines of Sinter. Both are checked against a
  reference instruction-set simulator and through the oracle.

**Known limitations** (designed in `DESIGN.md`, not yet implemented):

- Values are at most 64 bits wide (`u65` and wider are rejected).
- Foreign Verilog bindings (EXT1) take scalar ports and default parameters
  only. Clock-generating modules (PLLs) still belong in a hand-written Verilog
  top level.
- No elastic ready/valid `Stream<T>`, and no dual-clock `mem` as a
  user-visible type (only inside `async_fifo`).
- Clocks have fixed integer periods: no derived or related clocks, clock
  gating, or multi-domain `pipeline` / `multiplex` functions. The fault
  latch (`--faults`) and `.pins` board tops are single-clock only.
- No editor support (LSP / syntax highlighting) and no package / dependency
  layer yet.

To use the emitted SystemVerilog from your own Verilog (port names, reset,
timing), see [`docs/generated-verilog.md`](docs/generated-verilog.md).

## Workspace layout

| Crate | Role |
|---|---|
| `crates/sinter-syntax` | `.snt` grammar (pest), AST, and parser |
| `crates/sinter-compiler` | semantic checks + backends (Verilog, Rust sim, Verilator harness) |
| `crates/sinter-lang` | the `sinter` CLI binary (published crate: `sinter-lang`) |

`examples/` holds example `.snt` programs; the conformance oracle lives in
`crates/sinter-compiler/tests/conformance.rs`.

## Install

To use `sinter` as a build tool in another project, either download a
prebuilt binary or build it with `cargo install`. The published crate is
`sinter-lang`; the installed binary is `sinter`.

### Prebuilt binary (v0.1.1+)

Each [release](https://github.com/sinter-lang/sinter-lang/releases) carries
fully static Linux binaries for `x86_64` and `aarch64`. They have no runtime
dependencies, so they run on any distro, including Alpine.

```sh
SINTER_VERSION=v0.5.0
mkdir -p ~/.local/bin
curl -fsSL "https://github.com/sinter-lang/sinter-lang/releases/download/${SINTER_VERSION}/sinter-${SINTER_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz" \
    | tar -xz -C ~/.local/bin sinter
```

Each tarball has a matching `.sha256` file alongside it.

### From source

```sh
# From this public repository (no SSH key needed). `--locked` pins the exact
# dependency versions from the committed Cargo.lock for a reproducible build.
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --locked

# Reproducible builds: pin to a release tag (v0.1.1+) or a specific commit.
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --tag v0.5.0 --locked
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

To add `sinter` to another project's container, fetch the prebuilt static
binary. This needs `curl` and `ca-certificates` in the image, and the version
is pinned so builds are reproducible.

```dockerfile
# --- Sinter HDL compiler ---------------------------------------------------
ARG SINTER_VERSION=v0.5.0
RUN curl -fsSL "https://github.com/sinter-lang/sinter-lang/releases/download/${SINTER_VERSION}/sinter-${SINTER_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz" \
        | tar -xz -C /usr/local/bin sinter \
    && sinter --version
```

To build from source instead (for example, to pin an untagged commit), build
`sinter` in a throwaway stage and copy only the binary into your image, so
your image does not carry a Rust toolchain. Use `v0.1.1` or later:
`v0.1.0`'s submodules use SSH URLs, so a git install of it fails without a
GitHub SSH key.

```dockerfile
# --- Sinter HDL compiler (build stage) -------------------------------------
FROM rust:1-slim-bookworm AS sinter
ARG SINTER_REF=v0.5.0
RUN cargo install --locked --root /opt/sinter \
        --git https://github.com/sinter-lang/sinter-lang.git --tag ${SINTER_REF} \
        sinter-lang

# --- your project image ----------------------------------------------------
FROM ubuntu:24.04
COPY --from=sinter /opt/sinter/bin/sinter /usr/local/bin/sinter
# ... the rest of your image (e.g. oss-cad-suite for Yosys / Verilator) ...
```

The source-built binary needs only glibc 2.34+, so it runs on Ubuntu 22.04+
and Debian 12+ base images. If your Dockerfile has a
single `FROM`, put the `sinter` stage above it and add the `COPY --from=sinter`
line to your stage.

## Quick start

Working inside this repository (devcontainer or any machine with Rust +
`oss-cad-suite`), run the CLI straight from the workspace without installing:

```sh
# Parse and type-check a source file.
cargo run -p sinter-lang -- check examples/fn_counter.snt

# Emit SystemVerilog + a Verilator C++ harness into ./build.
cargo run -p sinter-lang -- build examples/fn_counter.snt

# Run the cycle-accurate reference simulator over a deterministic stimulus.
cargo run -p sinter-lang -- sim examples/fn_counter.snt --cycles 16

# ... or over directed CSV vectors, checking expected outputs, and dump a
# VCD waveform (open it in GTKWave or Surfer).
cargo run -p sinter-lang -- sim examples/fn_counter.snt \
    --stimulus examples/fn_counter.csv --vcd build/fn_counter.vcd
```

## Directed stimulus

By default `sinter sim` drives seeded-random inputs. To test specific
behaviour, give it a CSV file with `--stimulus`. Each row is one clock cycle.

```text
# examples/fn_counter.csv: comments and blank lines are ignored.
en, expect.value_q
1,  0
1,  1
0,  2   # hold
0,  2
```

- **Columns** are input port names, or `expect.` + an output port name. The
  port names are the ones in the header of `sinter sim`'s output table. Input
  ports with no column are driven `0`.
- **Values** are decimal (negative only for signed ports), `0x` hex, or `0b`
  binary, with optional `_` separators. Each must fit its port's type.
- **Expectations** are checked against the outputs sampled that cycle. A
  register's `_q` output shows its value *before* the clock edge, so it lags
  that row's inputs by one cycle. An empty cell or `-` means "don't check".

Any mismatch is logged with its cycle and line number, and `sinter sim` exits
non-zero, so a CSV file works directly as a regression test in CI. The
conformance oracle also replays the example CSVs through Verilator, so a
directed test that passes in `sinter sim` holds for the emitted Verilog too.

`--vcd FILE` writes a waveform with the input ports, output ports, registers,
and named wires grouped under `in` / `out` / `reg` / `wire` scopes. Cycle *k*
starts at the clock's rising edge at *10k* ns, and the next rising edge samples
it. Values are raw bit patterns, so set a signed display format in your viewer
for `iN` signals.

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
    pub value: u8 = 0, // `pub`: exported as the output port `value_q`
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
