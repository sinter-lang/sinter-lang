# Roadmap to 1.0

This file covers **release sequencing**: what has to be true before each
release. [`DESIGN.md`](DESIGN.md) remains the source of truth for the
language itself, and [`CHANGELOG.md`](CHANGELOG.md) records what shipped.

**1.0 promises** that within 1.x, existing designs keep compiling to the same
hardware. Concretely, these are stable:

- the surface syntax and semantics of the functional surface,
- the `sinter` CLI (subcommands, flags, exit codes),
- the port interface of the generated module (clock / reset / port naming),
  so hand-written Verilog that instantiates it keeps working.

Until 1.0, every `0.x` minor release may break any of these.

---

## v0.1.1 — installable

- [x] Skill submodules use HTTPS, so `cargo install --git` works without SSH.
- [x] README reflects the implemented feature set and known limitations.
- [x] Docker / devcontainer install snippets.
- [x] Release workflow: tagging `v*` publishes static x86_64 / aarch64 musl
      binaries.
- [x] CHANGELOG.
- [ ] Tag `v0.1.1` (the "release: v0.1.1" commit) and confirm the release
      assets and the README snippets work.

## v0.2 — usable in a real project ✅

Close the gaps that force workarounds today.

- [x] **Directed stimulus**: `sinter sim --stimulus` reads CSV input vectors
      with optional expected outputs, replayed through the oracle too.
- [x] **Waveforms**: `sinter sim --vcd` writes VCD.
- [x] **Verilog interop** (EXT1, DESIGN §6.15 / §9.13): instantiate an
      existing Verilog module through a `#[verilog]` binding whose Sinter
      reference model the oracle checks against the real Verilog.
- [x] **Documented module interface**: clock / reset naming, port naming for
      nested structs and arrays, and reset behaviour, written down
      ([docs/generated-verilog.md](docs/generated-verilog.md)) and pinned by
      `tests/interface.rs`.
- [x] **Q-format port comments**: fixed-point ports and registers are
      commented with their Q-notation in the emitted Verilog.
- [x] **Diagnostics: file + caret**: semantic errors name the file, quote the
      line, and put a caret under the position.

Release: tag `v0.2.0` on the "release: v0.2.0" commit.

## v0.3 — dogfood

Build real things and fix what breaks. Release: tag `v0.3.0` on the "release:
v0.3.0" commit. The two unchecked items below need hands-on work and can
land in a later 0.3.x.

- [x] RV32I stepping-stone core (DESIGN §9.12 step 5): `examples/rv32/mini.snt`,
      Harvard, word-only loads / stores, checked against a reference ISS
      (`tests/rv32.rs`) and through the oracle.
- [x] Full 5-stage RV32I core with hazard detection + forwarding (§9.12
      step 6): `examples/rv32/pipe.snt`, same ISS suite and oracle, 46.7 MHz
      on ECP5 against the stepping stone's 31.5 MHz.
- [ ] At least one design outside this repo built with `sinter`.
- [ ] `go_board_counter` brought up on real hardware (iCE40 via nextpnr).
- [x] Compile time and simulator speed are acceptable on the largest design:
      the 5-stage RV32 core compiles in ~5 ms and simulates at ~415k cycles/s.
      DESIGN §7.2's generated-Rust simulator (compiled, Verilator-class speed)
      remains future work.

## v0.4 — memories ✅

The language features the RV32 cores showed were missing. Release: tag
`v0.4.0` on the "release: v0.4.0" commit.

- [x] Multi-read-port `mem` (§9.6), so a 2R1W register file lives in block RAM.
      The pipelined core's register file is a `mem<u32, 32, 2>` and the core
      shrank from 5.2k to 2.2k LUTs.
- [x] Byte-enable `mem` writes (§9.6), and RV32 byte / halfword loads and
      stores (`LB` / `LH` / `LBU` / `LHU` / `SB` / `SH`) in both cores.
- [x] BRAM-backed deep FIFO (CHAN4, §9.8): a storage choice, cycle-for-cycle
      identical to the register `fifo`.
- [x] Directional `Sender` / `Receiver` channel views (CHAN3, §6.11), with a
      `Push<T>` token so a lossless `fifo` can't be overfilled.

## v0.5 — clock domains ✅

Release: tag `v0.5.0` on the "release: v0.5.0" commit.

- [x] CLK1: several clock domains in one design, with the domain rule checked
      (§6.7 / §9.14).
- [x] CLK2: `sync` and `pulse` synchronizers.
- [x] CLK3: `mailbox` handshake register.
- [x] CLK4: `async_fifo` (dual-clock FIFO).

## v0.6 — cleanup ✅

Release: tag `v0.6.0` on the "release: v0.6.0" commit.

- [x] Settle every DESIGN §11 open question that affects syntax: generators
      return a `Step` sum and are declared `gen fn` (post-1.0, `gen` reserved
      now, §4.4); the compiler keeps its pest parser and editors get a
      tree-sitter grammar (§7.1). §3.5 is deferred past 1.0 and changes no
      1.0 syntax.
- [x] Remove the legacy `module` surface.
- [x] **Diagnostics: expression positions**: carets point at the offending
      expression (a binary operation at its operator) rather than the
      enclosing statement or block. Moved up from v0.9 with the §7.1 decision.

**Decided for 1.0:** values stay at most 64 bits wide (the IR and simulator
hold every value in a `u64`); wider values are post-1.0.

**Deferred past 1.0** (additive, so they don't break the 1.0 promise):

- Elastic `Stream<T>` with skid buffers.
- `Bounded` / `Dynamic` latencies and the §3.5 latency prover.
- FLT2: per-file fault enables, `assert_static!`, `unreachable!()` proofs.
- External packages / dependencies (§10).
- `include_bin!`, inferred-depth ROMs, non-scalar memory elements.
- Values wider than 64 bits.
- Native place-and-route (§7.5).

## v0.7 — engine

- [x] **Generated-Rust simulator** (DESIGN §7.2): `sinter build --emit rust`
      produces a standalone crate with a typed `step()` API for Rust
      testbenches, cross-checked against the interpreter (and so the Verilog)
      on every oracle case. About 60× the interpreter's speed.
- [x] Program fuzzer: generate random valid `.snt` programs and run each
      through the whole oracle (`tests/fuzz.rs`, `just fuzz`). It found
      five Verilog backend bugs and a Verilator optimizer bug, all fixed or
      worked around.
- [x] **Diagnostics: multiple errors**: one run reports the first type error
      in every function and method, not just the first overall (parsing still
      stops at the first syntax error, §7.1).

## v0.8 — tooling

- [ ] tree-sitter grammar (`editors/tree-sitter-sinter/`) with highlight
      queries, tested against the parser corpus (§7.1).
- [ ] VS Code syntax highlighting (`editors/vscode/`): a TextMate grammar and
      language configuration, packaged as a `.vsix`. A full LSP can follow
      1.0.
- [ ] CI against more than one Yosys / Verilator version.

## v0.9 — release candidate

- [ ] Language reference and tutorial written for users, separate from
      DESIGN.md: an mdBook under `docs/book/`, its examples compiled by a test.
- [ ] Written stability / semver policy.
- [ ] Publish `sinter-syntax`, `sinter-compiler`, `sinter-lang` to crates.io.

## 1.0 exit criteria

- [ ] The RV32 core and at least one outside design run on real hardware.
- [ ] The fuzzer has run clean for a sustained period with no open
      oracle mismatches.
- [ ] The language reference covers every 1.0 feature.
- [ ] No open DESIGN question would change 1.0 syntax.
