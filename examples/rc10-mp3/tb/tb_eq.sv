`timescale 1ns/1ps

// The bass boost (rtl/eq.sv) against crates/mp3model/src/eq.rs: every line
// of build/eq-vectors.txt (`mp3model eq-vectors build/eq-vectors.txt`) is
// one stereo sample with its rate and level; both outputs must match.
// Run with `fpgaflow -C examples/rc10-mp3 sim --tb tb_eq`.
module tb_eq;
    localparam string FILE = "../../eq-vectors.txt";
    logic clk = 0;
    always #11.3379 clk = ~clk;            // 44.1 MHz, the DAC clock

    logic start = 0, done;
    logic [1:0] rate = '0, level = '0;
    logic signed [15:0] in_l = '0, in_r = '0, out_l, out_r;
    eq dut (.clk, .start, .rate, .level, .in_l, .in_r, .out_l, .out_r, .done);

    integer fd, n = 0, errors = 0, cyc, worst = 0;
    integer r, lv, il, ir, wl, wr;
    initial begin
        fd = $fopen(FILE, "r");
        if (fd == 0) begin
            $display("FAIL: no %s; run mp3model eq-vectors build/eq-vectors.txt", FILE);
            $finish;
        end
        repeat (3) @(posedge clk);
        while ($fscanf(fd, "%d %d %d %d %d %d\n", r, lv, il, ir, wl, wr) == 6) begin
            @(posedge clk); #1;
            rate = r[1:0]; level = lv[1:0]; in_l = il[15:0]; in_r = ir[15:0];
            start = 1;
            @(posedge clk); #1 start = 0;
            cyc = 1;
            while (!done && cyc < 200) begin @(posedge clk); #1 cyc = cyc + 1; end
            if (cyc > worst) worst = cyc;
            if (!done) begin
                $display("FAIL: sample %0d never finished", n);
                errors = errors + 1;
            end else if (out_l != wl[15:0] || out_r != wr[15:0]) begin
                if (errors < 10)
                    $display("FAIL: sample %0d (rate %0d level %0d, in %0d %0d): out %0d %0d, model %0d %0d",
                             n, r, lv, il, ir, out_l, out_r, wl, wr);
                errors = errors + 1;
            end
            n = n + 1;
        end
        if (errors == 0 && n > 0)
            $display("PASS: %0d stereo samples (3 rates x 4 levels) match the model; %0d clocks per sample", n, worst);
        else
            $display("FAIL: %0d of %0d samples differ", errors, n);
        $finish;
    end
endmodule
