# Board pin maps

When a Sinter design is a whole FPGA design, a **pin map** connects it
straight to the board's pins, with no hand-written top level:

```sh
sinter build top.snt --pins board.pins --out rtl
```

This writes, besides the design's own module:

- `rtl/board.sv`: a top-level module named after the pin-map file, with one
  1-bit port per pin, a power-on reset, and the design instantiated inside;
- `rtl/board.pcf`: the matching pin constraints for nextpnr.

Synthesize with `board` as the top module. Pin maps work for single-clock
designs.

For an ECP5, ask for an LPF with `--constraints`. The extension picks the
format, `.pcf` or `.lpf`, and the file is written where it names:

```sh
sinter build top.snt --pins board.pins --constraints rtl/board.lpf --out rtl
```

## Format

Each line binds one pin. `#` starts a comment.

```text
set_io <pin> <direction> <path> [low] [io_type=<std>] [freq=<n>MHz]
```

- `<pin>` is the package pin name (an iCE40 pin number, an ECP5 site such as
  `G2`), copied into the constraints as is.
- `<direction>` is `clock`, `in`, or `out`.
- `<path>` names what the pin connects to:
  - for `clock`, the clock domain (`sys`);
  - for `in`, an input of the clock root: a parameter (`start`), a field of a
    struct parameter (`btn.inc`), or a bit of a wider one (`data[3]`);
  - for `out`, a `pub` register: a field (`led`), a bit of it (`led[2]`), or
    a bit of an array element (`seg[0][6]`).
- `low` marks an active-low pin. The wrapper inverts it, so the design is
  written active-high.
- `io_type=` gives the pin's I/O standard, such as `LVCMOS33`.
- `freq=` gives a `clock` pin's frequency in MHz, such as `25MHz` or
  `12.5MHz`. A `#[clock]`'s `period` is in abstract units, so the board's
  real frequency is set here.

`io_type=` and `freq=` go into an LPF only: an LPF gets an `IOBUF` line for
each pin with an `io_type=` and a `FREQUENCY` line for the clock, which
nextpnr-ecp5 checks timing against. A PCF has no place for them, so `build`
warns and leaves them out.

A path that ends in `[i]` selects bit `i`; an earlier `[i]` indexes an array.
A path must name a single bit, and every input of the design must be bound to
a pin.

```text
# Nandland Go Board: two 7-segment digits and two buttons.
set_io 15  clock  sys           # 25 MHz oscillator
set_io 53  in     btn.inc       # SW1
set_io 51  in     btn.dec       # SW2
set_io 3   out    seg[0][0] low # ones digit, segment A
set_io 4   out    seg[0][1] low # ones digit, segment B
```

```text
# The same design on an ECP5 board (examples/go_board_ecp5.pins).
set_io G2  clock  sys       io_type=LVCMOS33 freq=25MHz
set_io R1  in     btn.inc   io_type=LVCMOS33
set_io B11 out    seg[0][0] low io_type=LVCMOS33
```

which gives:

```text
LOCATE COMP "clk" SITE "G2";  # sys clock
IOBUF PORT "clk" IO_TYPE=LVCMOS33;
FREQUENCY PORT "clk" 25 MHZ;
LOCATE COMP "btn_inc" SITE "R1";  # btn.inc
IOBUF PORT "btn_inc" IO_TYPE=LVCMOS33;
LOCATE COMP "seg_0_0" SITE "B11";  # seg[0][0], active-low
IOBUF PORT "seg_0_0" IO_TYPE=LVCMOS33;
```

## Reset

A pin map has no reset pin. The wrapper holds the design in reset for 15
clock cycles after configuration, using a counter initialized by the FPGA's
configuration, then releases it.

## Unused outputs

Outputs that no pin uses are still connected inside the wrapper, so it lints
clean, and synthesis removes the logic that only they need.
