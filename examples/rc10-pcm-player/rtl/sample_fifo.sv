`timescale 1ns/1ps

// Stereo sample FIFO in block RAM: 2^AW words of {right, left}.
module sample_fifo #(
    parameter int AW = 12
) (
    input  logic        clk,
    input  logic        we,
    input  logic [31:0] wdata,
    input  logic        re,           // only when `avail`
    output logic [31:0] rdata,        // the word at the read pointer
    output logic        avail,        // rdata holds a written word
    output logic [AW:0] count         // words held
);
    logic [31:0] mem [0:(1 << AW) - 1];
    logic [AW:0] wptr = '0, rptr = '0, wptr_d = '0, wptr_d2 = '0;
    always_ff @(posedge clk) begin
        if (we) mem[wptr[AW-1:0]] <= wdata;
        rdata <= mem[rptr[AW-1:0]];
        wptr  <= wptr + AW'(we);
        rptr  <= rptr + AW'(re);
        // A word is readable two cycles after it was written.
        wptr_d  <= wptr;
        wptr_d2 <= wptr_d;
    end
    assign count = wptr - rptr;
    assign avail = wptr_d2 != rptr;
endmodule
