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
- [ ] Tag `v0.1.1` on `master` and confirm the release assets and the README
      snippets work.

## v0.2 — usable in a real project

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

## v0.3 — dogfood

Build real things and fix what breaks.

- [ ] RV32I stepping-stone core (DESIGN §9.12 step 5): single-cycle, Harvard,
      word-only loads / stores.
- [ ] Full 5-stage RV32I core with hazard detection + forwarding (§9.12
      step 6).
- [ ] At least one design outside this repo built with `sinter`.
- [ ] `go_board_counter` brought up on real hardware (iCE40 via nextpnr).
- [ ] Compile time and simulator speed are acceptable on the largest design.

## v0.4 – v0.6 — the 1.0 language scope

Implement what 1.0 needs, and explicitly defer the rest.

**Planned for 1.0:**

- [ ] Multiple clock domains: clock-domain types + CDC primitives (§6.7).
- [ ] Byte-enable / multi-port `mem` (§9.6). RV32 `SB` / `SH` needs it.
- [ ] BRAM-backed deep FIFO (CHAN3–4, §9.8).
- [ ] Directional `Sender` / `Receiver` channel views (§6.11).
- [ ] Settle every DESIGN §11 open question that affects syntax (generator
      signature §4.4, parser strategy §7.1).
- [ ] Remove the legacy `module` surface.
- [ ] Decide whether 1.0 lifts the 64-bit value limit (the IR and simulator
      hold every value in a `u64`).

**Deferred past 1.0** (additive, so they don't break the 1.0 promise):

- Elastic `Stream<T>` with skid buffers.
- `Bounded` / `Dynamic` latencies and the §3.5 latency prover.
- FLT2: per-file fault enables, `assert_static!`, `unreachable!()` proofs.
- External packages / dependencies (§10).
- `include_bin!`, inferred-depth ROMs, non-scalar memory elements.
- Native place-and-route (§7.5).

## v0.9 — release candidate

- [ ] Language reference and tutorial written for users, separate from
      DESIGN.md.
- [ ] tree-sitter grammar + VS Code syntax highlighting (a full LSP can follow
      1.0).
- [ ] **Diagnostics: expression positions**: carets point at the offending
      expression rather than the enclosing statement or block (expressions
      carry no `Pos` today). Do this with the §7.1 parser-strategy decision.
- [ ] **Diagnostics: multiple errors**: one run reports more than the first
      error.
- [ ] Program fuzzer: generate random valid `.snt` programs and run each
      through the sim-vs-Verilog oracle.
- [ ] CI against more than one Yosys / Verilator version.
- [ ] Publish `sinter-syntax`, `sinter-compiler`, `sinter-lang` to crates.io.
- [ ] Written stability / semver policy.

## 1.0 exit criteria

- [ ] The RV32 core and at least one outside design run on real hardware.
- [ ] The fuzzer has run clean for a sustained period with no open
      oracle mismatches.
- [ ] The language reference covers every 1.0 feature.
- [ ] No open DESIGN question would change 1.0 syntax.
