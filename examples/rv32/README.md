# RV32I cores

Two RV32I processors in Sinter (DESIGN §9.12) that share a decoder
(`decode.snt`), an ALU (`alu.snt`), and a program (`prog.s`, assembled to
`prog.hex`):

| Root | Design | Cycles per instruction |
|---|---|---|
| `mini.snt` | Fetch-ahead state machine (§9.12 step 5) | 1, or 2 for a load |
| `pipe.snt` | Classic 5-stage pipeline: forwarding, load-use stall, branches resolved in EX (§9.12 step 6) | 1, plus a bubble for each load-use pair or taken branch |

Both implement RV32I except byte and halfword loads and stores, with the
same memory map:

- **Instructions:** a 256-word `rom`.
- **Data:** a 256-word `mem` (word-addressed by `addr[9:2]`).
- **Output port:** a store to any address with bit 31 set updates the `port`
  output and increments `port_count`.
- **Halt:** `ECALL`, `EBREAK`, or an illegal instruction halts the core. An
  illegal instruction also fails a simulation assertion.

The `run` input is a clock enable.

```sh
sinter sim examples/rv32/pipe.snt --cycles 300 --vcd pipe.vcd
sinter build examples/rv32/pipe.snt --out build/rv32
```

## How they are checked

- `crates/sinter-compiler/tests/rv32.rs` assembles each program in
  `tests/rv32/programs/` and runs it on both cores and on a reference
  instruction-set simulator, with and without clock-enable stalls. The
  output-port writes, final registers, and final pc must all match.
- The conformance oracle co-simulates both cores against their own Verilog
  for 1000 cycles.

To change the program, edit `prog.s` and regenerate `prog.hex` with
`SINTER_BLESS=1 cargo test -p sinter-compiler --test rv32`.

## Results

Measured on Lattice ECP5-25K (Yosys `synth_ecp5`, nextpnr-ecp5), running
`prog.s`:

| Core | LUTs | FFs | Block RAMs | Fmax | Cycles to halt | Time |
|---|---|---|---|---|---|---|
| `mini` | 5,493 | 1,186 | 2 | 31.5 MHz | 182 | 5.8 µs |
| `pipe` | 5,247 | 1,557 | 2 | 46.7 MHz | 207 | 4.4 µs |

Most of the logic is the register file. It is 32 × 32 flip-flops with
multiplexed read ports, because Sinter's `mem` has a single read port
today. A multi-port `mem` (DESIGN §9.6) would move it into block RAM.
