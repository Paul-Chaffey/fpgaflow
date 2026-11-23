// Drives usb_probe with a model of the FX2's GPIF waveforms as decoded from
// Celoxica's firmware (waveform 0 = FIFO write, waveform 1 = FIFO read):
// outputs change just after the IFCLK rising edge, the FX2 samples D on it.
`timescale 1ns/1ps
module tb_usb_probe;
    logic clk = 0;
    always #10.4167 clk = ~clk;

    logic ncs = 1, nwr = 1;
    logic [7:0] d_drv;
    logic d_oe = 0;
    wire  [7:0] d;
    assign d = d_oe ? d_drv : 8'bz;
    logic full, empty;
    logic [4:0] joy = '0;
    logic [7:0] led, seg0, seg1;

    usb_probe dut (.clk, .fx2_ncs(ncs), .fx2_nwr(nwr), .fx2_d(d), .fx2_full(full),
                   .fx2_empty(empty), .joy, .led, .seg0, .seg1);

    int errors = 0;
    // Both sides driving D at once would be contention.
    always @(posedge clk) if (d_oe && dut.oe) begin
        $display("FAIL: FPGA drives D during a write"); errors++;
    end

    task automatic step(input logic c, input logic w);
        @(posedge clk); #1 ncs = c; nwr = w;
    endtask

    // Waveform 0 with INTRDY set: S3 (nWR low), S4/S5 one byte per cycle
    // with nCS and nWR low, S6/S7 idle.
    task automatic gpif_write(input int n, input int seed);
        step(1, 0);
        for (int i = 0; i < n; i++) begin
            @(posedge clk); #1 ncs = 0; nwr = 0; d_oe = 1; d_drv = 8'(seed + 7 * i);
        end
        step(1, 1); d_oe = 0;
        repeat (8) @(posedge clk);
    endtask

    // Waveform 1: S1 3 cycles + S2 1 cycle with nCS low, then S3 samples D
    // every cycle; S6 samples once more with nCS high.
    logic [7:0] r[$];
    task automatic gpif_read(input int n);
        r = {};
        for (int i = 0; i < 4; i++) step(0, 1);
        for (int i = 0; i < n - 1; i++) begin
            @(posedge clk); r.push_back(d); #1 ncs = 0;
        end
        @(posedge clk); r.push_back(d); #1 ncs = 1;    // S6
        @(posedge clk); r.push_back(d);
        repeat (8) @(posedge clk);
    endtask

    int k, wc;
    logic [7:0] sum, x;
    initial begin
        repeat (5) @(posedge clk);
        gpif_write(100, 3);
        gpif_write(28, 200);
        gpif_read(512);
        // The counter part: consecutive values, no X, no gaps.
        for (int i = 1; i < 100; i++)
            if (r[i] !== r[i-1] + 8'd1) begin
                $display("FAIL: read byte %0d = %h after %h", i, r[i], r[i-1]); errors++;
            end
        // Find the stats block (A5 .. 5A C3) and check the write counts.
        begin
            k = -1;
            for (int i = 0; i + 15 < r.size(); i++)
                if (r[i] == 8'hA5 && r[i+14] == 8'h5A && r[i+15] == 8'hC3) begin k = i; break; end
            if (k < 0) begin $display("FAIL: no stats block"); errors++; end
            else begin
                wc = {r[k+3], r[k+2], r[k+1]};
                sum = 0; x = 0;
                for (int i = 0; i < 100; i++) begin sum += 8'(3 + 7*i); x ^= 8'(3 + 7*i); end
                for (int i = 0; i < 28; i++)  begin sum += 8'(200 + 7*i); x ^= 8'(200 + 7*i); end
                $display("first read byte %h, stats at %0d: %0d bytes, sum %h xor %h first %h last %h, %0d write bursts, last %0d cycles",
                         r[0], k, wc, r[k+4], r[k+5], r[k+6], r[k+7], {r[k+11], r[k+10]}, {r[k+13], r[k+12]});
                if (wc != 128 || r[k+4] != sum || r[k+5] != x || r[k+6] != 8'd3
                    || r[k+7] != 8'(200 + 7*27) || {r[k+11], r[k+10]} != 2 || {r[k+13], r[k+12]} != 28) begin
                    $display("FAIL: stats"); errors++;
                end
            end
        end
        // 128 = 0x80 written: display "80".
        if (seg0 !== 8'h7F || seg1 !== 8'h3F) begin $display("FAIL: display %h %h", seg0, seg1); errors++; end
        if (errors == 0) $display("PASS");
        else $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
