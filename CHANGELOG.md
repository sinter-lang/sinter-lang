# Changelog

All notable changes to Sinter are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Before 1.0, any
minor release (`0.x`) may change the language or CLI incompatibly. See
[`ROADMAP.md`](ROADMAP.md) for the path to 1.0.

## [Unreleased]

### Added

- **Sign-changing casts** (DESIGN §6.3): `uN` ↔ `iN` in `wrap` (modular)
  and `sat` (clamping) modes. Signed compare, arithmetic shift, and sign
  extension of unsigned fields now work, which an RV32 datapath needs.
- A bare literal under a cast takes the cast's type (`0 as u18 wrap`).
- **Array repeat** `[value; N]` (DESIGN §6.6), e.g. `[0; 32]`.
- Array lengths, repeat counts, and memory depths may name a literal-valued
  `const` (`[u8; DEPTH]`, `mem<u8, DEPTH>`), not only a literal.
- A `rom` initializer (inline array or `include_hex!`) may hold fewer words
  than the ROM's depth; the rest are zero-filled.
- **`examples/rv32_mini`**: an RV32I core (DESIGN §9.12 step 5): Harvard,
  word-only loads and stores, fetch-ahead with one instruction per cycle.
  `tests/rv32.rs` adds a small RV32I assembler and a reference instruction-set
  simulator, and checks the core against it over a suite of test programs,
  with and without clock-enable stalls. The core is also in the conformance
  oracle.

### Changed

- **Breaking: only `pub` state fields are output ports.** Previously every
  register of the clock-root state struct became a `<reg>_q` port (the RV32
  core had 42 ports). Now only the leaves of its `pub` fields do; private
  fields are internal registers. Mark the fields you want exported `pub`.
  `sinter build` warns about a module with no outputs. The examples are
  updated; go_board_counter now exports just `seg` and `led`.
- **Shared wires.** Each non-trivial `let` binding and call argument now
  becomes a named wire, instead of being copied into every use. On the RV32
  core this shrank the emitted Verilog from 1.2 MB to 28 KB, made Yosys
  synthesis 28× faster (11.2 s to 0.4 s), and halved simulation time. Wires
  keep their source names (suffixed with `_w` if the name is taken or is a
  SystemVerilog keyword). They appear in VCD dumps, and Verilog emits only
  those that hardware reads.
- A `multiplex fn` can now be combined with `#[verilog]` instances.

### Fixed

- Width-suffixed literals (`0b0111111_u7`) now carry their type and are
  range-checked, as DESIGN §6.10 specifies. Previously the suffix was
  silently dropped, so `0_u18` could not be typed, and inside a concat it
  took the width of its context. They also fold as `const` values and
  register resets. A suffixed literal in a legacy `module` body is now
  rejected rather than ignored.
- The legacy surface's keywords `module`, `in`, `out`, and `reg` are now
  contextual, so a functional-surface field or parameter may be called `out`
  or `in`. (`reg` is still rejected as a name, because it is a SystemVerilog
  keyword.)
- A register or port named after a SystemVerilog keyword (`end`, `begin`,
  …) produced invalid Verilog. It is now a compile error.

## [0.2.0] — unreleased

### Added

- **Directed stimulus**: `sinter sim --stimulus vectors.csv` drives inputs
  from a CSV file (one row per cycle) and checks optional `expect.<output>`
  columns. Any mismatch fails the run. The conformance oracle replays the
  example CSVs through Verilator as well.
- **Waveforms**: `sinter sim --vcd out.vcd` writes a VCD of the inputs,
  outputs, registers, and wires.
- **Interface documentation**: `docs/generated-verilog.md` specifies the
  generated module's port names, ordering, reset, and timing, and
  `tests/interface.rs` pins those rules.
- **Foreign Verilog modules** (EXT1, DESIGN §6.15): a
  `#[verilog(module = "...", src = "...")]` attribute on a state struct binds
  it to an existing Verilog module, and the struct's `step` method is its
  Sinter reference model. `sinter sim` runs the model, `sinter build` emits an
  instance of the real module and copies its source, and the conformance
  oracle checks the model against the Verilog (`examples/verilog_acc/`).
  Attribute values may now be strings, and attribute keys may be keywords.
- Emitted Verilog comments each fixed-point port and register with its
  Q-format (`// Q4.4 signed fixed-point: value = raw / 2^4`).

### Changed

- Semantic errors name the source **file** (not the item), quote the
  offending line, and put a caret under the position. In a multi-file
  project they now point at the right file.
- `sinter sim` prints signed ports as signed decimals (e.g. `-8`, not `8`
  for an `i4`).

## [0.1.1] — unreleased

### Added

- **Faults** (DESIGN §6.12 / §9.9):
  - Sim-only `assert!(cond, "msg")` / `debug_assert!`, checked by the
    reference simulator each cycle and emitting zero hardware (FLT0).
  - Each assert failure reports a static backtrace of the inlined call path
    (FLT1a).
  - `sinter build --faults` adds an opt-in silicon fault latch: a sticky
    bitmap, a priority-encoded `fault_code` sideband output, and a
    `<module>.faults.json` code-to-backtrace table (FLT1b).
- **Release binaries**: tagging `v*` publishes static `x86_64` / `aarch64`
  Linux (musl) binaries to GitHub Releases.
- README: Docker / devcontainer install snippets.

### Fixed

- `cargo install --git` (and Docker builds that use it) failed without a
  GitHub SSH key, because the skill submodules used SSH URLs. They now use
  HTTPS.

## [0.1.0] — 2026-07-18

The first tagged release. One `.snt` source lowers to SystemVerilog and to a
cycle-accurate Rust reference simulator, and the two agree bit for bit under
the Yosys + Verilator conformance oracle.

- Functional surface: `struct` state + `impl` transition methods + `#[clock]`
  root, `enum` / `match`, tuples, arrays, `uN` / `iN`, fixed-point, and
  mode-named narrowing casts (`wrap` / `sat` / `round`).
- Multi-file `mod` / `use` with `pub` visibility.
- `pipeline fn` and `multiplex fn`.
- `mem<T, N>`, `rom<T, N>` with `include_hex!`, and runtime-indexed `[T; N]`
  register files.
- `Option<T>` with `?`; the `fifo` / `ring` / `flow` channel family.
- Bit slices, concat, and constant and runtime shifts.
- `.pins` pin maps that emit a board-top wrapper and a nextpnr PCF.
- The legacy `module` surface (frozen).

[0.2.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/sinter-lang/sinter-lang/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sinter-lang/sinter-lang/releases/tag/v0.1.0
