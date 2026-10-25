# Introduction

Sinter is a register-transfer-level (RTL) hardware description language with
Rust-like syntax. You describe the registers in your design and the logic
that computes their next values, and the compiler turns that one source into
two things:

- **portable SystemVerilog** that synthesizes with Yosys and vendor tools and
  lints clean under Verilator, and
- **a cycle-accurate simulator**, both as an interpreter (`sinter sim`) and as
  a generated Rust crate you can drive from Rust tests.

The two are required to agree bit for bit, cycle for cycle. Sinter's own test
suite checks that on every example by simulating the generated Verilog in
Verilator and comparing it with the simulator, so what you test in simulation
is what you get in hardware.

## The idea in one example

```sinter
// An 8-bit counter with an enable.
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

The `struct` is the design's state: one 8-bit register, reset to 0. The
method marked `#[clock]` is one clock cycle: it receives the current state
and this cycle's inputs, and returns the next state. There is no `always`
block, no blocking-vs-non-blocking distinction, and no way to drive a
register from two places.

`self.value + 1` is a `u9`, because adding two 8-bit numbers can carry. To
store it back in an 8-bit register you have to say what happens to the carry:
`as u8 wrap` drops it. Sinter never narrows a value silently.

## What Sinter checks for you

- **Widths.** Arithmetic widens to fit its result, and every narrowing names
  its mode (`wrap`, `sat`, `trunc`, or `round`).
- **One driver.** Each register gets exactly one next value per cycle, because
  the next state is a single returned value.
- **Clock-domain crossings.** Reading another clock domain's register is a
  compile error. Crossings go through synchronizer types.
- **Lossless queues stay lossless.** A `fifo` can only be pushed through a
  handle that refuses the push when the queue is full.

## Where to go next

- [Installation](getting-started/installation.md) and
  [Your first design](getting-started/first-design.md) get the compiler
  running.
- [The tutorial](tutorial/overview.md) builds a UART transmitter step by
  step, tests it, and turns it into Verilog.
- [The language reference](reference/lexical.md) describes every construct.
- [The tools section](tools/cli.md) covers the command line, stimulus files,
  and the generated Verilog and Rust interfaces.

Sinter is pre-1.0: the language and command line can still change between
minor releases. [Stability and versioning](appendix/stability.md) says what
1.0 will promise, and [Limitations](appendix/limitations.md) lists what isn't
implemented yet.
