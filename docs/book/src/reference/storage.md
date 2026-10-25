# Arrays, memories, and ROMs

Sinter has two kinds of indexed storage, and they are deliberately different
types:

- an **array** `[T; N]` is `N` separate registers. Every element can be read
  in the same cycle, at any index, combinationally. It costs flip-flops and
  multiplexers.
- a **memory** `mem<T, N>` is block RAM. It has a fixed number of ports, and
  a read takes one cycle. It costs RAM blocks.

Pick an array for a handful of entries that you need all at once, and a
memory for anything deep.

## Register files

An array field with runtime-indexed reads and `set` is a register file. This
one has two read ports and one write port, with register 0 hardwired to zero
as in RISC-V:

```sinter
struct RegFile {
    pub regs: [u32; 8] = [0; 8],
}

impl RegFile {
    #[clock(domain = sys)]
    fn tick(self, we: u1, rd: u3, wdata: u32, rs1: u3, rs2: u3) -> (RegFile, (u32, u32)) {
        let a = self.regs[rs1];
        let b = self.regs[rs2];
        let regs = if we == 1 & rd != 0 { self.regs.set(rd, wdata) } else { self.regs };
        (RegFile { regs: regs }, (a, b))
    }
}
```

Each runtime read is a multiplexer over all elements, so reads are free in
latency and the number of read ports is unlimited, but each one costs logic
proportional to the array's size. A read past the end returns the last
element; a `set` past the end changes nothing.

## Memories

```sinter
struct Ram {
    pub data: mem<u8, 16> = 0,
}

impl Ram {
    #[clock(domain = sys)]
    fn tick(self, we: u1, waddr: u4, wdata: u8, raddr: u4) -> (Ram, u8) {
        let rd = self.data.value();
        (Ram { data: self.data.next(raddr, we, waddr, wdata) }, rd)
    }
}
```

`mem<T, N>` is `N` words of type `T`:

- `T` is an integer or fixed-point type. `N` is a power of two, at least 2.
- The field's reset value must be `= 0`. The contents start at zero but are
  **not** cleared by reset, as in real block RAM.

A memory is used through two methods:

- **`next(raddr, we, waddr, wdata)`** is the memory's step for this cycle. It
  starts a read of `raddr`, and if `we` is 1, writes `wdata` to `waddr`. It
  returns the memory, which must go into the next state. Addresses are
  unsigned; only their low log<sub>2</sub>`N` bits are used.
- **`value()`** is the word read by the *previous* cycle's `next`. Reads
  take one cycle, as in synchronous block RAM.

If a cycle reads and writes the same address, the read returns the old word
(read-first).

`next` must be called on the memory field itself, and its result must go
straight into the next state, outside any `if` or `match` arm: a memory's
ports can't be multiplexed. To write conditionally, pass a condition as
`we`:

```sinter,fail
~struct Ram {
~    pub data: mem<u8, 16> = 0,
~}
~impl Ram {
~    #[clock(domain = sys)]
~    fn tick(self, we: u1, waddr: u4, wdata: u8, raddr: u4) -> (Ram, u8) {
~        let rd = self.data.value();
        let data = if we == 1 { self.data.next(raddr, 1, waddr, wdata) } else { self.data };
~        (Ram { data: data }, rd)
~    }
~}
```

```text
error: a memory `next()` must be unconditional
```

### Read ports

`mem<T, N, R>` has `R` read ports and one write port. With `R` greater than 1,
`next` takes the read addresses as an array, and `value()` returns an array:

```sinter
struct Ram2 {
    ram: mem<u32, 16, 2> = 0,
}

impl Ram2 {
    #[clock(domain = sys)]
    fn tick(self, ra: u4, rb: u4, we: u1, wa: u4, wd: u32) -> (Ram2, (u32, u32)) {
        let rd = self.ram.value();
        (Ram2 { ram: self.ram.next([ra, rb], we, wa, wd) }, (rd[0], rd[1]))
    }
}
```

Synthesis tools typically build each read port from its own copy of the RAM.

### Byte enables

For a word whose width is a multiple of 8, `we` can be a mask with one bit
per byte: for a `mem<u32, N>`, a `u4`, where bit *i* enables bits
`8i + 7` to `8i`. Only the enabled bytes are written.

### Reset and simulation

A memory's read register (`value()`) resets to 0; its contents don't. The
write port isn't gated by reset, so if `we` can be 1 while `rst_n` is low,
the hardware writes during reset. Sinter's simulators start after reset, so
to match them, keep `we` at 0 in the reset state.

A `pub` memory field contributes its read register as an output,
`data_rdata_q` (one per port: `data_rdata_0_q`, ...).

## ROMs

`rom<T, N>` is a memory with contents fixed at compile time and no write
port. Its methods are `next(raddr)` and `value()`, with the same one-cycle
read:

```sinter
struct Squares {
    lut: rom<u8, 8> = [0, 1, 4, 9, 16, 25, 36, 49],
}

impl Squares {
    #[clock(domain = sys)]
    fn tick(self, addr: u3) -> (Squares, u8) {
        let v = self.lut.value();
        (Squares { lut: self.lut.next(addr) }, v)
    }
}
```

The contents are an array literal, or a file read at compile time with
`include_hex!`. At most `N` words are allowed; missing words are zero.
`rom<T, N, R>` has `R` read ports, as for `mem`.

### `include_hex!`

```sinter,project=hex,file=main.snt
struct Program {
    code: rom<u32, 256> = include_hex!("program.hex"),
}

impl Program {
    #[clock(domain = sys)]
    fn tick(self, pc: u8) -> (Program, u32) {
        (Program { code: self.code.next(pc) }, self.code.value())
    }
}
```

```text,project=hex,file=program.hex
// One word per line, in hex. Blank lines and // comments are skipped.
00500093
0x00100113
002081b3
```

The path is relative to the source file. Each line is one word, in hex, with
an optional `0x`; this is a subset of the `$readmemh` format. The contents are
written into the generated Verilog, so it needs no data file at synthesis
time.
