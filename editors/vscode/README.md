# Sinter for VS Code

Syntax highlighting and editing support for
[Sinter](https://github.com/sinter-lang/sinter-lang), a register-transfer-level
hardware description language that compiles to SystemVerilog and a
cycle-accurate Rust simulator. Files ending in `.snt` get:

- highlighting of keywords, types (`u8`, `i4.12`, `mem<T, N>`, `fifo<T, N>`,
  ...), literals with width suffixes (`0x1f_u8`), casts (`as u8 wrap`),
  attributes (`#[clock(domain = sys)]`), and `assert!` / `include_hex!`;
- comment toggling, bracket matching and auto-closing, and indentation.

There is no language server yet, so no diagnostics or go-to-definition; run
`sinter check` for those.

## Install

Download `sinter-<version>.vsix` from a
[release](https://github.com/sinter-lang/sinter-lang/releases), or build it
(below), then:

```sh
code --install-extension sinter-<version>.vsix
```

## Build

From the repository root, with Node.js 22 or later:

```sh
just vsix    # tests the grammar, then writes build/sinter-<version>.vsix
```

The grammar is `syntaxes/sinter.tmLanguage.json`, and
`test/highlight.test.snt` pins its scopes (`vscode-tmgrammar-test`). When the
language gains syntax, update it here and in the tree-sitter grammar
(`editors/tree-sitter-sinter`).
