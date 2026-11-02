`default_nettype none

// mul_pipe: an 8 x 8 multiplier with STAGES registers after it, so a product
// comes out STAGES clocks after its operands go in.
//
// Stands in for parameterized IP (a DSP block's register stages):
// examples/verilog_params/main.snt sets STAGES through `#[params]`, and the
// conformance oracle checks the binding's model against this file at more
// than one value.
module mul_pipe #(
    parameter integer STAGES = 1  // register stages after the multiply (>= 1)
) (
    input  wire         clk,    // clock
    input  wire         rst_n,  // active-low asynchronous reset
    input  wire  [7:0]  a,      // multiplicand
    input  wire  [7:0]  b,      // multiplier
    output logic [15:0] p       // a * b from STAGES clocks ago
);
    // The product, then its delayed copies.
    logic [15:0] pipe [0:STAGES-1];

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            for (int i = 0; i < STAGES; i = i + 1) begin
                pipe[i] <= 16'h0;
            end
        end else begin
            pipe[0] <= 16'(a) * 16'(b);
            for (int i = 1; i < STAGES; i = i + 1) begin
                pipe[i] <= pipe[i - 1];
            end
        end
    end

    assign p = pipe[STAGES - 1];
endmodule

`default_nettype wire
