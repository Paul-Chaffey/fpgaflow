`timescale 1ns/1ps

// The DACs' clock, from the FX2's 48 MHz:
//   "48": 48 MHz itself (48 kHz audio)
//   "A":  two DCMs, 48 x 21/20 x 7/8 = 44.1 MHz exactly (44.1 kHz audio)
//   "B":  one DCM,  48 x 23/25 = 44.16 MHz (44.16 kHz: 0.14% fast)
module audio_clock #(
    parameter MODE = "A"
) (
    input  logic clk48,
    output logic aclk
);
`ifdef SYNTHESIS
    if (MODE == "48") begin : g48
        assign aclk = clk48;
    end else if (MODE == "A") begin : gA
        logic fx1, fx1_g, fx2, locked1;
        DCM #(.CLKIN_PERIOD(20.833), .CLKFX_MULTIPLY(21), .CLKFX_DIVIDE(20),
              .CLK_FEEDBACK("NONE"), .DFS_FREQUENCY_MODE("LOW")) u_dcm1 (
            .CLKIN(clk48), .CLKFB(1'b0), .RST(1'b0), .DSSEN(1'b0), .PSCLK(1'b0),
            .PSEN(1'b0), .PSINCDEC(1'b0), .CLKFX(fx1), .LOCKED(locked1));
        BUFG u_bufg1 (.I(fx1), .O(fx1_g));
        DCM #(.CLKIN_PERIOD(19.841), .CLKFX_MULTIPLY(7), .CLKFX_DIVIDE(8),
              .CLK_FEEDBACK("NONE"), .DFS_FREQUENCY_MODE("LOW")) u_dcm2 (
            .CLKIN(fx1_g), .CLKFB(1'b0), .RST(~locked1), .DSSEN(1'b0), .PSCLK(1'b0),
            .PSEN(1'b0), .PSINCDEC(1'b0), .CLKFX(fx2));
        BUFG u_bufg2 (.I(fx2), .O(aclk));
    end else begin : gB
        logic fx;
        DCM #(.CLKIN_PERIOD(20.833), .CLKFX_MULTIPLY(23), .CLKFX_DIVIDE(25),
              .CLK_FEEDBACK("NONE"), .DFS_FREQUENCY_MODE("LOW")) u_dcm (
            .CLKIN(clk48), .CLKFB(1'b0), .RST(1'b0), .DSSEN(1'b0), .PSCLK(1'b0),
            .PSEN(1'b0), .PSINCDEC(1'b0), .CLKFX(fx));
        BUFG u_bufg (.I(fx), .O(aclk));
    end
`else
    // Simulation: an independent clock at the nominal frequency.
    if (MODE == "48") begin : g48
        assign aclk = clk48;
    end else begin : gdcm
        localparam real HALF = (MODE == "A") ? 11.3379 : 11.3225;
        logic c = 1'b0;
        always #(HALF) c = ~c;
        assign aclk = c;
    end
`endif
endmodule
