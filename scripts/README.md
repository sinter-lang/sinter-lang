# `scripts/` — task scripts behind the `just` recipes

Each script here implements exactly one task. They are the single source of
truth for *how* a check runs; the [`justfile`](../justfile) at the repository
root is a thin set of recipes over them, and
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) drives the same
recipes so CI and local runs stay in lockstep.

```
just <recipe>  ->  scripts/<task>.sh  ->  the underlying cargo/oracle command
```

Run a task either way — they are equivalent:

```sh
just clippy          # via the recipe
scripts/clippy.sh    # directly (works from any directory)
```

| Script | Recipe | What it does |
|---|---|---|
| `fmt.sh` | `just fmt` | `cargo fmt --all --check` (CI: Format check) |
| `fmt-fix.sh` | `just fmt-fix` | `cargo fmt --all` (apply formatting; dev only) |
| `clippy.sh` | `just clippy` | Clippy, all targets/features, warnings denied |
| `test.sh` | `just test` | `cargo test --workspace` |
| `conformance.sh` | `just conformance` | Yosys/Verilator conformance oracle |
| `tree-sitter.sh` | `just tree-sitter [--check]` | Regenerate the editor grammar's parser (or check it is current) |
| `vscode.sh` | `just vsix` | Test the VS Code grammar and package `build/sinter-<v>.vsix` (needs Node.js) |
| `fuzz.sh` | `just fuzz [N] [SEED]` | Random programs through the whole oracle (needs Yosys + Verilator) |
| `build.sh` | `just build` | `cargo build --release` |
| `doc.sh` | `just doc` | `cargo doc` (broken doc links denied) |
| `deny.sh` | `just deny` | `cargo deny check` (skips if cargo-deny absent) |
| `audit.sh` | `just audit` | `cargo audit` (skips if cargo-audit absent) |

`lib.sh` is sourced by every script: it enables strict shell flags, pins the
working directory to the repository root, and provides the `step`/`info`/`warn`/
`have` helpers. It is not a task itself.

## Conventions

- `#!/usr/bin/env bash` + `set -euo pipefail` (via `lib.sh`).
- Diagnostics go to **stderr**; a script's stdout is the tool's own output.
- One task per file; the filename matches the recipe name.
