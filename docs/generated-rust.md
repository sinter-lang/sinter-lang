# The generated Rust simulator

`sinter build --emit rust` (also part of `--emit all`, the default) writes a
cycle-accurate simulator of the design as a standalone Rust crate, for
testbenches written in Rust. It computes exactly what the emitted Verilog
computes: the conformance oracle runs every example through it, the
interpreter behind `sinter sim`, and Verilator, and requires all three traces
to match bit for bit. It is about 60× faster than `sinter sim`'s interpreter
(31 million cycles per second for the pipelined RV32 core in
`examples/rv32/pipe.snt`, on one core).

From 1.0 this interface is stable; until then it may change, and any
change will be listed in the [CHANGELOG](https://github.com/sinter-lang/sinter-lang/blob/master/CHANGELOG.md). See
[Stability and versioning](https://github.com/sinter-lang/sinter-lang/blob/master/STABILITY.md).

## The crate

For `struct Counter` with a `#[clock]` method, the crate is
`<out>/counter-sim/` (the module name in snake case, plus `-sim`):

```
counter-sim/
  Cargo.toml       package `counter-sim`, no dependencies, publish = false
  src/lib.rs       the model
  src/runtime.rs   Sinter's value arithmetic, shared with the compiler
```

Add it to your testbench crate as a path dependency:

```toml
[dev-dependencies]
counter-sim = { path = "build/counter-sim" }
```

The crate is regenerated on every build, so don't edit it; commit it or
regenerate it in your build script, whichever suits your project.

## The API

```rust
use counter_sim::{Counter, Inputs};

let mut sim = Counter::new();          // registers and memories at reset
for cycle in 0..10 {
    let out = sim.step(&Inputs { en: 1 });
    assert_eq!(out.value_q, cycle);
}
```

- **`Inputs` / `Outputs`** have one public field per port, named as in the
  Verilog (a name that is a Rust keyword becomes a raw identifier, such as
  `r#loop`). Each field has the smallest Rust integer type that holds the
  port: `u1`–`u8` → `u8`, `u9`–`u16` → `u16`, up to `u64`, and signed ports
  get `i8`–`i64`, sign-extended. A fixed-point port is its raw bit pattern; its
  doc comment gives the number of fractional bits. Input values wider than
  their port are masked.
- **`step(&Inputs) -> Outputs`** is one clock cycle. The outputs are sampled
  before the clock edge, from the current state and this cycle's inputs, as a
  Verilog testbench sees them just before `posedge clk`. Then the registers
  and memories update.
- **`eval(&Inputs) -> Outputs`** returns the outputs `step` would return
  for these inputs, without committing anything (see below).
- **`reset()`** returns to the reset state. `new()` and `Default` start there.
- **`register(name)`** and **`memory(name)`** read state by its flattened name
  (a nested field `a.b` is `a_b`), as `Option<u64>` / `Option<&[u64]>`.
- **`assert_failures()`** lists the sim-only `assert!`s that read false, with
  the step they fired on, the message, and the static backtrace. An assert
  never stops the model.
- **`Inputs::NAMES`**, `from_raw` / `to_raw`, and **`run_raw(&[[u64; N]])`**
  work with raw bit patterns in port order, for replaying traces.

## Models of external chips

A testbench often surrounds the design with Rust models of the chips it
talks to: a memory, a sensor, a host. A model that answers in a later cycle
just reads the outputs `step` returns. A model that answers *in the same
cycle*, like an asynchronous memory, uses `eval` to see the design's outputs
first, then runs the cycle with its answer:

```sinter
// Sums the bytes of an external, asynchronous memory, one per cycle.
struct Reader {
    ptr: u8 = 0,
    pub sum: u16 = 0,
}

struct Bus {
    addr: u8,
}

impl Reader {
    #[clock(domain = sys)]
    fn tick(self, go: u1, data: u8) -> (Reader, Bus) {
        let next = if go == 1 {
            Reader { ptr: (self.ptr + 1) as u8 wrap, sum: (self.sum + data) as u16 wrap }
        } else {
            self
        };
        (next, Bus { addr: self.ptr })
    }
}
```

```rust
use reader_sim::{Inputs, Reader};

let memory: Vec<u8> = (0..=255).collect();
let mut sim = Reader::new();
for _ in 0..10 {
    // The address the design drives this cycle. It doesn't depend on
    // `data`, so any value will do here.
    let addr = sim.eval(&Inputs { go: 1, data: 0 }).out_addr;
    // The memory answers, and the cycle runs with its answer.
    sim.step(&Inputs { go: 1, data: memory[usize::from(addr)] });
}
assert_eq!(sim.register("sum"), Some((0..10).sum()));
```

The outputs the model reads mustn't depend on the input it answers with;
in hardware that would be a combinational loop.

## Hierarchical designs

With `#[module]` structs (the book's Hierarchy chapter), `--emit rust`
writes a crate for the top module, which simulates the whole design, and one
for each `#[module]` struct (`delay-sim` for `Delay`), which simulates that
block on its own, with its ports as `Inputs` and `Outputs`. Use those to
test a block without the rest of the design.

## Several clock domains

Each `step` advances to the next
clock edge, as `sinter sim` does: the domains with an edge at that time take
their inputs from `Inputs` and update, and the others hold, ignoring their
fields. `time()` gives the edge time in period units, and `ticked()` says which
domains (in declaration order) had an edge.
