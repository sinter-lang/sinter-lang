# Changelog

All notable changes to Sinter are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Before 1.0, any
minor release (`0.x`) may change the language or CLI incompatibly. See
[`ROADMAP.md`](ROADMAP.md) for the path to 1.0.

## [1.0.0-rc.1] — 2026-10-26

A version to build a real project with before 1.0 is frozen. Not published
to crates.io.

### Added

- **Struct update**: `S { a: x, ..base }` takes the fields not listed from
  `base`, so holding most of a state struct is `Top { n: n, ..self }`.
- **`Self`** inside an `impl` names the struct, as a type and in struct
  values.
- **Constant arithmetic**: an integer `const` can be computed from literals
  and other constants with operators, casts, and bit slices, typed exactly as
  at run time, and declared in any order. Computed constants work as array
  lengths, memory depths, and slice bounds.
- **Tuple and nested `match` patterns**: `match (op, arg) { (Op::Add,
  Some(v)) => .., (_, None) => .. }`, `Some(Some(x))`. Exhaustiveness is
  checked properly (all four `(u1, u1)` pairs need no `_` arm), and a
  non-exhaustive match names a value no arm matches.
- `examples/fn_ergonomics.snt` puts all four through the oracle.
- **Hierarchy**: `#[module]` on a state struct makes it a Verilog module of
  its own, which its parent instantiates (`u_<field>`), so a large design
  keeps its structure in synthesis reports and waveforms. Its ports are its
  step method's parameters and its `Out` struct's fields (`out_<field>`),
  aggregates flattened as for the top module. It can hold anything a state
  struct can, including memories and other `#[module]`s. `sinter build`
  writes one `.sv` per module; the simulators still run the whole design
  flat, and the oracle checks them against the hierarchical Verilog
  (`examples/fn_hier.snt`).
- The generated Rust simulator has **`eval(&Inputs) -> Outputs`**: the
  outputs `step` would return, without committing. A Rust model of an
  external chip can read them and answer in the same cycle. `--emit rust`
  also writes a crate per `#[module]` struct, to test a block on its own.
  The oracle checks `eval` against `step` on every cycle of every example,
  and co-simulates each `#[module]` child on its own as well.

### Changed (breaking)

- **Unknown attributes are errors**, as are attributes in the wrong place
  (`#[clock]` on a free function) and arguments an attribute doesn't take.
  They used to be ignored, so a misspelling did nothing.
- `#[encoding(gray)]` and `#[encoding(johnson)]` are rejected. They used to
  give the binary encoding.
- A **`multiplex fn` waits in phase 0 while `in_valid` is 0**, so a datum
  enters only on a valid cycle. It used to ignore `in_valid` and take its
  inputs every `N + 1` cycles.
- The clock root's `pub` fields can't hold a channel, a clock-domain
  crossing, or a staged function, whose registers are internal to the
  compiler; output the values you need instead. The examples now output a
  queue's `len()` explicitly.
- An `impl` on an enum, and a method without `self`, are rejected with a
  clear message. (The first failed with "expected `E`, found `E`"; the second
  compiled but couldn't be called.)

### Fixed

- `sinter sim | head` stops printing quietly when the pipe closes, instead of
  panicking.

## [0.9.0] — unreleased

### Added

- **The Sinter Book** (`docs/book/`): installation, a tutorial that builds a
  UART transmitter, a language reference, the tools (CLI, stimulus files, the
  generated Verilog and Rust interfaces, pin maps), and appendices.
  `just book` builds it with mdBook (`just book serve` live-reloads), and CI
  builds it too. `cargo test` compiles every code sample in it, and checks
  that the samples meant to fail give the error the book shows.
- **`STABILITY.md`**: what 1.0 will keep stable (the language, the generated
  module's ports, the CLI and its input files, the generated Rust
  simulator's API) and what it won't (Verilog internals, diagnostics, the
  library crates' Rust APIs).
- The crates are ready for crates.io: license files, a README for each, and
  keywords and categories. `just package` (a `cargo publish --workspace
  --dry-run`) runs in CI.

### Fixed

- A pin map that binds an `out` pin to a private register is an error. It
  used to produce a board wrapper with the pin undriven.

## [0.8.0] — unreleased

### Added

- **A tree-sitter grammar** (`editors/tree-sitter-sinter/`) with highlight
  queries, for Neovim, Helix, Zed, and other tree-sitter editors; setup is in
  its README. `cargo test` checks it parses every corpus case and example.
- **A VS Code extension** (`editors/vscode/`): highlighting, comment
  toggling, and bracket handling for `.snt` files. `just vsix` tests its
  grammar and packages `build/sinter-<version>.vsix`. Releases attach it.

### Changed

- CI runs the oracle and a 30-program fuzz campaign against two tool sets:
  Ubuntu 24.04's Yosys 0.33 / Verilator 5.020 and oss-cad-suite's Yosys 0.61
  / Verilator 5.045. (The Verilator optimizer bug worked around in 0.7 is
  in 5.045, not 5.020.) A new `editors` job checks the tree-sitter parser
  is current and packages the VS Code extension. `just ci` runs the fuzz
  campaign too.
- The oracle retries a failed Verilator build once, since Verilator 5.020's
  parallel build occasionally fails with nothing wrong.

## [0.7.0] — unreleased

### Added

- **A compiled Rust simulator** (DESIGN §7.2): `sinter build --emit rust`
  (also in `--emit all`) writes `<out>/<module>-sim/`, a dependency-free
  crate with typed `Inputs` / `Outputs` and a `step()` per clock cycle, for
  Rust testbenches. It runs about 60× faster than `sinter sim`. The oracle
  checks it against the interpreter, and so the Verilog, on every example.
  See [`docs/generated-rust.md`](docs/generated-rust.md).
- **Several errors per run**: `sinter check` / `build` / `sim` report the
  first type error in every function and method, each with its source line
  and caret, then a count. `CompileError::Multiple` holds them, and
  `CompileError::errors()` lists them.
- **A program fuzzer** (`crates/sinter-compiler/tests/fuzz.rs`,
  `just fuzz [N] [SEED]`): random, well-typed programs mixing every scalar
  operator, register files, memories, and helper functions, each put
  through the whole oracle. A normal test run checks 40.

### Fixed

Found by the fuzzer. Each has a reduced regression case in the oracle.

- Verilog: a cast to an unsigned type of a signed value stayed signed in
  SystemVerilog, so a comparison of two such values compared signed.
- Verilog: a widening cast of an arithmetic or bitwise-not result computed
  at the wider width (`(a - b) as u16 wrap` set high bits on underflow).
- Verilog: `~` of a cast was emitted as `~8'(x)`, which Yosys rejects.
- Verilog: a module with no ports besides `clk` / `rst_n` had a trailing
  comma in its port list.
- The Verilator harness clocked once during reset, so a memory whose write
  enable is true in the reset state was written before cycle 0. It now
  resets with a falling edge of `rst_n` and no clock edge.
- Worked around a Verilator 5.045 optimizer bug that sign-extends or
  arithmetic-shifts a zero-extended variable from the variable's own top
  bit: such casts are emitted through their own wire.

## [0.6.0] — unreleased

### Removed

- **The legacy `module { in …; out …; reg …; }` surface.** Write a state
  struct with a `#[clock]` transition method instead (DESIGN §2.1); a
  `module` declaration is now a syntax error. `examples/counter.snt` is
  gone (`fn_counter.snt` is the same design), and `accumulator.snt` became
  `fn_accumulator.snt`.

### Changed

- **Diagnostics point at the offending expression**, not the enclosing
  statement: a type error in `self.value + 1` puts its caret under the `+`,
  and an unknown name under the name.
- `sinter_syntax::ast::Expr` is now a struct, `{ kind: ExprKind, pos: Pos }`;
  the variants moved to `ExprKind`. Equality ignores `pos`.
- `gen` is a reserved word, for generator functions (`gen fn`, DESIGN §4.4),
  which come after 1.0. A program using `gen` as a name must rename it.

## [0.5.0] — unreleased

### Added

- **Several clock domains** (CLK1, DESIGN §6.7 / §9.14). A state struct tags
  each field `#[domain(d)]` and has one `#[clock(domain = d, period = p)]`
  tick per domain. A tick may read and change only its own domain's state
  (other fields pass through), checked after inlining, so reading another
  domain's register is a compile error. The module gets a `clk_<d>` per
  domain and synchronizes the one `rst_n` into each. The simulator, the
  Verilator harness, `sinter sim` (rows are edge times), and VCD dumps all
  follow one edge schedule (`examples/fn_two_clocks.snt`).
- **Clock-domain crossings** (CLK2): `sync<from, to>` (a 2-flop level
  synchronizer for one bit) and `pulse<from, to>` (a toggle synchronizer for
  one-cycle events). The source tick calls `send`, the destination tick calls
  `recv` and reads `value()`, and each must be called exactly once per cycle,
  outside any `if` (`examples/fn_cdc_sync.snt`).
- `mailbox<T, from, to>` (CLK3): a req / ack handshake carrying one
  multi-bit word at a time, pushed through its `Sender` (`sender()`,
  `send(push)`) and read through its `Receiver` (`receiver()`, `recv(pop)`)
  (`examples/fn_mailbox.snt`).
- `async_fifo<T, N, from, to>` (CLK4): a dual-clock FIFO with gray-coded
  pointers, a RAM written on `from` and read on `to` (block RAM when large
  enough), and a first-word-fall-through `front()` at a word per cycle. It
  uses the same `Sender` / `Receiver` views as `mailbox`
  (`examples/fn_async_fifo.snt`).

## [0.4.0] — unreleased

### Added

- **Multi-port and byte-enable memories** (MEM2, DESIGN §9.6):
  `mem<T, N, R>` / `rom<T, N, R>` declare `R` read ports (1–4). With `R > 1`,
  `next` takes the read addresses as an array and `value()` returns an array
  of words. A memory's write enable may be a `uB` mask, one bit per byte
  lane, as well as `u1`. Yosys maps both onto block or distributed RAM
  (`examples/fn_mem2.snt`).
- **RV32: all of RV32I's loads and stores.** Both cores gain byte and halfword
  loads and stores through byte-enabled data RAM (a shared `lsu.snt`).
  Misaligned halfword / word accesses halt the core and fail a sim-only
  assertion. The pipelined core's register file moved into a
  `mem<u32, 32, 2>`, so it maps to distributed RAM: 2.2k LUTs instead of 5.2k.
- `Simulator::memory(name)` reads a memory's current contents.
- **Channel views** (CHAN3, DESIGN §6.11 / §9.8): `q.sender()` /
  `q.receiver()` return `Sender<T, N>` / `Receiver<T, N>` views of a `fifo`,
  usable as ordinary values across modules (`examples/fn_fifo_views.snt`).
- **Block-RAM FIFOs** (CHAN4, DESIGN §9.8): a `fifo` of depth 64 or more,
  or one marked `#[storage(bram)]`, is held in block RAM and behaves
  cycle-for-cycle like the register version. `#[storage(regs)]` forces
  registers. A 512 × 32 `fifo` now synthesizes to one block RAM and 261 LUTs
  instead of 67,634 LUTs. Struct fields can carry attributes.

### Changed

- **Breaking: a `fifo` push is a `Push<T>` token** (CHAN3). `fifo::next`
  takes a `Push<T>`, which only the queue's `Sender` makes (`try_push`,
  `offer`, `no_push`), each refusing the word when the queue is full, so a
  lossless queue can't be overfilled. Replace `q.next(Some(x), pop)` with
  `q.next(q.sender().try_push(x), pop)`, or `q.sender().offer(opt)` for an
  `Option`. `ring` still takes an `Option`.
- Errors name channel types as written (`Push<u8>`, `fifo<u8, 4>`) rather
  than as their synthesized structs.

## [0.3.0] — unreleased

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
- **`examples/rv32`**: two RV32I cores (DESIGN §9.12) sharing a decoder,
  ALU, and program. `mini.snt` is a fetch-ahead core (one instruction per
  cycle). `pipe.snt` is a 5-stage pipeline with forwarding, a load-use stall,
  and branches resolved in EX; it reaches 46.7 MHz on ECP5 against `mini`'s
  31.5 MHz. `tests/rv32.rs` adds a small RV32I assembler and a reference
  instruction-set simulator, and checks both cores against it over a suite
  of test programs, with and without clock-enable stalls. Both cores are
  also in the conformance oracle.

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
- The reference simulator resolves every signal to a slot once, instead of
  looking names up each cycle: about 9.5× faster (the 5-stage RV32 core runs
  ~415k cycles/s, up from ~43k).

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

[0.8.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/sinter-lang/sinter-lang/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/sinter-lang/sinter-lang/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sinter-lang/sinter-lang/releases/tag/v0.1.0
