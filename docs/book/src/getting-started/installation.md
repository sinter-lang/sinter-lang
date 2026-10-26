# Installation

The compiler is a single binary, `sinter`. It needs nothing else to check,
simulate, and build designs. You only need Yosys or Verilator to synthesize or
simulate the Verilog it emits, as with any other Verilog.

## Prebuilt binary

Each [release](https://github.com/sinter-lang/sinter-lang/releases) has fully
static Linux binaries for `x86_64` and `aarch64`, which run on any
distribution:

```sh
SINTER_VERSION=v0.9.0
mkdir -p ~/.local/bin
curl -fsSL "https://github.com/sinter-lang/sinter-lang/releases/download/${SINTER_VERSION}/sinter-${SINTER_VERSION}-$(uname -m)-unknown-linux-musl.tar.gz" \
    | tar -xz -C ~/.local/bin sinter
```

Each tarball has a `.sha256` file beside it.

## From source

With a Rust toolchain (1.85 or later), build and install from the
repository:

```sh
cargo install --git https://github.com/sinter-lang/sinter-lang.git sinter-lang --locked
```

Add `--tag v0.9.0` to pin a release, or `--rev <commit>` for a commit. The
package is called `sinter-lang`; the binary it installs is `sinter`.

## Check it works

```sh
sinter --version
sinter --help
```

## Editor support

Syntax highlighting is available for VS Code and for editors that use
tree-sitter (Neovim, Helix, Zed). See [Editor support](../tools/editors.md).
