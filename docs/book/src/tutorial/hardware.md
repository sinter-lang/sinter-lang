# Into hardware

This chapter takes the transmitter from [A state machine](fsm.md) (the one
with `start` and `data` inputs) to an FPGA.

## Set the baud rate

A bit period is the clock frequency divided by the baud rate. For a 25 MHz
clock and 115200 baud that is 217 cycles, which fits the counter's `u8`:

```sinter,ignore
const BIT_CYCLES: u8 = 217;
```

A faster clock or slower baud rate might need a wider counter: make `count`
and `BIT_CYCLES` a `u16`, and the literal check (`217` must fit the type)
will tell you if they are too narrow.

## Emit the Verilog

```sh
sinter build uart.snt --emit verilog --out rtl
```

`rtl/Top.sv` is one self-contained SystemVerilog module, named after the
clock root's struct:

```systemverilog
module Top (
    input  wire         clk,    // sys clock
    input  wire         rst_n,  // active-low asynchronous reset
    input  wire  [0:0] start,
    input  wire  [7:0] data,
    output logic [0:0] line_q,
    output logic [0:0] out
);
```

- `clk` and `rst_n` come first. Every register resets asynchronously while
  `rst_n` is low.
- Then one input per parameter of `tick`.
- Then `line_q`, the `pub` register, and `out`, the extra output (`busy`).

The nested `UartTx` and `Baud` are flattened into this one module; their
registers have names like `tx_baud_count`.

## Use it from Verilog

Instantiate it like any module, with named connections:

```systemverilog
Top u_uart (
    .clk    (clk),
    .rst_n  (rst_n),
    .start  (send),
    .data   (byte_to_send),
    .line_q (uart_tx),
    .out    (uart_busy)
);
```

Drive `rst_n` from a reset synchronizer, as for any logic.
[The generated Verilog](../tools/generated-verilog.md) documents the naming
rules for structs, arrays, tuples, and `Option` ports.

## Or map it straight to pins

If the design *is* the whole FPGA, a pin map saves writing a top level. This
`board.pins` sends the byte set on eight switches whenever a button is held:

```text
# UART transmitter on a 25 MHz board. Pin numbers are examples: take yours
# from your board's constraints file.
set_io 15  clock sys          # 25 MHz oscillator
set_io 53  in    start        # push button
set_io 70  in    data[0]      # switches
set_io 71  in    data[1]
set_io 72  in    data[2]
set_io 73  in    data[3]
set_io 76  in    data[4]
set_io 77  in    data[5]
set_io 78  in    data[6]
set_io 79  in    data[7]
set_io 74  out   line         # serial out to the USB-UART bridge
```

```sh
sinter build uart.snt --emit verilog --out rtl --pins board.pins
```

This also writes `rtl/board.sv`, a top-level module with one port per pin
and a power-on reset, and `rtl/board.pcf` for nextpnr. For an iCE40:

```sh
yosys -p "read_verilog -sv rtl/board.sv rtl/Top.sv; synth_ice40 -top board -json uart.json"
nextpnr-ice40 --hx1k --package vq100 --json uart.json --pcf rtl/board.pcf --asc uart.asc
icepack uart.asc uart.bin
```

Open a terminal on the board's serial port at 115200 baud, set the switches,
and press the button. See [Board pin maps](../tools/pin-maps.md) for the file
format, including active-low pins.

## Where next

You have used most of the core language. The
[reference](../reference/lexical.md) covers the rest, including:

- [hierarchy](../reference/hierarchy.md), to keep blocks like `UartTx` as
  Verilog modules of their own,
- [memories and ROMs](../reference/storage.md) for block RAM,
- [pipelines](../reference/staged.md) for datapaths,
- [clock domains](../reference/clock-domains.md) and safe crossings between
  them, and
- [foreign Verilog](../reference/foreign-verilog.md), to use existing IP.

For a larger design, `examples/rv32/` in the repository has two RISC-V cores
written in Sinter.
