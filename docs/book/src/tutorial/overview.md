# Tutorial: a UART transmitter

This tutorial builds a UART transmitter, the circuit that sends bytes over a
serial line one bit at a time. It is small enough to finish in an hour and
uses most of what you need for real designs:

1. [A baud-rate counter](baud.md): registers, reset values, constants, and
   width-checked arithmetic.
2. [A state machine](fsm.md): enums, `match`, methods, and state structs
   nested inside each other.
3. [Testing in simulation](testing.md): directed stimulus with expected
   outputs, waveforms, and assertions.
4. [Buffering with a FIFO](fifo.md): `Option`, and a queue that can't be
   overfilled.
5. [Into hardware](hardware.md): the generated Verilog, instantiating it, and
   mapping it to board pins.

You need `sinter` [installed](../getting-started/installation.md), and it
helps to have done [Your first design](../getting-started/first-design.md).

## The protocol

A UART line idles high. To send a byte, the transmitter drives:

1. a **start bit** (0),
2. the eight **data bits**, least significant first,
3. a **stop bit** (1),

holding each bit for one *bit period*. The bit period is the clock frequency
divided by the baud rate: at 25 MHz and 115200 baud, it is 217 clock cycles.
The receiver knows the baud rate and samples each bit in the middle.

```text
idle  start  d0  d1  d2  d3  d4  d5  d6  d7  stop  idle
 1      0    .   .   .   .   .   .   .   .    1     1
```
