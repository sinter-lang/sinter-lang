# Sinter — Agent Guide

Sinter is a register-transfer-level HDL that lowers one source to **portable
Verilog** and a **cycle-accurate Rust simulator**. See `DESIGN.md` for the full
language design (the source of truth). This file captures the conventions every
change — hand-written *and* compiler-generated — must follow.

## Repository layout

```
crates/sinter-syntax     grammar (.pest) + AST + parser for `.snt`
crates/sinter-compiler   semantic checks + Verilog/Rust-sim/Verilator backends
crates/sinter-lang       `sinter` CLI binary (published crate: sinter-lang)
examples/                example `.snt` programs
tests/                   end-to-end conformance harness (sim ↔ Verilog oracle)
.claude/skills/          coding-standard skills (git submodules; see below)
```

- Published crate: **`sinter-lang`**; installed binary: **`sinter`**.
- Source file extension: **`.snt`**.

## Skills (submodules)

The `.claude/skills/` directory mirrors the `pico-gs` setup as git submodules.
They may be unpopulated in ephemeral/CI environments; the load-bearing rules are
restated below so they bind regardless.

- `claude-skill-rust` — Rust conventions.
- `claude-skill-verilog` — Verilog/SystemVerilog conventions for Yosys + Verilator.
- `ecp5-sv-yosys-verilator` — Yosys/nextpnr + Verilator invocation flow.
- `systemrs-modeling-skill` — TLM modeling / binary-trace methodology.

Populate them with: `git submodule update --init --recursive` (needs SSH access
to the `londey`/`systemrs` repos).

## Rust rules (apply to hand-written code AND the compiler's own source)

- Module files use `<module_name>.rs`, **never** `mod.rs`.
- Crate-level lints in `lib.rs`/`main.rs`; `#![forbid(unsafe_code)]`.
- No `.unwrap()` / `.expect()` / `panic!` on non-bug paths. Libraries return
  `Result` with `thiserror`; the binary uses `anyhow`.
- Use the `log` crate, never `println!`/`eprintln!` for diagnostics (the CLI may
  print intended program output to stdout).
- New deps: `default-features = false`, enable only what is needed.
- Verify sequence before declaring done: `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test`, `cargo build --release`,
  `cargo doc`, (when available) `cargo deny check`, `cargo audit`.

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

## Scope note

The current implementation is the **minimal synchronous core** (M1–M3 slice):
clocked modules with `uN`/`iN` registers, combinational next-state, and
single-driver + acyclicity checks. Fixed-point, width inference, `pipeline`/
`multiplex` colors, clock-domain phantom types, and memories are designed in
`DESIGN.md` but not yet implemented. Keep new work faithful to that design.

The **functional surface** (DESIGN §2.1: `struct` state + `impl` transition
methods + a `#[clock]` root, with `enum`/`match`, tuples, arrays, fixed-point
types, and mode-named casts) is **parsed** by `sinter-syntax` today — see
`examples/go_board_counter.snt` and the corpus — but semantic checking and
lowering of it are not yet implemented; `sinter-compiler` still lowers only the
legacy `module` form. The two share the AST's `Expr`; the legacy `module` parse
path is frozen (keep its goldens and the oracle bit-for-bit stable).
