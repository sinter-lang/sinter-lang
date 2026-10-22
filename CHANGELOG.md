# Changelog

All notable changes to Sinter are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Before 1.0, any
minor release (`0.x`) may change the language or CLI incompatibly. See
[`ROADMAP.md`](ROADMAP.md) for the path to 1.0.

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

[0.1.1]: https://github.com/sinter-lang/sinter-lang/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sinter-lang/sinter-lang/releases/tag/v0.1.0
