# Assertions and faults

## `assert!`

```sinter
struct Sat {
    pub count: u8 = 0,
}

impl Sat {
    #[clock(domain = sys)]
    fn tick(self, inc: u1) -> Sat {
        assert!(self.count <= 200, "count passed the cap of 200");
        let next: u8 = if inc == 1 & self.count != 200 { (self.count + 1) as u8 wrap } else { self.count };
        Sat { count: next }
    }
}
```

`assert!(condition, "message")` states something that should be true every
cycle. The condition is a `u1`. An assertion is a statement, written before a
block's final expression like a `let`.

Assertions are checked by the simulators, and **cost nothing in hardware**:
by default the generated Verilog is byte-for-byte the same with or without
them, so you can assert freely.

When an assertion fails, `sinter sim` logs it with the cycle and a backtrace,
keeps simulating, and exits with status 1 at the end:

```text
[ERROR sinter] assert failed (cycle 17): bit counter moved outside the data phase [Top -> UartTx::step@66 (line 41)]
```

The backtrace is the chain of calls that reached the assertion: here, `Top`'s
clock root called `UartTx::step` on line 66, and the assertion is on line 41.
A method called from several places gets a backtrace for each call. The
[generated Rust simulator](../tools/generated-rust.md) collects failures too
(`assert_failures()`), without stopping.

`debug_assert!` has the same form, and currently behaves exactly like
`assert!`.

Assertions can't appear inside an `if` or `match` arm, or in a `pipeline` or
`multiplex` function. To assert something only in some cases, put the case in
the condition:

```sinter
~enum Mode { Idle, Run }
~struct T { pub count: u4 = 0 }
~impl T {
~    #[clock(domain = sys)]
~    fn tick(self, mode: Mode) -> T {
        // "count stays below 8 while running"
        assert!(mode != Mode::Run | self.count < 8, "count overran while running");
~        T { count: if mode == Mode::Run { (self.count + 1) as u4 wrap } else { 0 } }
~    }
~}
```

## Faults in hardware

To keep a record of failed assertions in the real hardware, build with
`--faults`:

```sh
sinter build design.snt --faults
```

This adds to the module:

- a **sticky** register with one bit per assertion, which is set the first
  time that assertion fails and stays set until reset, and
- an output port, `fault_code`, holding the number of the lowest-numbered
  failed assertion, or 0 if none has failed. It comes from the register, so
  it shows a failure the cycle after it happens.

The numbers are explained by a table written beside the Verilog,
`<Module>.faults.json`:

```text
[
  { "code": 1, "message": "a exceeded threshold 200", "backtrace": "Mon (line 22)" },
  { "code": 2, "message": "b high bit set", "backtrace": "Mon (line 23)" }
]
```

Read `fault_code` from a debugger, a status register, or an LED. The codes
are assigned per build, so always decode with the table from the same build.

`fault_code` only reports: nothing in the design can read it, so turning
faults on or off never changes what the design computes. Without `--faults`,
the module has no `fault_code` port.

The fault latch works only in single-clock designs.
