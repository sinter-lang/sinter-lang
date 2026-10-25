# Editor support

Sinter has syntax highlighting for VS Code and for editors that use
tree-sitter. There is no language server yet, so diagnostics come from
running `sinter check`.

## VS Code

Download `sinter-<version>.vsix` from a
[release](https://github.com/sinter-lang/sinter-lang/releases), then:

```sh
code --install-extension sinter-<version>.vsix
```

`.snt` files get highlighting, comment toggling, and bracket matching. See
[`editors/vscode`](https://github.com/sinter-lang/sinter-lang/tree/master/editors/vscode)
to build the extension yourself.

## Neovim, Helix, Zed

[`editors/tree-sitter-sinter`](https://github.com/sinter-lang/sinter-lang/tree/master/editors/tree-sitter-sinter)
is a tree-sitter grammar with highlight queries. Its README has setup
instructions for each editor.
