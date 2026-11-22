`timescale 1ns/1ps

// Peak level with a 32 ms decay, shown as an 8-step bar on a log-ish scale.
module level_meter (
    input  logic       clk,
    input  logic       rst,
    input  logic       tick_ms,
    input  logic       smp,
    input  logic [9:0] level,
    output logic [7:0] led
);
    logic [15:0] peak;   // level in [15:6]
    always_ff @(posedge clk) begin
        if (rst) peak <= 16'd0;
        else if (smp && {level, 6'd0} > peak) peak <= {level, 6'd0};
        else if (tick_ms) peak <= peak - (peak >> 5);
    end
    wire [9:0] p = peak[15:6];
    always_ff @(posedge clk) begin
        led <= {p >= 10'd400, p >= 10'd280, p >= 10'd192, p >= 10'd128,
                p >= 10'd80,  p >= 10'd48,  p >= 10'd24,  p >= 10'd12};
    end
endmodule
