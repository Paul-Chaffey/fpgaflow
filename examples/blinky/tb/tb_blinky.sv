`timescale 1ns/1ps

// Self-checking testbench for blinky: how long between flashes.
//
// The LED is the counter's MSB, so for a WIDTH-bit counter:
//   - first rising edge comes 2^(WIDTH-1) clocks after reset is released
//   - rising edge to rising edge (flash period) is 2^WIDTH clocks
//   - LED on-time (rise to fall) is 2^(WIDTH-1) clocks (50% duty)
//
// The real WIDTH=25 would take 2^25 clocks per flash (~0.67 s at 50 MHz), far
// too long for simulation, so two small widths are checked instead. With two
// widths the test also confirms that the period scales with WIDTH.
//
// Timing is measured in clock cycles. The LED is sampled 1 ns after each rising
// clock edge so the sample never races the DUT's register update.
module tb_blinky;
    localparam int unsigned NDUT      = 2;
    localparam int unsigned FLASHES   = 4;      // flash periods measured per DUT
    localparam real         CLK_NS    = 20.0;   // 50 MHz

    logic clk = 1'b0;
    logic rst = 1'b1;
    int   errors   = 0;
    int   finished = 0;

    always #(CLK_NS/2) clk = ~clk;

    initial begin
        string wave;
        if (!$value$plusargs("wave=%s", wave)) wave = "tb_blinky.fst";
        $dumpfile(wave);
        $dumpvars(0, tb_blinky);
    end

    genvar g;
    generate
        for (g = 0; g < NDUT; g++) begin : g_dut
            localparam int unsigned W      = (g == 0) ? 4 : 7;
            localparam int unsigned PERIOD = 1 << W;        // clocks per flash
            localparam int unsigned HALF   = 1 << (W - 1);  // clocks LED is on
            localparam int unsigned MAXCYC = (FLASHES + 1) * PERIOD;

            logic        led;
            logic        prev;
            int unsigned cyc;
            int unsigned last_rise;
            int unsigned rises;
            int unsigned falls;

            blinky #(.WIDTH(W)) dut (.clk(clk), .rst(rst), .led(led));

            initial begin
                cyc       = 0;
                last_rise = 0;
                rises     = 0;
                falls     = 0;

                @(negedge rst);
                if (led !== 1'b0) begin
                    $display("FAIL: WIDTH=%0d led=%b at reset release, expected 0", W, led);
                    errors++;
                end
                prev = 1'b0;

                // Exactly enough cycles for FLASHES+1 rises and FLASHES+1 falls.
                while (cyc < MAXCYC) begin
                    @(posedge clk);
                    #1;
                    cyc++;

                    if (led !== 1'b0 && led !== 1'b1) begin
                        $display("FAIL: WIDTH=%0d led=%b (not 0/1) at cycle %0d", W, led, cyc);
                        errors++;
                    end else if (led && !prev) begin
                        // Rising edge: start of a flash.
                        if (rises == 0) begin
                            if (cyc != HALF) begin
                                $display("FAIL: WIDTH=%0d first flash %0d clocks after reset, expected %0d",
                                         W, cyc, HALF);
                                errors++;
                            end
                        end else if (cyc - last_rise != PERIOD) begin
                            $display("FAIL: WIDTH=%0d flash %0d came %0d clocks (%0.1f ns) after the previous one, expected %0d (%0.1f ns)",
                                     W, rises, cyc - last_rise, (cyc - last_rise) * CLK_NS,
                                     PERIOD, PERIOD * CLK_NS);
                            errors++;
                        end
                        last_rise = cyc;
                        rises++;
                    end else if (!led && prev) begin
                        // Falling edge: end of a flash.
                        if (cyc - last_rise != HALF) begin
                            $display("FAIL: WIDTH=%0d LED on for %0d clocks, expected %0d",
                                     W, cyc - last_rise, HALF);
                            errors++;
                        end
                        falls++;
                    end
                    prev = led;
                end

                if (rises != FLASHES + 1 || falls != FLASHES + 1) begin
                    $display("FAIL: WIDTH=%0d saw %0d rises and %0d falls in %0d clocks, expected %0d of each",
                             W, rises, falls, MAXCYC, FLASHES + 1);
                    errors++;
                end else begin
                    $display("INFO: WIDTH=%0d flash every %0d clocks (%0.1f ns), on for %0d clocks",
                             W, PERIOD, PERIOD * CLK_NS, HALF);
                end
                finished++;
            end
        end
    endgenerate

    // Stimulus: hold reset for two clocks, release it off the clock edge.
    initial begin
        repeat (2) @(posedge clk);
        #1 rst = 1'b0;

        wait (finished == NDUT);
        if (errors == 0) $display("PASS: tb_blinky");
        else             $display("FAIL: tb_blinky, %0d error(s)", errors);
        $finish;
    end

    // Watchdog: the longest DUT needs (FLASHES+1)*2^7 = 640 clocks (~13 us).
    initial begin
        #100_000;
        $display("FAIL: tb_blinky watchdog timeout (finished %0d of %0d DUTs)", finished, NDUT);
        $finish;
    end
endmodule
