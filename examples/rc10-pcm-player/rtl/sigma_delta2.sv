`timescale 1ns/1ps

// Second-order 1-bit sigma-delta DAC, noise shaped by (1 - z^-1)^2:
//   v = i2 + 2*i1;  y = (v >= 0);  i2 += i1;  i1 += x - (y ? +FS : -FS)
// At 48 MHz and 48 kHz audio this models at 95-100 dB SNR in 20 kHz
// (16-bit input). Keep |x| below about 0.9 of full scale for stability; the
// integrators then stay under 2^20 (26 bits leaves margin).
module sigma_delta2 (
    input  logic               clk,
    input  logic signed [15:0] x,
    output logic               y
);
    logic signed [25:0] i1 = '0, i2 = '0;
    // x +/- full scale, ready before the loop needs them.
    logic signed [25:0] x_lo = '0, x_hi = '0;
    always_ff @(posedge clk) begin
        x_lo <= 26'(x) - 26'sd32768;     // y = 1: subtract FS
        x_hi <= 26'(x) + 26'sd32768;     // y = 0: add FS
    end
    wire signed [25:0] v = i2 + (i1 <<< 1);
    wire               s = ~v[25];
    always_ff @(posedge clk) begin
        i2 <= i2 + i1;
        i1 <= i1 + (s ? x_lo : x_hi);
        y  <= s;
    end
endmodule
