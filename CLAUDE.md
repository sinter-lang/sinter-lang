# Sinter — Agent Guide

Sinter is a register-transfer-level HDL that lowers one source to **portable
Verilog** and a **cycle-accurate Rust simulator**. See `DESIGN.md` for the full
language design (the source of truth). This file captures the conventions every
change — hand-written *and* compiler-generated — must follow.

## Repository layout

```
crates/sinter-syntax     grammar (.pest) + AST + parser + multi-file loader for `.snt`
crates/sinter-compiler   semantic checks + elaboration + Verilog/Rust-sim/Verilator backends
crates/sinter-lang       `sinter` CLI binary (published crate: sinter-lang)
examples/                example `.snt` programs (also the oracle corpus)
scripts/                 task scripts wrapped by the justfile (shared by dev shell + CI)
.claude/skills/          coding-standard skills (git submodules; see below)
```

- Published crate: **`sinter-lang`**; installed binary: **`sinter`**.
- Source file extension: **`.snt`**.
- The conformance oracle harness is `crates/sinter-compiler/tests/conformance.rs`
  (there is **no** top-level `tests/`); parser goldens live in
  `crates/sinter-syntax/tests/corpus/`.

## Building, running, and the task runner

The `just` recipes wrap `scripts/*.sh` so the same commands run in a dev shell,
the devcontainer, and CI (`just ci` reproduces the GitHub pipeline step for step):

- `just check` — fast inner loop: `fmt` + `clippy` + `test`.
- `just verify` — full pre-push gate: the above + `build` + `doc` + `deny` +
  `audit`, with the conformance oracle in `auto` mode.
- `just conformance 1` — run the oracle with the tools **required** (fails if
  Yosys / Verilator are missing); `auto` (default) skips them when absent.
  Equivalent to `SINTER_REQUIRE_TOOLS=1 cargo test -p sinter-compiler --test
  conformance`. The tools live at `/opt/oss-cad-suite/bin` in the devcontainer.
- `just verilog` — emit SystemVerilog for every example into `build/verilog/`
  (git-ignored); `just board` emits the board-top wrapper + nextpnr PCF.

The `sinter` CLI has three subcommands (a `.snt` path is compiled as the root of
a possibly-multi-file project — sibling `mod`s are pulled in automatically):

- `sinter check <file.snt>` — parse + type-check, report errors.
- `sinter build <file.snt> --emit verilog|harness|all [--out DIR] [--pins F.pins]`
  — emit backend artifacts (default out dir `build/`).
- `sinter sim <file.snt> [--cycles N --seed S | --stimulus F.csv] [--trace
  trace.bin] [--vcd F.vcd]` — run the cycle-accurate reference simulator over
  random or directed CSV stimulus (`stimulus.rs`); `expect.<output>` columns
  are checked; `--vcd` dumps a waveform (`vcd.rs`).

## Compiler pipeline (source → IR)

One source lowers to one `ir::Design` (`compile_project` / `compile_str` in
`crates/sinter-compiler/src/lib.rs`), consumed identically by the sim and Verilog
backends. The stages, in order:

1. **parse** (`sinter-syntax`) → `Program`; multi-file projects first go through
   `load_project` + `resolve` (cross-module names + visibility) into one flat
   program.
2. **`include_expand`** — replace `include_hex!("f.hex")` with an array literal
   (compile-time file read, resolved relative to the source `.snt`).
3. **staged-color desugars** — `pipeline_desugar` then `multiplex_desugar` rewrite
   `pipeline fn` / `multiplex fn` into a synthesized instance `struct` + `advance`
   method, so the rest of the pipeline treats them as ordinary functional items.
4. **type checking** — `functional::check_functional` (functional surface) +
   `check::check_program` (legacy `module` + single-driver / acyclicity).
5. **`elaborate`** — lower the `#[clock]` transition root to an `ir::Module` by
   symbolic inlining + SROA (scalar-replacement of aggregates). Memories/ROMs are
   held opaque (`ir::Mem`, never SROA-flattened).
6. **`ir_cse::share_muls`** (multiplex only) — collapse the desugar's shared,
   SROA-duplicated multiply into one physical multiplier.

**Gotcha when adding an AST `Expr` / `Pattern` / `TypeRef` variant:** every stage
above matches it exhaustively, and so do `pipeline_desugar`, `multiplex_desugar`,
`include_expand`, `functional`, and `elaborate`. A new variant needs an arm in
each — rustc's non-exhaustive-match errors enumerate exactly where.

## Skills (submodules)

The `.claude/skills/` directory mirrors the `pico-gs` setup as git submodules.
They may be unpopulated in ephemeral/CI environments; the load-bearing rules are
restated below so they bind regardless.

- `claude-skill-rust` — Rust conventions.
- `claude-skill-verilog` — Verilog/SystemVerilog conventions for Yosys + Verilator.
- `ecp5-sv-yosys-verilator` — Yosys/nextpnr + Verilator invocation flow.
- `systemrs-modeling-skill` — TLM modeling / binary-trace methodology.

Populate them with: `git submodule update --init --recursive` (public
HTTPS URLs, so no SSH key is needed).

## Rust rules (apply to hand-written code AND the compiler's own source)

- Module files use `<module_name>.rs`, **never** `mod.rs`.
- Crate-level lints in `lib.rs`/`main.rs`; `#![forbid(unsafe_code)]`.
- No `.unwrap()` / `.expect()` / `panic!` on non-bug paths. Libraries return
  `Result` with `thiserror`; the binary uses `anyhow`.
- Use the `log` crate, never `println!`/`eprintln!` for diagnostics (the CLI may
  print intended program output to stdout).
- New deps: `default-features = false`, enable only what is needed.
- Verify before declaring done: `just verify` (fmt-check, clippy `-D warnings`,
  test, release build, doc, and — when available — `cargo deny` / `cargo audit`),
  and run the oracle with the tools enforced (`just conformance 1`) whenever a
  change can affect lowering. The equivalent raw commands are `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build
  --release`, `cargo doc`.

## Generated-Verilog rules (the compiler MUST emit code that satisfies these)

The Verilog backend is held to the `claude-skill-verilog` standard:

- Start every file with `` `default_nettype none ``.
- One module per file; filename matches module name; named port connections only.
- `always_ff` contains **only** simple non-blocking (`<=`) assignments;
  all combinational logic lives in `always_comb` / `assign`.
- One declaration and one statement per line, with **explicit** bit widths.
- Active-low signals carry an `_n` suffix (e.g. `rst_n`).
- Comment modules/ports/registers; document fixed-point with Q-notation.
- Use enums for FSM state encoding.
- Avoid (not Yosys-synthesizable): `return` in functions, `interface`/`modport`,
  `unique`/`priority case`, multi-dimensional packed arrays in ports.
- Must pass Verilator lint with **no** suppression pragmas, and synthesize under
  Yosys without errors.

## Correctness oracle (CI from day one — DESIGN §7.4)

The Rust simulator and the Verilog are two lowerings of one source and must
agree **bit-for-bit, cycle-for-cycle**. The conformance harness:

1. compiles a `.snt` example to SystemVerilog,
2. runs the Rust cycle-accurate model over a fixed stimulus → reference trace,
3. (if `yosys` present) synthesizes the SV to confirm it is synthesizable,
4. (if `verilator` present) simulates the SV over the **same** stimulus and
   diffs its trace against the reference — any mismatch fails the build.

Steps 3–4 auto-skip when the tools are absent (e.g. this sandbox) and run in the
devcontainer / CI where `oss-cad-suite` provides Yosys and Verilator.

## Implementation status

Both source surfaces lower to one IR (`ir::Design` / `Module`) and pass the §7.4
oracle. Examples named `fn_*.snt` exercise the functional surface; bare-named ones
(`counter`, `accumulator`) are the legacy `module` form.

- **Functional surface** (DESIGN §2.1 — the primary, actively-developed surface):
  `struct` state + `impl` transition methods + a `#[clock]` root, with `enum` /
  `match`, tuples, arrays, `uN` / `iN` and fixed-point types, and mode-named
  width-fixing casts (`as T wrap` / `sat` / `round`). Type-checked in
  `functional.rs`, lowered by `elaborate.rs`. Landed milestones:
  - **F1–F5** — resolution + typecheck → elaborate → enums/match/methods/nested
    state → fixed-point → pin-map board-top wrapper (`pinmap.rs`).
  - **Modules** (S1–S3, §9.2 / §10) — multi-file `mod` / `use`, cross-module
    resolution + visibility (`sinter-syntax` loader + `resolve.rs`).
  - **Pipelines** (PL3, §3.1 / §9.3) — `pipeline fn`: free-running, `in_valid`
    bubbles, global-stall backpressure (`pipeline_desugar.rs`).
  - **Multiplex** (M1, §3.2 / §9.5) — `multiplex fn`: time-folded, one shared
    multiplier via operand-hoist + IR multiply-CSE (`multiplex_desugar.rs` +
    `ir_cse.rs`).
  - **Memory** (MEM1, §9.6) — `mem<T, N>` 1R1W synchronous block RAM,
    `value()` / `next(raddr, we, waddr, wdata)`, READ_FIRST, zero-init.
  - **ROM** (ROM1, §9.7) — `rom<T, N>` read-only, `value()` / `next(raddr)`,
    contents from an inline array literal or `include_hex!("f.hex")`.
  - **Option** (O1–O2, §6.5 / §9.4) — `Option<T>`, `Some` / `None`, `match`, and
    the postfix `?` validity-propagation operator.
  - **Channels** (CHAN1–CHAN2, §6.11 / §9.8) — the fixed-capacity channel family,
    all register-backed source-to-source desugars (`channel_desugar.rs`): the
    lossless `fifo<T, N>` and the overwrite-oldest `ring<T, N>` share one queue
    template (`front()` / `is_full()` / `is_empty()` / `len()` / `next(push, pop)`,
    differing only in the `pushf` / `shiftf` flags), and the depth-1 newest-wins
    `flow<T>` (`value()` / `next(push)`, no pop) erases to an `Option<T>` register.
    The loss policy is in the type, never a call-site flag.
  - **Bit manipulation** (BIT1–BIT2, §6.13 / §9.10) — a **literal**-amount `<<` /
    `>>` (widen / narrow, arithmetic for signed), bit-slice `x[hi:lo]`, and concat
    `a @ b`, all lowered to existing IR shift / cast / or nodes; plus a **runtime**
    (barrel) shift (BIT2) — a same-width `ir::VarShift` → native Verilog shift, the
    `SLL` / `SRL` / `SRA` primitive.
  - **Register files** (REG1, §6.14 / §9.11) — runtime-indexed `[T; N]`
    vector-of-registers: read `a[i]` (combinational N:1 mux tree, any number of
    read ports) and functional-update write `a.set(i, v)` (1:N guarded-hold
    decode); `functional` + `elaborate` only (no new syntax).
  - **Faults** (FLT0–FLT1b, §6.12 / §9.9) — a **location-based** fault model
    (not the earlier enum-`FaultSet` sketch): a fault is identified by *where* it
    fires, the compiler names the site, and the "call stack" falls out of
    elaboration's inlining. FLT0: sim-only `assert!(cond, "msg")` /
    `debug_assert!` — a `u1` predicate the reference sim checks each cycle
    (recorded as an `AssertFailure`), emitting **zero** bits in either backend
    (byte-identical SV with/without it); rejected in `if`/`match` arms and staged
    (`pipeline`/`multiplex`) fns. FLT1a: each assert carries a **static
    backtrace** — a `frames` push/pop stack threaded through the `elaborate`
    inliner records the inlined call path (`Top -> Inner::guard@16 (line 6)`,
    each `@N` a call-site line). The CLI `sim` reports failures and exits
    non-zero. FLT1b: the **silicon latch** (`fault_latch.rs`) — a gated
    IR-construction pass (built from the same assert sites, no new backend code)
    synthesizes a sticky one-hot bitmap register + a priority-encoded `fault_code`
    sideband output + an offline `<module>.faults.json` table, gated by a global
    `--faults on|off` (`compile_str_faults` / CLI `build --faults`). `off` is
    byte-identical to the assert-free module; `on` co-simulates the sideband
    bit-for-bit (`fn_fault`).
- **Legacy `module` surface** — the original minimal synchronous core (clocked
  `uN` / `iN` registers, combinational next-state, single-driver + acyclicity).
  **Frozen**: keep its goldens and the oracle bit-for-bit stable; put new work on
  the functional surface. Both surfaces share the AST's `Expr`.

Designed in `DESIGN.md` but **not yet implemented** (keep new work faithful to it):
directional `Sender`/`Receiver` views + deep BRAM-backed FIFO (CHAN3–4, §6.11 /
§9.8); the fault **FLT2** tier (§6.12 / §9.9) — per-file fault enable (a
`#[faults(mode)]` attribute or build-time file list; FLT1b ships the global knob
only) and the `assert_static!` / `unreachable!()` SMT-prover bucket;
byte-enable / multi-port `mem` (the BRAM register-file / sub-word-store axis, §9.6);
the RV32 flagship soft core that composes them (§9.12); elastic `Stream<T>`
(per-stage ready/valid + skid buffers),
clock-domain phantom types (§6.7), multi-port / dual-clock / non-scalar-element
memories, `include_bin!` and inferred-depth ROMs, and data-dependent (`Bounded` /
`Dynamic`) latencies with the §3.5 latency prover.
