`timescale 1ns/1ps

// 7-segment patterns for the RC10 music demo. Segment bytes are
// {dp, g, f, e, d, c, b, a}, active high (common-cathode display); seg0 is
// the left digit.
module segments (
    input  logic       clk,
    input  logic       rst,
    input  logic       tick_ms,
    input  logic       eighth,
    input  logic [2:0] pos,
    input  logic [1:0] mode,       // 0 snake, 1 note name, 2 VU columns, 3 sparkle
    input  logic       run,
    input  logic [1:0] tune,
    input  logic       tune_change,
    input  logic [5:0] mel,
    input  logic [7:0] mel_env,
    input  logic [7:0] kick_env,
    output logic [7:0] seg0,
    output logic [7:0] seg1
);
    localparam logic [7:0] S_A = 8'h01, S_B = 8'h02, S_C = 8'h04, S_D = 8'h08,
                           S_E = 8'h10, S_F = 8'h20, S_G = 8'h40, S_DP = 8'h80;

    function automatic logic [7:0] digit(input logic [3:0] d);
        case (d)
            4'd0: digit = 8'h3f;  4'd1: digit = 8'h06;  4'd2: digit = 8'h5b;
            4'd3: digit = 8'h4f;  4'd4: digit = 8'h66;  4'd5: digit = 8'h6d;
            4'd6: digit = 8'h7d;  4'd7: digit = 8'h07;  4'd8: digit = 8'h7f;
            4'd9: digit = 8'h6f;  default: digit = S_G;   // -
        endcase
    endfunction

    // Note letter for a pitch class (sharps get the decimal point).
    function automatic logic [7:0] letter(input logic [3:0] pc);
        case (pc)
            4'd0:  letter = 8'h39;          // C
            4'd1:  letter = 8'h39 | S_DP;   // C#
            4'd2:  letter = 8'h5e;          // d
            4'd3:  letter = 8'h5e | S_DP;
            4'd4:  letter = 8'h79;          // E
            4'd5:  letter = 8'h71;          // F
            4'd6:  letter = 8'h71 | S_DP;
            4'd7:  letter = 8'h3d;          // G
            4'd8:  letter = 8'h3d | S_DP;
            4'd9:  letter = 8'h77;          // A
            4'd10: letter = 8'h77 | S_DP;
            4'd11: letter = 8'h7c;          // b
            default: letter = 8'h40;
        endcase
    endfunction

    // A column that rises from the bottom segment with the level.
    function automatic logic [7:0] column(input logic [7:0] v);
        if (v >= 8'd200) column = 8'h7f;
        else if (v >= 8'd140) column = 8'h7e;
        else if (v >= 8'd80) column = 8'h5c;
        else if (v >= 8'd30) column = 8'h1c;
        else if (v >= 8'd8) column = 8'h08;
        else column = 8'h00;
    endfunction

    // The snake's track round both digits: left a, right a, b, c, d, left d, e, f.
    function automatic logic [15:0] ring(input logic [2:0] s);
        case (s)
            3'd0: ring = {S_A, 8'h00};
            3'd1: ring = {8'h00, S_A};
            3'd2: ring = {8'h00, S_B};
            3'd3: ring = {8'h00, S_C};
            3'd4: ring = {8'h00, S_D};
            3'd5: ring = {S_D, 8'h00};
            3'd6: ring = {S_E, 8'h00};
            default: ring = {S_F, 8'h00};
        endcase
    endfunction

    logic [2:0]  step;
    logic [14:0] lfsr;
    logic [13:0] sparkle;
    logic [9:0]  overlay_ms;     // time left showing "tN" after a tune change

    always_ff @(posedge clk) begin
        if (rst) begin
            step       <= 3'd0;
            lfsr       <= 15'h1;
            sparkle    <= 14'd0;
            overlay_ms <= 10'd0;
        end else begin
            if (eighth && run) begin
                step    <= step + 3'd1;
                lfsr    <= {lfsr[13:0], lfsr[14] ^ lfsr[13]};
                sparkle <= {lfsr[13:7], lfsr[6:0]};
            end
            if (tune_change) overlay_ms <= 10'd1000;
            else if (tick_ms && overlay_ms != 10'd0) overlay_ms <= overlay_ms - 10'd1;
        end
    end

    // mel = MIDI - 36, so pitch class = mel % 12 and octave = mel / 12 + 2.
    // A lookup instead of a divider: the divider's subtract chains were the
    // slowest path in the design at 48 MHz.
    logic [3:0] pc, octave;
    always_comb begin
        pc = 4'(mel) ;
        octave = 4'd2;
        if (mel >= 6'd60)      begin pc = 4'(mel - 6'd60); octave = 4'd7; end
        else if (mel >= 6'd48) begin pc = 4'(mel - 6'd48); octave = 4'd6; end
        else if (mel >= 6'd36) begin pc = 4'(mel - 6'd36); octave = 4'd5; end
        else if (mel >= 6'd24) begin pc = 4'(mel - 6'd24); octave = 4'd4; end
        else if (mel >= 6'd12) begin pc = 4'(mel - 6'd12); octave = 4'd3; end
    end

    logic [15:0] pat;
    always_comb begin
        case (mode)
            // Decimal points: left on the first beat of the bar, right on every beat.
            2'd0: pat = ring(step) | ring(step - 3'd1)
                      | {(pos == 3'd0 ? S_DP : 8'h00), (pos[0] ? 8'h00 : S_DP)};
            2'd1: pat = (mel == 6'd63) ? {S_G, S_G} : {letter(pc), digit(octave)};
            2'd2: pat = {column(kick_env), column(mel_env)};
            default: pat = {1'b0, sparkle[13:7], 1'b0, sparkle[6:0]};
        endcase
        if (overlay_ms != 10'd0) pat = {8'h78, digit({2'b00, tune} + 4'd1)};  // "t1".."t3"
        else if (!run) pat = {8'h73, 8'h77};                                 // "PA"
    end

    always_ff @(posedge clk) {seg0, seg1} <= pat;
endmodule
