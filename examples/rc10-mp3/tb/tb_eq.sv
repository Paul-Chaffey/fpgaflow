`timescale 1ns/1ps

// Tone control (rtl/eq.sv) against crates/mp3model/src/eq.rs (`Equalizer`):
// build/eq-vectors.txt (`mp3model eq-vectors build/eq-vectors.txt`) holds
//   S rate level in_l in_r out_l out_r   one stereo sample; both outputs must match
//   L snaps l0 .. l9 r0 .. r9            the analyser's snapshot after that sample
//   C on c0 .. c49                       from the next sample: EQ on/off and a set
// A set is written into the bank not in use on the write clock (as the USB
// side does), then the banks swap. Run with
// `fpgaflow -C examples/rc10-mp3 sim --tb tb_eq`.
module tb_eq;
    localparam string FILE = "../../eq-vectors.txt";
    logic clk = 0, wclk = 0;
    always #11.3379 clk = ~clk;            // 44.1 MHz, the DAC clock
    always #10.4167 wclk = ~wclk;          // 48 MHz, the USB side

    logic start = 0, done, busy, on = 0, bank = 0;
    logic [319:0] levels;
    logic [7:0] snaps;
    integer nsnap = 0, ws, wv;
    logic [1:0] rate = '0, level = '0;
    logic signed [15:0] in_l = '0, in_r = '0, out_l, out_r;
    logic cw_en = 0;
    logic [6:0] cw_addr = '0;
    logic signed [24:0] cw_data = '0;
    eq dut (.clk, .start, .rate, .level, .on, .bank, .in_l, .in_r, .out_l, .out_r, .done,
            .busy, .levels, .snaps,
            .wclk, .cw_en, .cw_addr, .cw_data);

    integer fd, n = 0, sets = 0, errors = 0, cyc, worst = 0, worst_on = 0, t, v, onv;
    integer r, lv, il, ir, wl, wr;
    initial begin
        fd = $fopen(FILE, "r");
        if (fd == 0) begin
            $display("FAIL: no %s; run mp3model eq-vectors build/eq-vectors.txt", FILE);
            $finish;
        end
        repeat (3) @(posedge clk);
        while (1) begin
            t = $fgetc(fd);
            if (t == -1) break;
            if (t == "C") begin
                void'($fscanf(fd, "%d", onv));
                if (onv != 0) begin
                    for (int i = 0; i < 50; i++) begin
                        void'($fscanf(fd, "%d", v));
                        @(posedge wclk); #1;
                        cw_en = 1; cw_addr = {~bank, 6'(i)}; cw_data = v[24:0];
                    end
                    @(posedge wclk); #1 cw_en = 0;
                    bank = ~bank;
                end else begin
                    for (int i = 0; i < 50; i++) void'($fscanf(fd, "%d", v));
                end
                on = onv != 0;
                sets = sets + 1;
                void'($fgetc(fd));          // newline
            end else if (t == "S") begin
                void'($fscanf(fd, "%d %d %d %d %d %d\n", r, lv, il, ir, wl, wr));
                @(posedge clk); #1;
                rate = r[1:0]; level = lv[1:0]; in_l = il[15:0]; in_r = ir[15:0];
                start = 1;
                @(posedge clk); #1 start = 0;
                cyc = 1;
                while (!done && cyc < 1000) begin @(posedge clk); #1 cyc = cyc + 1; end
                if (on && cyc > worst_on) worst_on = cyc;
                if (!on && cyc > worst) worst = cyc;
                if (!done) begin
                    $display("FAIL: sample %0d never finished", n);
                    errors = errors + 1;
                end else if (out_l != wl[15:0] || out_r != wr[15:0]) begin
                    if (errors < 10)
                        $display("FAIL: sample %0d (rate %0d level %0d eq %0d, in %0d %0d): out %0d %0d, model %0d %0d",
                                 n, r, lv, on, il, ir, out_l, out_r, wl, wr);
                    errors = errors + 1;
                end
                // The analyser finishes before the next sample (as on the board).
                cyc = 0;
                while (busy && cyc < 1000) begin @(posedge clk); #1 cyc = cyc + 1; end
                n = n + 1;
            end else if (t == "L") begin
                void'($fscanf(fd, "%d", ws));
                if (snaps != ws[7:0]) begin
                    if (errors < 10) $display("FAIL: after sample %0d: snapshot %0d, model %0d", n - 1, snaps, ws);
                    errors = errors + 1;
                end
                for (int b = 0; b < 20; b++) begin   // left 0-9, right 10-19
                    void'($fscanf(fd, "%d", wv));
                    if (levels[16 * b +: 16] != wv[15:0]) begin
                        if (errors < 10) $display("FAIL: after sample %0d: band %0d level %0d, model %0d",
                                                  n - 1, b, levels[16 * b +: 16], wv);
                        errors = errors + 1;
                    end
                end
                nsnap = nsnap + 1;
                void'($fgetc(fd));
            end
        end
        if (errors == 0 && n > 0)
            $display("PASS: %0d stereo samples, %0d settings changes, %0d level snapshots match the model; %0d clocks to the outputs (EQ off), %0d (on)",
                     n, sets, nsnap, worst, worst_on);
        else
            $display("FAIL: %0d of %0d samples differ", errors, n);
        $finish;
    end
endmodule
