`timescale 1ns/1ps
// RC10 USB bus probe: measures how the FX2's GPIF moves data to and from
// the FPGA, so a streaming design can be built on known timing.
//
// The FX2 is the bus master and its IFCLK is the FPGA's 48 MHz clock:
//   nCS low, nWR low   the FX2 drives a byte on D for the FPGA (host -> FPGA)
//   nCS low, nWR high  the FX2 samples D (FPGA -> host)
// but only while SELn (GPIF2, FX2 PA5) is low. The same waveforms read and
// write the NAND flash with SELn high; D[7:0] is shared, so the FPGA must
// ignore those and never drive D then.
//   FULL (RDY1) high   the FPGA can't take more; EMPTY (RDY0) high: nothing to read
// The probe never asserts FULL or EMPTY.
//
// Reads: each byte is f(cycles since nCS fell), so the bytes the host gets
// show exactly which cycles the FX2 sampled. Counts 0..127 are the counter
// itself; from 128 on, the bytes cycle through a 16-byte stats block:
//   0 A5 | 1-3 bytes written (LE) | 4 sum | 5 xor | 6 first | 7 last
//   8-9 read bursts | 10-11 write bursts | 12-13 cycles of the last write burst
//   14 5A | 15 C3
// Writes: every rising edge with nCS and nWR low counts as one byte.
// Display: bytes written (low 8 bits, hex); LEDs: read bursts. Joystick
// centre clears the counts.
module usb_probe (
    input  logic       clk,        // 48 MHz IFCLK from the FX2
    input  logic       fx2_ncs,    // T14, GPIF CTL2
    input  logic       fx2_nwr,    // V4,  GPIF CTL1
    input  logic       fx2_seln,   // P13, GPIF2 (FX2 PA5): 0 = FPGA, 1 = flash
    inout  wire  [7:0] fx2_d,
    output logic       fx2_full,   // R14, GPIF RDY1
    output logic       fx2_empty,  // P14, GPIF RDY0
    input  logic [4:0] joy,        // {select, down, up, right, left}; pressed = 1
    output logic [7:0] led,
    output logic [7:0] seg0,       // left digit  {dp, g, f, e, d, c, b, a}, active high
    output logic [7:0] seg1        // right digit
);
    // Bus inputs, registered on the same edge.
    logic       ncs_q = 1'b1, ncs_q2 = 1'b1, nwr_q = 1'b1;
    logic [7:0] d_q = '0;
    always_ff @(posedge clk) begin
        ncs_q  <= fx2_ncs | fx2_seln;   // a cycle for the flash is no cycle for us
        ncs_q2 <= ncs_q;
        nwr_q  <= fx2_nwr;
        d_q    <= fx2_d;
    end

    wire fell     = ncs_q2 & ~ncs_q;
    wire wr_byte  = ~ncs_q & ~nwr_q;

    logic        clear = 1'b0;
    always_ff @(posedge clk) clear <= joy[4];

    // Write statistics.
    logic [23:0] wcount = '0;
    logic [7:0]  wsum = '0, wxor = '0, wfirst = '0, wlast = '0;
    logic [15:0] rbursts = '0, wbursts = '0, wrun = '0;
    always_ff @(posedge clk) begin
        if (clear) begin
            wcount <= '0; wsum <= '0; wxor <= '0; wfirst <= '0; wlast <= '0;
            rbursts <= '0; wbursts <= '0; wrun <= '0;
        end else begin
            if (wr_byte) begin
                wcount <= wcount + 24'd1;
                wsum   <= wsum + d_q;
                wxor   <= wxor ^ d_q;
                wlast  <= d_q;
                if (wcount == '0) wfirst <= d_q;
            end
            if (fell & nwr_q)  rbursts <= rbursts + 16'd1;
            if (fell & ~nwr_q) begin
                wbursts <= wbursts + 16'd1;
                wrun    <= 16'd1;
            end else if (wr_byte) begin
                wrun <= wrun + 16'd1;
            end
        end
    end

    // Read data: the cycle count since nCS fell, then the stats block.
    logic [9:0] cnt = '1;
    always_ff @(posedge clk)
        if (fell) cnt <= 10'd0;
        else if (cnt != '1) cnt <= cnt + 10'd1;

    // Byte i of the stats block is stats[8*i +: 8].
    wire [127:0] stats = {8'hC3, 8'h5A, wrun, wbursts, rbursts, wlast, wfirst,
                          wxor, wsum, wcount, 8'hA5};
    wire [7:0] stat = stats[{cnt[3:0], 3'b000} +: 8];

    // Drive D only while the FX2 is reading (nCS low, nWR high), registered.
    logic [7:0] dout = '0;
    logic       oe = 1'b0;
    always_ff @(posedge clk) begin
        dout <= (cnt < 10'd128) ? cnt[7:0] : stat;
        oe   <= ~ncs_q & nwr_q;
    end
    assign fx2_d     = oe ? dout : 8'bz;
    assign fx2_full  = 1'b0;
    assign fx2_empty = 1'b0;

    // Display and LEDs.
    function automatic logic [7:0] hex7(input logic [3:0] v);
        case (v)
            4'h0: hex7 = 8'h3F; 4'h1: hex7 = 8'h06; 4'h2: hex7 = 8'h5B; 4'h3: hex7 = 8'h4F;
            4'h4: hex7 = 8'h66; 4'h5: hex7 = 8'h6D; 4'h6: hex7 = 8'h7D; 4'h7: hex7 = 8'h07;
            4'h8: hex7 = 8'h7F; 4'h9: hex7 = 8'h6F; 4'hA: hex7 = 8'h77; 4'hB: hex7 = 8'h7C;
            4'hC: hex7 = 8'h39; 4'hD: hex7 = 8'h5E; 4'hE: hex7 = 8'h79; default: hex7 = 8'h71;
        endcase
    endfunction
    always_ff @(posedge clk) begin
        seg0 <= hex7(wcount[7:4]);
        seg1 <= hex7(wcount[3:0]);
        led  <= rbursts[7:0];
    end
endmodule
