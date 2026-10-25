# Your first design

This chapter writes a counter, simulates it, tests it, and turns it into
Verilog. Make an empty directory and put this in `counter.snt`:

```sinter
// An 8-bit counter that counts up while `en` is high.
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

Reading it from the top:

- `struct Counter` is the design's **state**. Each field is a register, and
  `= 0` is its reset value. `pub` makes the register visible outside the
  design, as an output port.
- `#[clock(domain = sys)]` marks `tick` as the **clock root**: one call of
  `tick` is one clock cycle. `sys` names the clock.
- `tick` takes the current state as `self` and this cycle's inputs as
  parameters (here one bit, `en`), and returns the state for the next cycle.
- `self.value + 1` is 9 bits wide, so `as u8 wrap` narrows it back to 8 bits,
  dropping the carry. Leave the cast out and the compiler rejects the program.

## Check it

```sh
sinter check counter.snt
```

```text
ok: module `Counter` (2 ports, 1 regs, 0 wires)
```

The two ports are the input `en` and the output `value_q`.

## Simulate it

`sinter sim` runs the design for a number of cycles with random inputs and
prints one row per cycle:

```sh
sinter sim counter.snt --cycles 6
```

```text
cycle | en || value_q
    0 | 1 || 0
    1 | 0 || 1
    2 | 1 || 1
    3 | 0 || 2
    4 | 1 || 2
    5 | 1 || 3
```

Inputs are left of `||`, outputs right of it. `value_q` is the register's
value *during* that cycle, so it changes the cycle after `en` is high.

## Test it

Random inputs are good for exploring; for a test you want chosen inputs and
checked outputs. Put this in `counter.csv`:

```text
en, expect.value_q
1,  0
1,  1
0,  2
1,  2
1,  3
```

Each row is one cycle. A column named after an input drives it, and an
`expect.` column checks an output:

```sh
sinter sim counter.snt --stimulus counter.csv
```

```text
cycle | en || value_q
    0 | 1 || 0
    1 | 1 || 1
    2 | 0 || 2
    3 | 1 || 2
    4 | 1 || 3
[INFO  sinter] all 5 expectation(s) over 5 cycle(s) passed
```

If an expectation fails, `sinter sim` logs the cycle and line and exits with a
non-zero status, so the file works as a test in CI. See
[Stimulus files](../tools/stimulus.md) for the format, and add
`--vcd counter.vcd` to write a waveform.

## Build it

```sh
sinter build counter.snt
```

```text
[INFO  sinter] wrote build/Counter.sv
[INFO  sinter] wrote build/Counter_tb.cpp
[INFO  sinter] wrote build/counter-sim
```

`build/Counter.sv` is the SystemVerilog module:

```systemverilog
module Counter (
    input  wire         clk,    // sys clock
    input  wire         rst_n,  // active-low asynchronous reset
    input  wire  [0:0] en,
    output logic [7:0] value_q
);
```

Every module gets a clock and an active-low asynchronous reset, then the
inputs, then the `pub` registers as `_q` outputs.
[The generated Verilog](../tools/generated-verilog.md) describes the
interface in full. `Counter_tb.cpp` is a Verilator harness, and `counter-sim/`
is a Rust crate that simulates the design (see
[The generated Rust simulator](../tools/generated-rust.md)). Use
`--emit verilog` to write only the `.sv` file.

## Next

The [tutorial](../tutorial/overview.md) builds something bigger: a UART
transmitter with a state machine, sub-modules, a FIFO, and tests.
