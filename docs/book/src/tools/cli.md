# The `sinter` command

```text
sinter check <FILE> [-D NAME=VALUE]... [--defines FILE]
sinter build <FILE> [-D NAME=VALUE]... [--defines FILE] [--emit verilog|harness|rust|all] [--out DIR] [--pins FILE [--constraints FILE]] [--faults]
sinter sim   <FILE> [-D NAME=VALUE]... [--defines FILE] [--cycles N] [--seed S] [--stimulus FILE] [--vcd FILE] [--trace FILE] [--cosim [--cosim-dir DIR]]
```

`<FILE>` is a `.snt` file, compiled as the root of its project: `mod`
declarations in it pull in the other files (see
[Modules](../reference/items.md#modules)).

## Build-time parameters

On every command, `-D NAME=VALUE` overrides an integer `pub const`
([Build-time parameters](../reference/items.md#build-time-parameters)), and
`--defines FILE` reads overrides from a file, one `NAME = VALUE` per line
with `#` comments:

```text
# wide.defs
N_MUL = 2
setup::DEPTH = 64
```

A value is decimal (negative for a signed constant), `0x` hex, or `0b`
binary. The file's overrides come first and each `-D` after them, so a `-D`
wins over the file.

## `sinter check`

Parses and type-checks the design and reports errors. On success it prints a
line per module with its port, register, and wire counts. A file with no
`#[clock]` root still type-checks; `check` notes that there is nothing to
build.

## `sinter build`

Writes the design's artifacts to the output directory (`--out`, default
`build/`):

| `--emit` | Writes |
|---|---|
| `verilog` | `<Module>.sv`, the SystemVerilog module ([details](generated-verilog.md)), and one `.sv` per [`#[module]`](../reference/hierarchy.md) struct |
| `rust` | `<module>-sim/`, a Rust crate that simulates the design ([details](generated-rust.md)), and one per `#[module]` struct |
| `harness` | `<Module>_tb.cpp`, a Verilator testbench used by Sinter's own conformance tests |
| `all` (default) | all three |

Also:

- the `src` file of every [`#[verilog]` binding](../reference/foreign-verilog.md)
  is copied into the output directory;
- `--pins FILE` writes a board-level wrapper and a nextpnr PCF file, and
  `--constraints FILE` writes the pin constraints there instead, as an iCE40
  PCF or an ECP5 LPF depending on its extension (`.pcf` or `.lpf`)
  ([Board pin maps](pin-maps.md));
- `--faults` adds the fault latch and writes `<Module>.faults.json`
  ([Assertions and faults](../reference/faults.md#faults-in-hardware)).

`build` warns when the design has no outputs, since synthesis would then
remove everything.

## `sinter sim`

Simulates the design, cycle by cycle, and prints a table with a row per cycle
(per clock edge, for [several clocks](../reference/clock-domains.md#in-simulation)):
the inputs left of `||`, the outputs right of it.

| Option | Meaning |
|---|---|
| `--cycles N` | simulate `N` cycles of random inputs (default 16) |
| `--seed S` | the random seed; the same seed gives the same inputs |
| `--stimulus FILE` | use the inputs, and check the expected outputs, in a CSV file ([Stimulus files](stimulus.md)); replaces `--cycles` and `--seed` |
| `--vcd FILE` | write a waveform of the inputs, outputs, registers, and named wires |
| `--trace FILE` | write the raw output trace used by Sinter's conformance tests |
| `--cosim` | also run the generated Verilog under Verilator over the same inputs, and fail on the first output that differs |
| `--cosim-dir DIR` | where `--cosim` builds (default `build/cosim`) |

The simulation starts from the reset state, at cycle 0. A failed
[assertion](../reference/faults.md) or expectation is logged and makes the
command fail after the run.

### Co-simulation

`--cosim` checks the compiler, not the design: the Verilog `sinter build`
would write must produce exactly the simulator's outputs, cycle for cycle.
It writes the SystemVerilog (with its `#[module]` children and any
`#[verilog]` sources) and a Verilator harness into `DIR/<Module>/`, builds
them, replays the same inputs, including a stimulus file's `rst_n` resets,
and compares every output of every cycle:

```text
sinter sim engine.snt --stimulus engine.csv --cosim
[INFO  sinter] co-simulation: the Verilog matches the simulator over 400 cycle(s)
```

A difference is reported with its cycle and output, and fails the command:

```text
[ERROR sinter] co-simulation mismatch (cycle 3): `sum` is 7 in the simulator but 4 in the Verilog
```

It needs `verilator` on the `PATH`. A mismatch in a design that binds
existing Verilog usually means its [reference model](../reference/foreign-verilog.md)
is wrong; otherwise, it is a compiler bug worth reporting.

In the VCD file, signals are grouped into `in`, `out`, `reg`, and `wire`
scopes, cycle *k* starts at a rising clock edge at 10*k* ns, and values are
raw bit patterns (set a signed display format for `iN` signals in your
viewer).

## Exit status and logging

| Status | Meaning |
|---|---|
| 0 | success |
| 1 | an error: a compile error, a bad stimulus file, a failed assertion or expectation, or an I/O error |
| 2 | invalid command-line arguments |

Diagnostics go to standard error through the logger; set `RUST_LOG=warn` to
hide the informational lines, or `RUST_LOG=debug` for more. `check` and
`sim` print their results to standard output.

A compile error names the file, line, and column, and points at the
offending expression:

```text
Error: compiling `counter.snt`

Caused by:
    counter.snt:10:31: type mismatch: expected `u8`, found `u9`
       |
    10 |             value: self.value + 1,
       |                               ^
       = in `tick`
```

(A binary operation is reported at its operator.)

Parsing stops at the first syntax error. Type checking reports the first error
in each function.
