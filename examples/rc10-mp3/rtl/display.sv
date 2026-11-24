`timescale 1ns/1ps

// The two 7-segment digits ({dp, g, f, e, d, c, b, a}, active high; seg0 is
// the left digit). After an up/down press (vol_key) the volume shows for
// VOL_MS ms (a press restarts it); otherwise "M" (across both digits: two
// upside-down U's without their inner lower uprights, so the left digit
// draws a, b, e, f and the right a, b, c, f) and "P3" take turns, ROT_MS ms
// each, starting with M. The right
// digit's dot is lit while paused.
module display #(
    parameter int MS     = 48000,   // clocks per millisecond
    parameter int VOL_MS = 3000,
    parameter int ROT_MS = 1000
) (
    input  logic       clk,
    input  logic       vol_key,     // a one-cycle pulse per up/down press
    input  logic [3:0] vol,
    input  logic       paused,
    output logic [7:0] seg0,
    output logic [7:0] seg1
);
    localparam logic [7:0] M_L = 8'h33, M_R = 8'h27, P = 8'h73, THREE = 8'h4F;

    function automatic logic [7:0] digit(input logic [3:0] v);
        case (v)
            4'd0: digit = 8'h3F; 4'd1: digit = 8'h06; 4'd2: digit = 8'h5B; 4'd3: digit = 8'h4F;
            4'd4: digit = 8'h66; 4'd5: digit = 8'h6D; 4'd6: digit = 8'h7D; 4'd7: digit = 8'h07;
            4'd8: digit = 8'h7F; default: digit = 8'h6F;
        endcase
    endfunction

    // Millisecond tick.
    logic [$clog2(MS)-1:0] div = '0;
    logic ms = 1'b0;
    always_ff @(posedge clk) begin
        div <= (div == $clog2(MS)'(MS - 1)) ? '0 : div + 1'b1;
        ms  <= div == '0;
    end

    // Volume time left; the M/P3 turn and its time.
    logic [$clog2(VOL_MS + 1)-1:0] vol_left = '0;
    logic [$clog2(ROT_MS)-1:0]     rot_ms = '0;
    logic                          show_p3 = 1'b0;
    always_ff @(posedge clk) begin
        if (vol_key) begin
            vol_left <= $clog2(VOL_MS + 1)'(VOL_MS);
            rot_ms <= '0;
            show_p3 <= 1'b0;
        end else if (ms) begin
            if (vol_left != '0) begin
                vol_left <= vol_left - 1'b1;
            end else if (rot_ms == $clog2(ROT_MS)'(ROT_MS - 1)) begin
                rot_ms <= '0;
                show_p3 <= ~show_p3;
            end else begin
                rot_ms <= rot_ms + 1'b1;
            end
        end
    end

    always_ff @(posedge clk)
        if (vol_left != '0) begin
            seg0 <= (vol >= 4'd10) ? digit(4'd1) : 8'h00;
            seg1 <= digit((vol >= 4'd10) ? vol - 4'd10 : vol) | {paused, 7'b0};
        end else if (show_p3) begin
            seg0 <= P;
            seg1 <= THREE | {paused, 7'b0};
        end else begin
            seg0 <= M_L;
            seg1 <= M_R | {paused, 7'b0};
        end
endmodule
