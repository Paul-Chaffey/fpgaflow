`timescale 1ns/1ps

// Self-checking testbench: with WIDTH=4 the LED must go high after
// exactly 8 clocks out of reset and low again after 16.
module tb_blinky;
    localparam int unsigned WIDTH = 4;

    logic clk = 1'b0;
    logic rst = 1'b1;
    logic led;
    int   errors = 0;

    blinky #(.WIDTH(WIDTH)) dut (.clk(clk), .rst(rst), .led(led));

    always #10 clk = ~clk;   // 50 MHz

    initial begin
        string wave;
        if (!$value$plusargs("wave=%s", wave)) wave = "tb_blinky.fst";
        $dumpfile(wave);
        $dumpvars(0, tb_blinky);
    end

    task automatic expect_led(input logic value, input string when);
        if (led !== value) begin
            $display("FAIL: led=%b, expected %b %s", led, value, when);
            errors++;
        end
    endtask

    initial begin
        repeat (2) @(posedge clk);
        #1 rst = 1'b0;   // step off the clock edge to avoid a race with the DUT
        expect_led(1'b0, "just after reset");
        repeat (8)  @(posedge clk);
        #1 expect_led(1'b1, "after 8 clocks");
        repeat (8)  @(posedge clk);
        #1 expect_led(1'b0, "after 16 clocks");
        if (errors == 0) $display("PASS: tb_blinky");
        else             $display("FAIL: tb_blinky, %0d error(s)", errors);
        $finish;
    end
endmodule
