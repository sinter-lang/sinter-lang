# RV32I cores

Two RV32I processors in Sinter (DESIGN §9.12) that share a decoder
(`decode.snt`), an ALU (`alu.snt`), a load / store unit (`lsu.snt`), and a
program (`prog.s`, assembled to `prog.hex`):

| Root | Design | Cycles per instruction |
|---|---|---|
| `mini.snt` | Fetch-ahead state machine (§9.12 step 5) | 1, or 2 for a load |
| `pipe.snt` | Classic 5-stage pipeline: forwarding, load-use stall, branches resolved in EX, register file in a 2-read-port `mem` (§9.12 step 6) | 1, plus a bubble for each load-use pair or taken branch |

Both implement all of RV32I's integer instructions, including byte and
halfword loads and stores, with the same memory map:

- **Instructions:** a 256-word `rom`.
- **Data:** a 256-word `mem` with byte enables (word `addr[9:2]`, byte
  `addr[1:0]`).
- **Output port:** a store of any size to an address with bit 31 set sets the
  `port` output to the whole source register and increments `port_count`.
- **Halt:** `ECALL`, `EBREAK`, an illegal instruction, or a misaligned
  halfword or word access halts the core. The last two also fail a
  simulation assertion.

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

| Core | LUTs | FFs | Block RAMs | Distributed RAM | Fmax | Cycles to halt | Time |
|---|---|---|---|---|---|---|---|
| `mini` | 5,762 | 1,196 | 2 | — | 31.6 MHz | 182 | 5.8 µs |
| `pipe` | 2,204 | 609 | 2 | 32 cells | 45.9 MHz | 207 | 4.5 µs |

`pipe` holds its register file in a `mem<u32, 32, 2>` (MEM2, DESIGN §9.6),
which maps to distributed RAM. That makes it less than half `mini`'s size,
whose register file is 32 × 32 flip-flops with multiplexed read ports. A
synchronous register file costs a cycle before its data is available. The
pipeline hides that between ID and EX, but `mini` decodes and executes in
one cycle, so it keeps the flip-flops.
