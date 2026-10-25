# sinter-compiler

The type checker, elaborator, and backends (SystemVerilog, Rust simulator,
Verilator harness) of [Sinter](https://github.com/sinter-lang/sinter-lang), a
register-transfer-level hardware description language.

This crate is an internal part of the `sinter` command, which is in the
[`sinter-lang`](https://crates.io/crates/sinter-lang) package. Its Rust API
may change in any release, including minor and patch releases; if you use it
directly, depend on an exact version. See
[STABILITY.md](https://github.com/sinter-lang/sinter-lang/blob/master/STABILITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
