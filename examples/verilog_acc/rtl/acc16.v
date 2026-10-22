`default_nettype none

// acc16: a 16-bit accumulator with synchronous clear.
//
// Stands in for existing Verilog IP: examples/verilog_acc/main.snt instantiates
// it through a `#[verilog]` binding, and the conformance oracle checks the
// binding's Sinter reference model against this file.
module acc16 (
    input  wire         clk,    // clock
    input  wire         rst_n,  // active-low asynchronous reset
    input  wire  [7:0]  din,    // value to add this cycle
    input  wire         clear,  // synchronously clear the accumulator
    output logic [15:0] sum,    // registered running total
    output logic        ovf     // combinational: this cycle's add carries out
);
    // Running total.
    logic [15:0] acc;
    logic [15:0] acc_next;
    // Total plus this cycle's input, one bit wider for the carry.
    logic [16:0] wide;

    always_comb begin
        wide = {1'b0, acc} + {9'b0, din};
        acc_next = clear ? 16'h0 : wide[15:0];
        sum = acc;
        ovf = wide[16];
    end

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            acc <= 16'h0;
        end else begin
            acc <= acc_next;
        end
    end
endmodule

`default_nettype wire
