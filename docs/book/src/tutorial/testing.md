# Testing in simulation

`sinter sim` runs the design cycle by cycle. This chapter uses it three ways:
with directed inputs and expected outputs, with a waveform, and with
assertions that check the design's own invariants.

## Directed stimulus

Put this in `frame.csv`. It starts one frame sending `0x35`, then checks the
line level and the busy output on every cycle:

```text
# Send 0x35 (0b0011_0101) with two clock cycles per bit.
start, data, expect.line_q, expect.out
1,     0x35, 1,             0   # idle; start a frame
0,     0,    0,             1   # start bit
0,     0,    0,             1
0,     0,    1,             1   # d0 = 1
0,     0,    1,             1
0,     0,    0,             1   # d1 = 0
0,     0,    0,             1
0,     0,    1,             1   # d2 = 1
0,     0,    1,             1
0,     0,    0,             1   # d3 = 0
0,     0,    0,             1
0,     0,    1,             1   # d4 = 1
0,     0,    1,             1
0,     0,    1,             1   # d5 = 1
0,     0,    1,             1
0,     0,    0,             1   # d6 = 0
0,     0,    0,             1
0,     0,    0,             1   # d7 = 0
0,     0,    0,             1
0,     0,    1,             1   # stop bit
0,     0,    1,             1
0,     0,    1,             0   # idle again
```

```sh
sinter sim uart.snt --stimulus frame.csv
```

```text
...
   21 | 0 0 || 1 0
[INFO  sinter] all 44 expectation(s) over 22 cycle(s) passed
```

Each row is a cycle. Input columns drive inputs (an input with no column is
held at 0), and `expect.` columns are compared with the outputs. Values can be
decimal, `0x` hex, or `0b` binary, and `#` starts a comment.

Notice the timing. `line_q` is a register, so it shows the start bit in cycle
1, the cycle after `start` was high. `out` is combinational from the current
state, so it goes high in cycle 1 too, and low again in the cycle after the
stop bit ends.

A failed expectation is logged with its cycle and CSV line, and `sinter sim`
exits with status 1. Run CSV files like this one from your CI.

## Waveforms

```sh
sinter sim uart.snt --stimulus frame.csv --vcd frame.vcd
```

`frame.vcd` opens in GTKWave or Surfer. Signals are grouped into `in`, `out`,
`reg` (every register, `pub` or not, with its flattened name such as
`tx_baud_count`), and `wire` (named `let` bindings).

## Assertions

An expected-output column checks behaviour from the outside. An `assert!`
checks a property from the inside, every cycle, in every simulation. The bit
counter should only move during the data phase, so add this line at the top
of `UartTx::step`:

```sinter
~const BIT_CYCLES: u8 = 2;
~struct Baud {
~    count: u8 = 0,
~}
~impl Baud {
~    fn step(self, run: u1) -> (Baud, u1) {
~        let next = (self.count + 1) as u8 wrap;
~        if run == 0 {
~            (Baud { count: 0 }, 0)
~        } else if next == BIT_CYCLES {
~            (Baud { count: 0 }, 1)
~        } else {
~            (Baud { count: next }, 0)
~        }
~    }
~}
~enum Phase {
~    Idle,
~    Start,
~    Data,
~    Stop,
~}
~struct UartTx {
~    phase: Phase = Phase::Idle,
~    shift: u8 = 0,
~    bit: u3 = 0,
~    baud: Baud,
~}
~impl UartTx {
~    fn busy(self) -> u1 {
~        self.phase != Phase::Idle
~    }
~    fn line(self) -> u1 {
~        match self.phase {
~            Phase::Start => 0,
~            Phase::Data => self.shift[0:0],
~            _ => 1,
~        }
~    }
    fn step(self, start: u1, data: u8) -> UartTx {
        assert!(self.phase == Phase::Data | self.bit == 0, "bit counter moved outside the data phase");
        let (baud, done) = self.baud.step(self.busy());
~        let load = self.phase == Phase::Idle & start == 1;
~        let shift_out = self.phase == Phase::Data & done == 1;
~        let phase = match self.phase {
~            Phase::Idle => if start == 1 { Phase::Start } else { Phase::Idle },
~            Phase::Start => if done == 1 { Phase::Data } else { Phase::Start },
~            Phase::Data => if shift_out & self.bit == 7 { Phase::Stop } else { Phase::Data },
~            Phase::Stop => if done == 1 { Phase::Idle } else { Phase::Stop },
~        };
~        UartTx {
~            phase: phase,
~            shift: if load { data } else if shift_out { 0_u1 @ self.shift[7:1] } else { self.shift },
~            bit: if shift_out { (self.bit + 1) as u3 wrap } else if load { 0 } else { self.bit },
~            baud: baud,
~        }
~    }
~}
~struct Top {
~    tx: UartTx,
~    pub line: u1 = 1,
~}
~impl Top {
~    #[clock(domain = sys)]
~    fn tick(self, start: u1, data: u8) -> (Top, u1) {
~        let tx = self.tx.step(start, data);
~        (Top { tx: tx, line: tx.line() }, self.tx.busy())
~    }
~}
```

The design still passes, and the assertion holds. Now break the design: in
the `Phase::Data` arm, change `self.bit == 7` to `self.bit == 6`, so the frame
ends one bit early and leaves `bit` at 7. Run `frame.csv` again:

```text
[ERROR sinter] assert failed (cycle 17): bit counter moved outside the data phase [Top -> UartTx::step@66 (line 41)]
...
[ERROR sinter] expectation failed: cycle 17 (line 20): `line_q` expected 0, got 1
...
Error: 5 sim-only assertion failure(s) and 4 expectation mismatch(es); see the log above
```

The assertion names the cycle and a backtrace: the `assert!` on line 41,
reached from `Top` through the call to `UartTx::step` on line 66 (your line
numbers will differ). A method called from several places gets a separate
backtrace for each call.

An `assert!` exists only in simulation: the Verilog is identical with or
without it, so you can assert freely. To keep a record of failures in
hardware too, see [Assertions and faults](../reference/faults.md).

## Testing from Rust

For long or complex tests, `sinter build` also writes a Rust crate that
simulates the design about 60 times faster than `sinter sim`, with an
`Inputs` / `Outputs` struct and a `step` method. See
[The generated Rust simulator](../tools/generated-rust.md).
