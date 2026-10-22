# The generated Verilog interface

`sinter build` emits one SystemVerilog module per design. This page describes
that module's interface, so that hand-written Verilog can instantiate it. The
rules are pinned by
[`crates/sinter-compiler/tests/interface.rs`](../crates/sinter-compiler/tests/interface.rs).
Before 1.0 they may still change, and any change will be listed in the
[CHANGELOG](../CHANGELOG.md).

## Module and file

- The module is named after the **state struct** that owns the `#[clock]`
  method (`struct Counter` → `module Counter`). It is written to
  `<Module>.sv` in the `--out` directory (default `build/`).
- The file starts with `` `default_nettype none `` and ends by restoring
  `` `default_nettype wire ``.
- The module is self-contained: no submodules, includes, or packages.

## Clock and reset

Every module has two ports first, in this order:

| Port    | Meaning |
|---------|---------|
| `clk`   | The clock of the `#[clock(domain = …)]` domain. All registers update on its rising edge. |
| `rst_n` | Active-low **asynchronous** reset. While low, every register holds its declared reset value (`field: T = value`). An `Option` field with no `= value` resets to `None`, and a nested struct field resets each of its own fields. |

Drive `rst_n` from a reset synchronizer (asynchronous assert, synchronous
deassert) as you would for any other logic.

Memory contents (`mem` / `rom`) are **not** cleared by `rst_n`. A `mem`
starts at zero and a `rom` at its contents, set by an `initial` block. FPGA
flows honour this as power-up contents; ASIC flows don't. A memory's
read-data register *is* reset (to zero).

## Ports

After `clk` and `rst_n`, ports appear in this order:

1. **Inputs**: one per scalar leaf of each `tick` parameter, in parameter
   order.
2. **Register outputs** (`<register>_q`): the current value of every state
   register, in field order. A memory contributes its read-data register
   (`<mem>_rdata_q`).
3. **Extra outputs** (`out`, `out_*`): the leaves of the second element when
   `tick` returns `(Self, T)`. They are absent when `tick` returns `Self`.
4. **`fault_code`**: only with `sinter build --faults`. It is the
   lowest code among the faults latched since reset (0 means no fault). Decode it with the emitted `<Module>.faults.json`.

### Names: flattening

Aggregate types flatten to scalar ports. Path segments are joined with `_`:

| Sinter type | Ports | Example |
|---|---|---|
| `uN` / `iN` | one port, `[N-1:0]` (`signed` for `iN`) | `x: i4` → `input wire signed [3:0] x` |
| fixed-point `uI.F` / `iI.F` | one port of `I+F` raw bits | `g: i2.2` → `signed [3:0] g` (Q2.2) |
| `enum` | one port holding the encoding | `m: Mode` → `[1:0] m` |
| `struct` | one port per field | `p: Pair` → `p_lo`, `p_hi` |
| array `[T; N]` | one port per element | `hist: [u4; 2]` → `hist_0_q`, `hist_1_q` |
| tuple | one port per element | `flags: (u1, u1)` → `flags_0`, `flags_1` |
| `Option<T>` | a valid bit, then the payload | `maybe: Option<u4>` → `maybe_is_some`, `maybe_payload` |

These compose. For example, a register `cmds: [Option<u4>; 2]` becomes
`cmds_0_is_some_q`, `cmds_0_payload_q`, `cmds_1_is_some_q`, and so on.
Extra outputs use the same rules with the prefix `out`: a scalar is `out`, a
tuple is `out_0`, `out_1`, …, and a struct is `out_<field>`.

**Encodings:**

- Enums use binary encoding in declaration order (the first variant is 0)
  unless `#[encoding(onehot | gray | johnson)]` says otherwise. The encoding
  sets the port width (3 variants → 2 bits binary, 3 bits one-hot).
- Fixed-point ports carry the raw bits (two's complement if signed): an
  `iI.F` value `v` appears as the integer `v × 2^F`. Each fixed-point port
  and register is commented with its format, e.g.
  `// Q2.2 signed fixed-point: value = raw / 2^2`. As in Sinter's own
  spelling, the integer-bit count includes the sign bit.
- An `Option` payload is meaningful only while its `_is_some` bit is 1.

**Collisions are errors.** All ports and internal signals share one
namespace. A parameter named `clk` or `rst_n`, or any two leaves that flatten
to the same name (an input `v_q` next to a register `v`, say), is rejected at
compile time.

### Timing

- `<register>_q` outputs come straight from flip-flops. They change only
  after a rising edge of `clk`.
- `out` / `out_*` are **combinational** functions of the current state *and
  the current inputs*. If `tick` computes an extra output from an input, the
  module has a combinational input-to-output path. Register it in the parent
  if that path is timing-critical.
- `fault_code` is decoded from registered state, so it has no combinational
  path from the inputs.

## Instantiating the module

`examples/fn_counter.snt` defines `struct Counter { value: u8 = 0 }` with
`fn tick(self, en: u1) -> Counter`, which emits:

```systemverilog
module Counter (
    input  wire         clk,    // sys clock
    input  wire         rst_n,  // active-low asynchronous reset
    input  wire  [0:0] en,
    output logic [7:0] value_q
);
```

Instantiate it with named port connections:

```systemverilog
Counter u_counter (
    .clk     (clk),
    .rst_n   (rst_n),
    .en      (count_en),
    .value_q (count)
);
```

Add the emitted `.sv` to your Yosys / Verilator / vendor project like any
other source. For a board top level that maps ports to physical pins, see the
`.pins` flow (`sinter build --pins`, DESIGN §7.6), which emits a wrapper
module and a nextpnr PCF.

## The legacy `module` surface

Designs written with the frozen `module { in …; out …; reg …; }` surface keep
their declared port names exactly, after `clk` and `rst_n`. None of the
flattening rules above apply to them.
