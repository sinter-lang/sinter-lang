# Foreign Verilog modules

A design can instantiate an existing Verilog module: IP you already have, or
a vendor primitive. You describe the module to Sinter with a **binding**: a
state struct marked `#[verilog(...)]` whose one method is a **reference
model** of the module, written in Sinter.

- The simulators run the model.
- The generated Verilog instantiates the real module instead.

Because Sinter's own tests (and yours) can co-simulate the generated Verilog
with Verilator against the simulator, a model that doesn't match its module
shows up as a mismatch rather than a surprise in hardware.

```sinter,project=acc,file=main.snt
// A binding for the module `acc16` in rtl/acc16.v.
#[verilog(module = "acc16", src = "rtl/acc16.v")]
struct Acc16 {
    acc: u16 = 0,
}

// The module's outputs: field names are output port names.
struct Acc16Out {
    sum: u16,
    ovf: u1,
}

impl Acc16 {
    // One clock cycle of `acc16`. Parameter names are its input port names.
    fn step(self, din: u8, clear: u1) -> (Acc16, Acc16Out) {
        let wide = self.acc + din;
        let next: u16 = if clear == 1 { 0 } else { wide as u16 wrap };
        (Acc16 { acc: next }, Acc16Out { sum: self.acc, ovf: wide[16:16] })
    }
}

struct Top {
    acc: Acc16,
    pub overflows: u8 = 0,
}

impl Top {
    #[clock(domain = sys)]
    fn tick(self, din: u8, clear: u1) -> (Top, u16) {
        let (acc, o) = self.acc.step(din, clear);
        let overflows: u8 = if o.ovf == 1 { (self.overflows + 1) as u8 wrap } else { self.overflows };
        (Top { acc: acc, overflows: overflows }, o.sum)
    }
}
```

```systemverilog,project=acc,file=rtl/acc16.v
`default_nettype none

module acc16 (
    input  wire         clk,
    input  wire         rst_n,
    input  wire  [7:0]  din,
    input  wire         clear,
    output logic [15:0] sum,
    output logic        ovf
);
    logic [15:0] acc;
    logic [16:0] wide;

    always_comb begin
        wide = {1'b0, acc} + {9'b0, din};
        sum = acc;
        ovf = wide[16];
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            acc <= 16'h0;
        end else begin
            acc <= clear ? 16'h0 : wide[15:0];
        end
    end
endmodule

`default_nettype wire
```

## The binding

`#[verilog(...)]` takes:

| Argument | Meaning |
|---|---|
| `module = "name"` | the Verilog module's name (required) |
| `src = "path.v"` | its source file, relative to the `.snt` file |
| `clk = "name"` | the module's clock port, if not `clk` |
| `rst_n = "name"` | its active-low reset port, if not `rst_n` |
| `rst_n = none` | the module has no reset port |

The struct has exactly one method, which takes `self` and returns
`(Self, Out)`:

- Its parameters map by name to the module's **input** ports, and the fields
  of `Out` to its **output** ports. Ports are scalars (integers or
  fixed-point).
- The struct's fields are the model's state. They exist only in simulation:
  the real module has its own registers.
- If the model computes an output from an input in the same cycle (like
  `ovf` above), the module is treated as having that combinational path. The
  model is the contract for timing too.

A binding with state gets the enclosing domain's clock and reset connected to
its `clk` and `rst_n` ports. A binding with **no** fields is combinational:
it gets no clock or reset, and each call is a separate instance.

## Rules

- A stateful binding must be stepped **exactly once per cycle**, through its
  own field (`self.acc.step(..)`), outside any `if` or `match` arm, and the
  result must go back into that field. The real module is clocked every
  cycle; to make it hold, give it inputs that make it hold.
- Nothing outside the model may read the model's fields (`self.acc.acc` from
  `Top` is an error): they don't exist in hardware. Use the module's outputs.
- Verilog parameters (`#(...)`) can't be overridden yet; the module is
  instantiated with its defaults.

## In the generated Verilog

The instance is named `u_<field>` (`u_acc`), or `u_<module>_<n>` for a
combinational binding. `sinter build` copies the `src` file next to the
emitted module, so the output directory is a complete file list; without
`src`, it warns that you must supply the module yourself.

Modules that generate clocks, such as PLLs, can't be bound yet. Instantiate
them in a hand-written top level that wraps the Sinter module.
