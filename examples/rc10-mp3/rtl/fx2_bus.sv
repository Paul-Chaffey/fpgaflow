`timescale 1ns/1ps

// FPGA side of the RC10's FX2 GPIF bus (docs/rc10/usb-protocol.md), as
// measured with examples/rc10-usb-probe. The FX2 is the master and its IFCLK
// is our 48 MHz clock:
//   SELn low, nCS low, nWR low   a byte from the host on D (one per cycle)
//   SELn low, nCS low, nWR high  the FX2 samples D, one byte per cycle
//   SELn high                    the FX2 is using the bus for the NAND flash:
//                                ignore it and never drive D
// Reads return the 2^SB-byte `status` (byte i = status[8i +: 8]), captured
// when the read starts and repeated if more is read.
module fx2_bus #(
    parameter int IDLE_BITS = 20,         // 2^20 cycles (22 ms) without a byte = idle
    parameter int SB = 4                  // status bytes = 2^SB
) (
    input  logic         clk,
    input  logic         fx2_ncs,
    input  logic         fx2_nwr,
    input  logic         fx2_seln,
    inout  wire  [7:0]   fx2_d,
    output logic         rx_valid,        // a byte from the host this cycle
    output logic [7:0]   rx_data,
    output logic         idle,            // no byte for 2^IDLE_BITS cycles
    input  logic [8*(1<<SB)-1:0] status
);
    logic       ncs_q = 1'b1, ncs_q2 = 1'b1, nwr_q = 1'b1;
    logic [7:0] d_q = '0;
    always_ff @(posedge clk) begin
        ncs_q  <= fx2_ncs | fx2_seln;
        ncs_q2 <= ncs_q;
        nwr_q  <= fx2_nwr;
        d_q    <= fx2_d;
    end
    wire fell = ncs_q2 & ~ncs_q;

    assign rx_valid = ~ncs_q & ~nwr_q;
    assign rx_data  = d_q;

    logic [IDLE_BITS:0] quiet = {1'b1, {IDLE_BITS{1'b0}}};   // idle at power-up
    always_ff @(posedge clk)
        if (rx_valid) quiet <= '0;
        else if (!quiet[IDLE_BITS]) quiet <= quiet + 1'b1;
    assign idle = quiet[IDLE_BITS];

    // Reads: the first byte the FX2 samples is the one registered from
    // count 0 (measured), so count from the edge nCS is seen low.
    logic [SB-1:0]          idx = '0;
    logic [8*(1<<SB)-1:0]   snap = '0;
    always_ff @(posedge clk)
        if (fell) begin
            idx  <= '0;
            snap <= status;
        end else begin
            idx <= idx + 1'b1;
        end

    logic [7:0] dout = '0;
    logic       oe = 1'b0;
    always_ff @(posedge clk) begin
        dout <= snap[{idx, 3'b000} +: 8];
        oe   <= ~ncs_q & nwr_q;
    end
    assign fx2_d = oe ? dout : 8'bz;
endmodule
