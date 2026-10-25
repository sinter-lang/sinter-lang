# tree-sitter-sinter

A [tree-sitter](https://tree-sitter.github.io/) grammar for Sinter (`.snt`),
for syntax highlighting and structural editing in Neovim, Helix, Zed, and
other tree-sitter editors. For VS Code, see [`../vscode`](../vscode).

The compiler doesn't use this grammar: it parses with its own pest grammar
(`crates/sinter-syntax/src/grammar/sinter.pest`), which is the normative one
(DESIGN §7.1). This grammar follows it, and `cargo test -p tree-sitter-sinter`
checks that it parses every program in the parser's test corpus and every
example without an error node. It's a little more permissive than the
compiler: for example, it accepts any `Name<...>` as a type and leaves the
built-in names (`mem`, `fifo`, `Option`, ...) to the highlight queries.

The generated parser in `src/` is committed, so editors can build it with
just a C compiler.

## Files

- `grammar.js`: the grammar.
- `queries/highlights.scm`: highlight captures, using the common capture names
  (`@keyword`, `@type.builtin`, `@function.call`, ...).
- `src/`: the generated parser. Don't edit it; regenerate it.
- `bindings/rust/`: a Rust binding (`tree_sitter_sinter::LANGUAGE`), used by
  the tests.

## Changing the grammar

Edit `grammar.js`, then regenerate `src/` with the pinned tree-sitter CLI
(v0.27.1, in the devcontainer) and commit both:

```sh
just tree-sitter            # regenerate
just tree-sitter --check    # fail if src/ is stale (CI runs this)
cargo test -p tree-sitter-sinter
```

When the language gains syntax, add it here and in the VS Code grammar along
with the pest grammar; a new corpus case or example the grammar can't parse
fails the test.

## Using it

### Neovim (nvim-treesitter)

```lua
local parsers = require("nvim-treesitter.parsers").get_parser_configs()
parsers.sinter = {
  install_info = {
    url = "https://github.com/sinter-lang/sinter-lang",
    location = "editors/tree-sitter-sinter",
    files = { "src/parser.c" },
    branch = "master",
  },
  filetype = "sinter",
}
vim.filetype.add({ extension = { snt = "sinter" } })
```

Then run `:TSInstall sinter`, and copy `queries/highlights.scm` to
`~/.config/nvim/queries/sinter/highlights.scm`.

### Helix

In `~/.config/helix/languages.toml`:

```toml
[[language]]
name = "sinter"
scope = "source.sinter"
file-types = ["snt"]
comment-token = "//"
block-comment-tokens = { start = "/*", end = "*/" }
indent = { tab-width = 4, unit = "    " }

[[grammar]]
name = "sinter"
source = { git = "https://github.com/sinter-lang/sinter-lang", rev = "master", subpath = "editors/tree-sitter-sinter" }
```

Then run `hx --grammar fetch && hx --grammar build`, and copy
`queries/highlights.scm` to `~/.config/helix/runtime/queries/sinter/`.
