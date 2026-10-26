# Stability and versioning

Sinter uses [semantic versioning](https://semver.org). This page says what
that means for a hardware language: what a release may change, and what
it may not.

## One version number

The compiler crates (`sinter-syntax`, `sinter-compiler`, `sinter-lang`) and
the editor packages share one version number and are released together.

## Before 1.0

Sinter is pre-1.0. Each `0.x` minor release (0.9 to 0.10, say), and each
1.0 release candidate (`1.0.0-rc.N`), may break anything on this page; the [CHANGELOG](https://github.com/sinter-lang/sinter-lang/blob/master/CHANGELOG.md) lists every breaking
change with how to update. Patch releases (0.9.0 to 0.9.1) only fix bugs.

## What 1.0 promises

From 1.0 on, every 1.x release keeps these stable, so that **a design that
compiles with 1.x keeps compiling, to hardware that behaves the same**:

1. **The language.** The syntax and meaning of everything documented in the
   language reference ([`docs/book`](https://github.com/sinter-lang/sinter-lang/blob/master/docs/book/src/reference/lexical.md)). A
   valid program stays valid, and computes the same values, cycle for cycle.
2. **The generated module's interface.** Its name, its ports (names, order,
   directions, and widths), and its clock and reset behaviour, as documented
   in [The generated Verilog](https://github.com/sinter-lang/sinter-lang/blob/master/docs/generated-verilog.md), and of each
   `#[module]` sub-module. Verilog that instantiates a Sinter module keeps
   working.
3. **The `sinter` command.** Its subcommands and flags and what they do, its
   exit statuses, and the files it reads: `.snt` projects, stimulus CSV
   files, pin maps, and `include_hex!` files.
4. **The generated Rust simulator's API**, as documented in
   [The generated Rust simulator](https://github.com/sinter-lang/sinter-lang/blob/master/docs/generated-rust.md), so Rust
   testbenches keep compiling.

"Behaves the same" means the same values at the module's ports on every
cycle, in the simulators and in the generated Verilog. It does not mean the
same Verilog text.

## What 1.x may change

- **Additions.** New syntax (in places that are currently errors), types,
  methods, attributes, subcommands, flags, and output files. A name followed
  by `<` is reserved for built-in types, so new ones can't clash with yours.
  New reserved words come only in a major release, except `gen`, which is
  already reserved.
- **The generated Verilog's internals.** Internal signal names, formatting,
  and how the logic is written, for example to make it smaller or faster.
  Only the interface is stable.
- **Diagnostics.** Error and warning text, positions, and how many errors are
  reported. New warnings may appear.
- **Simulator output details.** The layout of `sinter sim`'s table, the VCD
  file's scopes and wire names, the `--trace` format, and the Verilator
  harness (`--emit harness`), which exist for Sinter's own testing.
- **Fault codes.** `--faults` numbers assertions per build; decode
  `fault_code` with the `.faults.json` from the same build.
- **Tool and toolchain versions.** The Yosys and Verilator versions that CI
  tests against, and the minimum Rust version for building from source, may
  be raised in a minor release. Each change is in the CHANGELOG.
- **Bug fixes.** If the simulator and the generated Verilog disagree, or
  either disagrees with the documentation, that is a bug, and fixing it may
  change behaviour. If the compiler accepts a program the documentation says
  is invalid, the fix may reject it.

Behaviour the documentation doesn't describe isn't covered.

## The library crates

`sinter-syntax` and `sinter-compiler` are published so that `sinter-lang`
can be installed with `cargo install`. Their Rust APIs are internal to the
`sinter` tool and may change in any release, including minor and patch
releases. If you use them directly, depend on an exact version
(`sinter-compiler = "=1.2.3"`).

## Deprecation

Removing anything in the 1.0 promise needs a new major version. Before that,
a 1.x release deprecates it: the compiler warns where it is used, and the
CHANGELOG says what to use instead.
