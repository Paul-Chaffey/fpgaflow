`timescale 1ns/1ps

// The USB stream as records (EQ plan stage 2): every stream from the host is
// a sequence of [type, len_hi, len_lo, payload], so settings can travel
// between pieces of the song:
//   0  MP3 bytes, passed to the front end
//   1  EQ coefficients: 10 bands x 5 terms, each 4 bytes LE (Q2.23, 25 bits
//      used), 200 bytes; written into the bank not in use, then the banks
//      swap and the EQ is on (a record of another length is ignored)
//   2  bass level: 1 byte, 0..3
//   3  pause: 1 byte, 0 = play, 1 = pause. Paused this way the top lets go of
//      FULL (so the record that resumes can get in) and the host must send
//      nothing but settings records until it resumes
//   4  EQ off (no payload)
// Any other type is skipped. A new stream (`new_stream`, the first byte after
// an idle gap) always starts with a type byte. MP3 payload bytes keep the
// front end's FULL handling; settings bytes are taken at once.
module stream_demux (
    input  logic        clk,
    input  logic        new_stream,
    input  logic        rx_valid,
    input  logic [7:0]  rx_data,
    output logic        mp3_valid,         // (combinational: as rx_valid)
    output logic [7:0]  mp3_data,
    // EQ coefficients (to eq.sv's write port) and state
    output logic        cw_en = 1'b0,
    output logic [6:0]  cw_addr = '0,      // {bank, 5 band + k}
    output logic [24:0] cw_data = '0,
    output logic        eq_on = 1'b0,
    output logic        eq_bank = 1'b0,    // the bank in use
    // settings for the controls (one-cycle pulses)
    output logic        set_bass = 1'b0,
    output logic [1:0]  bass_val = '0,
    output logic        set_pause = 1'b0,
    output logic        pause_val = 1'b0,
    // records taken, for the status (wrapping)
    output logic [7:0]  eq_sets = '0,
    output logic [7:0]  settings = '0
);
    typedef enum logic [1:0] { D_TYPE, D_LEN1, D_LEN2, D_PAY } dstate_t;
    dstate_t st = D_TYPE;
    logic [7:0]  typ = '0;
    logic [15:0] len = '0, got = '0;
    logic [23:0] part = '0;
    logic [5:0]  idx = '0;                 // coefficient 0..49

    wire first = new_stream || st == D_TYPE;
    assign mp3_valid = rx_valid && !new_stream && st == D_PAY && typ == 8'd0;
    assign mp3_data  = rx_data;

    always_ff @(posedge clk) begin
        cw_en <= 1'b0;
        set_bass <= 1'b0;
        set_pause <= 1'b0;
        if (rx_valid) begin
            if (first) begin
                typ <= rx_data;
                st <= D_LEN1;
            end else begin
                case (st)
                    D_LEN1: begin
                        len[15:8] <= rx_data;
                        st <= D_LEN2;
                    end
                    D_LEN2: begin
                        len <= {len[15:8], rx_data};
                        got <= '0;
                        idx <= '0;
                        if ({len[15:8], rx_data} == 16'd0) begin
                            st <= D_TYPE;
                            if (typ == 8'd4) begin
                                eq_on <= 1'b0;
                                settings <= settings + 1'b1;
                            end
                        end else begin
                            st <= D_PAY;
                        end
                    end
                    D_PAY: begin
                        got <= got + 1'b1;
                        if (got + 1'b1 == len) st <= D_TYPE;
                        case (typ)
                            8'd1:
                                if (len == 16'd200) begin
                                    if (got[1:0] != 2'd3) begin
                                        part <= {rx_data, part[23:8]};
                                    end else begin
                                        cw_en <= 1'b1;
                                        cw_addr <= {~eq_bank, idx};
                                        cw_data <= {rx_data[0], part};
                                        idx <= idx + 1'b1;
                                    end
                                    if (got == 16'd199) begin
                                        // The last write lands next cycle; the
                                        // swap reaches the DAC side later still.
                                        eq_bank <= ~eq_bank;
                                        eq_on <= 1'b1;
                                        eq_sets <= eq_sets + 1'b1;
                                    end
                                end
                            8'd2: begin
                                set_bass <= 1'b1;
                                bass_val <= rx_data[1:0];
                                settings <= settings + 1'b1;
                            end
                            8'd3: begin
                                set_pause <= 1'b1;
                                pause_val <= rx_data[0];
                                settings <= settings + 1'b1;
                            end
                            default: ;
                        endcase
                    end
                    default: st <= D_TYPE;
                endcase
            end
        end
    end
endmodule
