# sinter-lang

The compiler for [Sinter](https://github.com/sinter-lang/sinter-lang), a
register-transfer-level hardware description language with Rust-like syntax.
One `.snt` source compiles to portable SystemVerilog and to a cycle-accurate
simulator, and the two are required to agree bit for bit, cycle for cycle.

This package installs the `sinter` command:

```sh
cargo install sinter-lang --locked
```

```sh
sinter check counter.snt                     # parse and type-check
sinter sim   counter.snt --cycles 16          # simulate with random inputs
sinter sim   counter.snt --stimulus test.csv  # or with directed inputs and checks
sinter build counter.snt --out build          # emit SystemVerilog and a Rust simulator
```

A Sinter design is a state struct and a method that computes the next state,
one call per clock cycle:

```rust
struct Counter {
    pub value: u8 = 0,
}

impl Counter {
    #[clock(domain = sys)]
    fn tick(self, en: u1) -> Counter {
        Counter {
            value: if en == 1 { (self.value + 1) as u8 wrap } else { self.value },
        }
    }
}
```

The [repository](https://github.com/sinter-lang/sinter-lang) has the user
guide and language reference (`docs/book`), examples including two RISC-V
cores, and prebuilt static binaries for Linux on each release.

Sinter is pre-1.0: the language and command line may change between minor
releases. See
[STABILITY.md](https://github.com/sinter-lang/sinter-lang/blob/master/STABILITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
