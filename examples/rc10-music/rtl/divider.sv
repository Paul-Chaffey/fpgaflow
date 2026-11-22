`timescale 1ns/1ps

// One-cycle pulse every DIV clocks.
module divider #(
    parameter int unsigned DIV = 48_000
) (
    input  logic clk,
    input  logic rst,
    output logic tick
);
    localparam int unsigned W = $clog2(DIV);
    localparam logic [W-1:0] LAST = W'(DIV - 1);
    logic [W-1:0] cnt;
    always_ff @(posedge clk) begin
        if (rst || cnt == LAST) cnt <= '0;
        else cnt <= cnt + 1'b1;
        tick <= !rst && cnt == LAST;
    end
endmodule
