`timescale 1ns/1ps

// First-order sigma-delta DAC: output density = x / 1024.
module sigma_delta (
    input  logic       clk,
    input  logic       rst,
    input  logic [9:0] x,
    output logic       y
);
    logic [10:0] acc;
    always_ff @(posedge clk) begin
        if (rst) acc <= 11'd0;
        else acc <= {1'b0, acc[9:0]} + {1'b0, x};
    end
    assign y = acc[10];
endmodule
