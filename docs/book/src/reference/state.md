# State and the clock

A Sinter design is a **state struct** and a **clock root**: a method that
computes the next state from the current one. One call of the clock root is
one clock cycle.

```sinter
struct Debounce {
    count: u16 = 0,
    level: u1 = 0,
}

impl Debounce {
    // The level once it has been stable for 50 000 cycles.
    fn step(self, raw: u1) -> Debounce {
        if raw == self.level {
            Debounce { count: 0, level: self.level }
        } else if self.count == 49_999 {
            Debounce { count: 0, level: raw }
        } else {
            Debounce { count: (self.count + 1) as u16 wrap, level: self.level }
        }
    }
}

struct Top {
    button: Debounce,
    pub presses: u8 = 0,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, raw: u1) -> (Top, u1) {
        let button = self.button.step(raw);
        let pressed = button.level & !self.button.level;
        let presses = if pressed { (self.presses + 1) as u8 wrap } else { self.presses };
        (Top { button: button, presses: presses }, pressed)
    }
}
```

## The clock root

The clock root is the one method marked `#[clock]`:

- Its `self` is the state during the current cycle.
- Its other parameters are the design's **inputs**, sampled this cycle.
- It returns the state for the next cycle, either alone (`-> Top`) or with
  extra outputs (`-> (Top, Out)`).

Its struct must be in the root module, and gives the generated Verilog module
its name. `domain = sys` names the clock; with one clock the name is your
choice. A design with several clocks has one clock root per domain; see
[Clock domains](clock-domains.md).

A file without a `#[clock]` method still type-checks, so a module of helper
functions can be checked on its own, but there is nothing to build.

## Registers and reset

Every scalar in the state struct, recursively, is a register:
`self.button.count` above is a 16-bit register. Each register's reset value
comes from its field's `= value`. A field whose type is a struct uses that
struct's reset values, and an `Option` field without one resets to `None`.
Any other field without a reset value is an error.

In the generated Verilog, all registers share one active-low asynchronous
reset, `rst_n`. In simulation, cycle 0 starts from the reset state.

## The next state

The value the clock root returns is the next state, so **each register gets
exactly one next value per cycle**. There is no way to assign a register in
two places, or to forget one: a struct value must name every field.

To hold a register, give it its current value (`level: self.level`). To hold
everything, return `self`.

A state struct can contain other state structs, as `Top` contains a
`Debounce`. A method on the inner struct computes *its* next state, and that
value takes effect only if it ends up in the returned state:
`self.button.step(raw)` computes a new `Debounce`, and putting it in the
returned `Top` is what commits it. Computing it and not using it costs
nothing and changes nothing. You can read the next state before it takes
effect, as `tick` does with `button.level`, and compare it with the current
one.

A method can return a next state and a value together, `(Debounce, u1)`,
when the parent needs a result computed in the same cycle.

Some built-in state types (memories, queues, clock crossings, and foreign
Verilog modules) must be stepped exactly once per cycle, outside any `if` or
`match` arm. Their pages say so; to make such a step conditional, pass it a
condition (a write enable, a pop flag) instead.

## Outputs

A design has two kinds of output:

- Each register under a **`pub`** field is an output port named after it with
  `_q` appended (`presses_q`). It shows the register's current value, so it
  changes only after a clock edge.
- If the clock root returns `(State, Out)`, each value in `Out` is an output
  port (`out`, or `out_0`, `out_1`, ... for a tuple, `out_<field>` for a
  struct). These are **combinational**: they can depend on this cycle's
  inputs.

Private fields are registers without ports. A `pub` field can't hold a
channel, a clock-domain crossing, or a staged function, since their
registers are internal to the compiler. A design with no outputs at all
compiles, but synthesis would remove all of its logic, so `sinter build`
warns about it.

[The generated Verilog](../tools/generated-verilog.md) gives the full naming
rules.

## One cycle is combinational

Everything the clock root computes, including every function and method it
calls, is combinational logic evaluated within the cycle. Values flow from
the current state and the inputs to the next state and the outputs, and the
only way back is through a register. So there are no combinational loops and
no latches.

Function calls are inlined, so the elaborated design is one flat module no
matter how it is organized in the source.
