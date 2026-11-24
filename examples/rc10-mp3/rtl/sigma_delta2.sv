`timescale 1ns/1ps

// Second-order 1-bit sigma-delta DAC, noise shaped by (1 - z^-1)^2:
//   v = i2 + 2*i1;  s = (v >= 0);  i2 += i1;  i1 += x - (s ? +FS : -FS)
//
// rz = 0 (NRZ): the modulator runs at 48 MHz and s goes straight to the pin.
//   Every transition adds the pin's rise/fall mismatch, and the number of
//   transitions depends on the signal: a 0.2 ns mismatch already limits the
//   audio band to ~70 dB (hiss).
// rz = 1 (return to zero): the modulator runs at 12 MHz and each bit goes out
//   as four 48 MHz slots, 1 s s 0. Every symbol has one rising and one
//   falling edge whatever s is, so edge mismatch is a constant offset; the
//   output swing is half that of NRZ.
// Modelled (16-bit input, 20 kHz band): ~100 dB either way with ideal edges;
// with edge mismatch NRZ falls to 50-70 dB, RZ stays at ~100 dB.
// Keep |x| below about 0.9 of full scale; the integrators then stay under
// 2^20 (26 bits leaves margin).
module sigma_delta2 (
    input  logic               clk,
    input  logic               rz,
    input  logic signed [15:0] x,
    output logic               y
);
    logic signed [25:0] i1 = '0, i2 = '0;
    logic signed [25:0] x_lo = '0, x_hi = '0;   // x -/+ full scale
    always_ff @(posedge clk) begin
        x_lo <= 26'(x) - 26'sd32768;
        x_hi <= 26'(x) + 26'sd32768;
    end
    logic [1:0] slot = '0;
    logic       s_q = 1'b0;
    wire signed [25:0] v = i2 + (i1 <<< 1);
    wire               s = ~v[25];
    wire               en = ~rz | (slot == 2'd0);
    always_ff @(posedge clk) begin
        slot <= slot + 2'd1;
        if (en) begin
            i2  <= i2 + i1;
            i1  <= i1 + (s ? x_lo : x_hi);
            s_q <= s;
        end
        if (!rz) y <= s;
        else case (slot)              // symbol 1, s, s, 0
            2'd0:    y <= 1'b1;
            2'd3:    y <= 1'b0;
            default: y <= s_q;
        endcase
    end
endmodule
