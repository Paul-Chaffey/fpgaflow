`timescale 1ns/1ps

// Free-running counter; the LED shows its top bit.
module blinky #(
    parameter int unsigned WIDTH = 25   // 50 MHz / 2^25 ~ 1.5 Hz toggle
) (
    input  logic clk,
    input  logic rst,   // active-high, synchronous
    output logic led
);
    logic [WIDTH-1:0] count;

    always_ff @(posedge clk) begin
        if (rst) count <= '0;
        else     count <= count + 1'b1;
    end

    assign led = count[WIDTH-1];
endmodule
