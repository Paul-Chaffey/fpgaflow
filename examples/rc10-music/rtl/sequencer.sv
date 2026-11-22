`timescale 1ns/1ps

// Steps through a tune from the ROM, one entry per note.
//
// note_on pulses when a new melody/bass pair starts (mel/bass change on the
// same edge). eighth pulses at the start of every eighth note, with pos its
// place in the bar (0..7); the bar restarts at the top of each tune.
module sequencer #(
    parameter int unsigned EIGHTH = 11_250_000   // clocks per eighth note
) (
    input  logic       clk,
    input  logic       rst,
    input  logic       run,       // 0 = paused (time stands still)
    input  logic [1:0] tune,
    input  logic       restart,   // start the tune from the top
    output logic [5:0] mel,       // 63 = rest
    output logic [5:0] bass,
    output logic       note_on,
    output logic       eighth,
    output logic [2:0] pos
);
    localparam int unsigned CW = $clog2(EIGHTH);
    localparam logic [CW-1:0] LAST = CW'(EIGHTH - 1);
    localparam logic [5:0] REST = 6'd63;

    logic [CW-1:0] cnt;
    logic [5:0]    idx;
    logic [2:0]    left;      // eighths left in this note, minus one
    logic          load;      // latch the ROM entry at idx next cycle
    logic [15:0]   rom;

    tunes u_rom (.addr({tune, idx}), .data(rom));

    always_ff @(posedge clk) begin
        note_on <= 1'b0;
        eighth  <= 1'b0;
        if (rst || restart) begin
            idx    <= 6'd0;
            cnt    <= '0;
            pos    <= 3'd0;
            left   <= 3'd0;
            mel    <= REST;
            bass   <= REST;
            load   <= 1'b1;
            eighth <= 1'b1;
        end else begin
            // The eighth-note counter never stops for a load, so note
            // boundaries stay exactly len * EIGHTH clocks apart.
            if (load) begin
                mel     <= rom[14:9];
                bass    <= rom[5:0];
                left    <= rom[8:6];
                note_on <= 1'b1;
                load    <= 1'b0;
            end
            if (run) begin
                if (cnt == LAST) begin
                    cnt    <= '0;
                    eighth <= 1'b1;
                    pos    <= pos + 3'd1;
                    if (left == 3'd0) begin
                        load <= 1'b1;
                        if (rom[15]) begin
                            idx <= 6'd0;
                            pos <= 3'd0;
                        end else begin
                            idx <= idx + 6'd1;
                        end
                    end else begin
                        left <= left - 3'd1;
                    end
                end else begin
                    cnt <= cnt + 1'b1;
            end
            end
        end
    end
endmodule
