# Hierarchy

By default a whole design becomes one Verilog module: every nested state
struct and every call is flattened into it. That is fine for small designs,
but in a large one you want the structure kept: in synthesis and timing
reports, in waveforms, and when reading the Verilog. Marking a state struct
`#[module]` makes it a Verilog module of its own, which its parent
instantiates.

```sinter
// A free-running counter that wraps at `limit`.
#[module]
struct Ticker {
    count: u16 = 0,
}

struct TickerOut {
    tick: u1,
}

impl Ticker {
    fn step(self, limit: u16) -> (Self, TickerOut) {
        let done = self.count == limit;
        (
            Self { count: if done { 0 } else { (self.count + 1) as u16 wrap } },
            TickerOut { tick: done },
        )
    }
}

struct Top {
    slow: Ticker,
    fast: Ticker,
    pub ticks: u8 = 0,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self) -> Top {
        let (slow, s) = self.slow.step(999);
        let (fast, f) = self.fast.step(9);
        let ticks = if s.tick & f.tick { (self.ticks + 1) as u8 wrap } else { self.ticks };
        Top { slow: slow, fast: fast, ticks: ticks }
    }
}
```

`sinter build` writes `Top.sv` and `Ticker.sv`. `Top` instantiates `Ticker`
twice, as `u_slow` and `u_fast`:

```systemverilog
    Ticker u_slow (
        .clk      (clk),
        .rst_n    (rst_n),
        .limit    (u_slow_limit),
        .out_tick (u_slow_tick)
    );
```

The design behaves exactly as it would without `#[module]`: the simulators run
it flattened, as always, and Sinter's oracle checks the flattened simulation
against the hierarchical Verilog.

## The step method

A `#[module]` struct has exactly one **step** method, returning
`(Self, Out)` with `Out` a struct. It is the module's one cycle:

- its parameters are the module's **inputs**,
- `Out`'s fields are its **outputs**, and
- `Self` is its next state.

It can have other methods, which `step` may call, and it can hold anything a
state struct can, including memories, queues, and other `#[module]` structs.

## Ports

A `#[module]` follows the same port rules as a top-level module
([The generated Verilog](../tools/generated-verilog.md)): `clk` and `rst_n`,
then an input per parameter, flattened (`s: Sample` gives `s_v` and `s_en`),
then an output per field of `Out`, named `out_<field>` and flattened the same
way. Its registers are never ports, whether `pub` or not.

The module is named after its struct. A struct in another source module
replaces `::` with `__` (`display::Seg` becomes `display__Seg`).

## Rules

A `#[module]` is a real module, clocked every cycle, whose state its parent
can't see. So:

- Its parent steps it **exactly once per cycle**, through its own state field
  (`self.slow.step(..)`), outside any `if` or `match` arm, and puts the result
  back in that field. To make it hold, give it inputs that make it hold.
- The parent can't read its state (`self.slow.count`), or call its other
  methods: everything it needs comes through `Out`.

The compiler reports each of these as an error.

```sinter,fail
~#[module]
~struct Ticker {
~    count: u16 = 0,
~}
~struct TickerOut {
~    tick: u1,
~}
~impl Ticker {
~    fn step(self, limit: u16) -> (Self, TickerOut) {
~        let done = self.count == limit;
~        (Self { count: if done { 0 } else { (self.count + 1) as u16 wrap } }, TickerOut { tick: done })
~    }
~}
~struct Top {
~    slow: Ticker,
~    pub count: u16 = 0,
~}
~impl Top {
~    #[clock(domain = sys)]
~    fn tick(self) -> Top {
        let (slow, s) = self.slow.step(999);
        Top { slow: slow, count: slow.count }
~    }
~}
```

```text
error: `slow.count` is internal to the `#[module]` `Ticker`, whose state its parent can't read; add what you need to its outputs
```

A `#[module]` runs on the clock of the domain whose clock root steps it; its
own fields have no `#[domain]` tags. `sinter build --faults` doesn't support
designs with `#[module]` instances yet.

## What to make a module

Make a module of each block you would draw as a box on a block diagram:
a UART, a cache, a pipeline stage. Leave small helpers (a debouncer, a
counter used once) flattened into their parent. A module boundary costs no
logic, but tools can't optimize across it as freely, and every output has to
be listed in `Out`.
