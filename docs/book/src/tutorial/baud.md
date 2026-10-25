# A baud-rate counter

The transmitter holds each bit for `BIT_CYCLES` clock cycles, so the first
thing it needs is a counter that produces a pulse every `BIT_CYCLES` cycles.
Create `uart.snt`:

```sinter
// Clock cycles per bit. Small, so that simulations are short; real hardware
// uses the clock frequency divided by the baud rate.
const BIT_CYCLES: u8 = 4;

struct Baud {
    count: u8 = 0,
    pub tick: u1 = 0,
}

impl Baud {
    #[clock(domain = sys)]
    fn step(self, run: u1) -> Baud {
        let next = (self.count + 1) as u8 wrap;
        if run == 0 {
            Baud { count: 0, tick: 0 }
        } else if next == BIT_CYCLES {
            Baud { count: 0, tick: 1 }
        } else {
            Baud { count: next, tick: 0 }
        }
    }
}
```

## State and reset

`Baud` has two registers: `count`, 8 bits wide, and `tick`, 1 bit wide. Both
reset to 0; every register must say what it resets to. `tick` is `pub`, so it
becomes an output port. `count` is private, so it is a register inside the
module but not a port.

## The clock root

`step` is marked `#[clock(domain = sys)]`, which makes it the design's clock
root: each call is one clock cycle, `self` is the state during that cycle, and
the returned `Baud` is the state after the next clock edge. Its parameter
`run` is an input port.

A design has exactly one clock root (or one per clock domain, see
[Clock domains](../reference/clock-domains.md)), and its struct gives the
generated Verilog module its name.

## Widths are checked

`self.count + 1` adds two 8-bit numbers, so its type is `u9`: Sinter widens
arithmetic so the result always fits. Storing it in the 8-bit `count` means
throwing away a bit, and Sinter makes you say how:

- `as u8 wrap` keeps the low 8 bits (here, 255 + 1 becomes 0),
- `as u8 sat` would clamp to 255.

Try deleting `as u8 wrap`. The comparison `next == BIT_CYCLES` then compares a
`u9` with a `u8`, which is an error:

```sinter,fail
~const BIT_CYCLES: u8 = 4;
~
~struct Baud {
~    count: u8 = 0,
~    pub tick: u1 = 0,
~}
~
~impl Baud {
~    #[clock(domain = sys)]
~    fn step(self, run: u1) -> Baud {
        let next = self.count + 1;
~        if run == 0 {
~            Baud { count: 0, tick: 0 }
~        } else if next == BIT_CYCLES {
~            Baud { count: 0, tick: 1 }
~        } else {
~            Baud { count: next, tick: 0 }
~        }
~    }
~}
```

```text
error: type mismatch: expected `u9`, found `u8`
```

Sinter has no implicit conversions between types, even widening ones: both
sides of `==` must have the same type. The literal `1` has no type of its
own; it takes the type its context needs (here `u8`, like `self.count`).

## `if` is an expression

There are no statements that assign registers. The `if` chain is an
expression whose value is the next `Baud`, and every branch must produce one.
An `if` condition is a `u1`, and `else` is required.

## Try it

```sh
sinter sim uart.snt --cycles 12
```

With `run` high, `tick_q` goes high for one cycle in every four. `tick` is a
register, so the pulse appears the cycle *after* `count` reaches 3.

In the next chapter, the counter becomes part of the transmitter, and instead
of a registered `tick` it reports "this bit period is done" directly.
