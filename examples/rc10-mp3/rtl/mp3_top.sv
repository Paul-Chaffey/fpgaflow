`timescale 1ns/1ps

// RC10 MP3 decoder, milestone 1: the front end on the board.
// `fpgaflow-celoxica mp3-check FILE` streams an MP3 over USB and compares
// what the front end found with crates/mp3model.
//
// Status (USB read, 32 bytes):
//   0-1   "M1"
//   2-5   bytes received in this stream
//   6-9   frames found
//   10-13 bytes inside those frames
//   14-17 CRC-32 of the frames' records (see crates/mp3model/src/frontend.rs)
//   18-21 last frame header
//   22-23 frames whose main data begins before the stream
//   24    input buffer fill, 0-255
//   25    {idle, full, 6'b0}
//   31    C3
// Display: frames found (low byte, hex). LEDs 4-7: buffer fill; LED 0: idle.
module mp3_top #(
    parameter int IDLE_BITS = 20
) (
    input  logic       clk,        // 48 MHz IFCLK from the FX2
    input  logic       fx2_ncs,
    input  logic       fx2_nwr,
    input  logic       fx2_seln,   // 0 = FPGA, 1 = NAND flash
    inout  wire  [7:0] fx2_d,
    output logic       fx2_full,
    output logic       fx2_empty,
    output logic [7:0] led,
    output logic [7:0] seg0,
    output logic [7:0] seg1
);
    logic         rx_valid, idle;
    logic [7:0]   rx_data;
    logic [255:0] status;
    fx2_bus #(.IDLE_BITS(IDLE_BITS), .SB(5)) u_bus (
        .clk, .fx2_ncs, .fx2_nwr, .fx2_seln, .fx2_d,
        .rx_valid, .rx_data, .idle, .status
    );
    assign fx2_empty = 1'b0;
    wire new_stream = rx_valid & idle;

    logic        full, rec_valid;
    logic [7:0]  rec_data, res_rdata;
    logic [31:0] frames, frame_bytes, crc, last_header;
    logic [15:0] undecodable;
    logic [12:0] fill;
    mp3_frontend u_fe (
        .clk, .in_valid(rx_valid), .in_data(rx_data), .new_stream, .idle, .full,
        .rec_valid, .rec_data, .frames, .frame_bytes, .undecodable, .crc,
        .last_header, .fill, .res_raddr(12'd0), .res_rdata
    );
    assign fx2_full = full;

    logic [31:0] rx_bytes = '0;
    always_ff @(posedge clk)
        if (new_stream) rx_bytes <= 32'd1;
        else if (rx_valid) rx_bytes <= rx_bytes + 1'b1;

    wire [7:0] fill8 = fill[12] ? 8'hFF : fill[11:4];
    // Byte i of the status is status[8i +: 8]. Listed from byte 31 down, a
    // 32-bit value placed as it is comes out little-endian; the header is
    // reversed so its bytes come out in stream order.
    assign status = {
        8'hC3, 40'd0, {idle, full, 6'd0}, fill8, undecodable,
        last_header[7:0], last_header[15:8], last_header[23:16], last_header[31:24],
        crc, frame_bytes, frames, rx_bytes, 8'h31, 8'h4D
    };

    function automatic logic [7:0] hex7(input logic [3:0] v);
        case (v)
            4'h0: hex7 = 8'h3F; 4'h1: hex7 = 8'h06; 4'h2: hex7 = 8'h5B; 4'h3: hex7 = 8'h4F;
            4'h4: hex7 = 8'h66; 4'h5: hex7 = 8'h6D; 4'h6: hex7 = 8'h7D; 4'h7: hex7 = 8'h07;
            4'h8: hex7 = 8'h7F; 4'h9: hex7 = 8'h6F; 4'hA: hex7 = 8'h77; 4'hB: hex7 = 8'h7C;
            4'hC: hex7 = 8'h39; 4'hD: hex7 = 8'h5E; 4'hE: hex7 = 8'h79; default: hex7 = 8'h71;
        endcase
    endfunction
    always_ff @(posedge clk) begin
        seg0 <= hex7(frames[7:4]);
        seg1 <= hex7(frames[3:0]);
        led[0] <= idle;
        led[3:1] <= '0;
        for (int i = 0; i < 4; i++) led[4 + i] <= fill8 > 8'(i * 64);
    end
endmodule
