`timescale 1ns/1ps

// Dual-clock FIFO in block RAM, 2^AW words. Pointers cross between the
// clocks in Gray code through two flip-flops (names ending _s1 are the first
// stage: the UCF exempts paths into them from timing).
module async_fifo #(
    parameter int AW = 12,
    parameter int W  = 32
) (
    input  logic          wclk,
    input  logic          we,             // only when count < 2^AW
    input  logic [W-1:0]  wdata,
    output logic [AW:0]   count,          // words held, as the write side sees it
    input  logic          rclk,
    input  logic          re,             // only when avail
    output logic [W-1:0]  rdata,          // the word at the read pointer
    output logic          avail
);
    function automatic logic [AW:0] to_gray(input logic [AW:0] b);
        to_gray = b ^ (b >> 1);
    endfunction
    function automatic logic [AW:0] from_gray(input logic [AW:0] g);
        from_gray[AW] = g[AW];
        for (int i = AW - 1; i >= 0; i--) from_gray[i] = from_gray[i+1] ^ g[i];
    endfunction

    logic [W-1:0] mem [0:(1 << AW) - 1];
    logic [AW:0] wbin = '0, wgray = '0, rgray_s1 = '0, rgray_s2 = '0;
    logic [AW:0] rbin = '0, rgray = '0, wgray_s1 = '0, wgray_s2 = '0;

    // Write side.
    always_ff @(posedge wclk) begin
        if (we) mem[wbin[AW-1:0]] <= wdata;
        wbin     <= wbin + (AW+1)'(we);
        wgray    <= to_gray(wbin + (AW+1)'(we));
        rgray_s1 <= rgray;
        rgray_s2 <= rgray_s1;
    end
    assign count = wbin - from_gray(rgray_s2);

    // Read side.
    always_ff @(posedge rclk) begin
        rdata    <= mem[rbin[AW-1:0]];
        rbin     <= rbin + (AW+1)'(re);
        rgray    <= to_gray(rbin + (AW+1)'(re));
        wgray_s1 <= wgray;
        wgray_s2 <= wgray_s1;
    end
    assign avail = wgray_s2 != rgray;
endmodule
