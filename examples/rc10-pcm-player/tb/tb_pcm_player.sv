`timescale 1ns/1ps

// pcm_player against a model of the FX2's GPIF (waveforms decoded from
// Celoxica's firmware, timing as measured with rc10-usb-probe). Small FIFO and
// fast sample rate so FULL is exercised constantly.
module tb_pcm_player;
    localparam int AW = 6, MARGIN = 16, TICK = 20, IDLE_BITS = 8;
    localparam int LAT = 3;          // cycles before the FX2 reacts to FULL
    localparam int WORDS = 3000;

    logic clk = 0;
    always #10.4167 clk = ~clk;

    logic ncs = 1, nwr = 1, seln = 0;
    logic [7:0] d_drv;
    logic d_oe = 0;
    wire  [7:0] d;
    assign d = d_oe ? d_drv : 8'bz;
    logic full, empty, audio_l, audio_r;
    logic [1:0] piezo;
    logic [4:0] joy = '0;
    logic [7:0] led, seg0, seg1;

    pcm_player #(.TICK(TICK), .AW(AW), .MARGIN(MARGIN), .IDLE_BITS(IDLE_BITS), .MS(4)) dut (
        .clk, .fx2_ncs(ncs), .fx2_nwr(nwr), .fx2_seln(seln), .fx2_d(d), .fx2_full(full),
        .fx2_empty(empty), .joy, .led, .seg0, .seg1, .audio_l, .audio_r, .piezo);

    int errors = 0;
    always @(posedge clk) if (d_oe && dut.u_bus.oe) begin
        $display("FAIL: FPGA drives D during a write"); errors++;
    end

    // FULL as the FX2 sees it, LAT cycles late.
    logic [LAT:0] full_hist = '0;
    always @(posedge clk) full_hist <= {full_hist[LAT-1:0], full};
    wire full_seen = full_hist[LAT];

    // Host data, sent in 512-byte packets.
    logic [7:0] tx[$];
    logic [31:0] sent[$];

    // Waveform 0: start only if FULL is low; then one byte per cycle until
    // the packet is done or FULL is seen (that cycle's byte still goes).
    task automatic fx2_send_all();
        int pos = 0;
        while (pos < tx.size()) begin
            @(posedge clk);
            if (full_seen) continue;              // firmware waits for RDY1 low
            #1 nwr = 0;                           // S3
            @(posedge clk);
            if (full_seen) begin #1 nwr = 1; repeat (8) @(posedge clk); continue; end
            do begin
                #1 ncs = 0; d_oe = 1; d_drv = tx[pos]; pos++;
                @(posedge clk);
            end while (pos % 512 != 0 && pos < tx.size() && !full_seen);
            #1 ncs = 1; nwr = 1; d_oe = 0;        // S6, S7
            repeat (8 + 20) @(posedge clk);       // and the firmware's turnaround
        end
    endtask

    // Waveform 1 (as measured): 4 cycles with nCS low, then one sample per cycle.
    logic [7:0] r[$];
    task automatic fx2_read(input int n);
        r = {};
        @(posedge clk); #1 ncs = 0; nwr = 1;
        repeat (3) @(posedge clk);
        for (int i = 0; i < n; i++) begin
            @(posedge clk); r.push_back(d);
            if (i == n - 2) #1 ncs = 1;
        end
        repeat (8) @(posedge clk);
    endtask

    // Samples as played.
    logic [31:0] played[$];
    always @(posedge clk) if (dut.re) played.push_back(dut.rdata);

    int k, ones, n;
    initial begin
        #50ms $display("FAIL: timeout (played %0d)", played.size()); $finish;
    end
    logic [31:0] w, last;
    logic [7:0] flags;
    real x, want, got;
    initial begin
        repeat (5) @(posedge clk);
        // Two stray bytes, then silence: the next stream must start aligned.
        tx = '{8'hEE, 8'hEE}; fx2_send_all();
        repeat ((1 << IDLE_BITS) + 50) @(posedge clk);
        tx = {};
        for (int i = 0; i < WORDS; i++) begin
            w = $urandom;
            sent.push_back(w);
            for (int b = 0; b < 4; b++) tx.push_back(w[8*b +: 8]);
        end
        fx2_send_all();
        // Let it play out.
        for (int c = 0; c < WORDS * TICK * 2 && played.size() < WORDS; c++) @(posedge clk);
        repeat (TICK * 4) @(posedge clk);

        if (played.size() != WORDS) begin
            $display("FAIL: played %0d of %0d samples", played.size(), WORDS); errors++;
        end
        k = 0;
        foreach (sent[i]) if (i < played.size() && played[i] !== sent[i]) begin
            if (k < 5) $display("FAIL: sample %0d played %h, sent %h", i, played[i], sent[i]);
            k++; errors++;
        end

        fx2_read(16);
        flags = r[15];
        $display("status: %s%s bytes %0d underruns %0d missed %0d dropped %0d late max %0d fill %0d flags %h",
                 string'(r[0]), string'(r[1]), {r[5], r[4], r[3], r[2]}, {r[7], r[6]}, {r[9], r[8]},
                 {r[11], r[10]}, {r[13], r[12]}, r[14], r[15]);
        if (r[0] != "P" || r[1] != "1" || {r[5], r[4], r[3], r[2]} != 4 * WORDS
            || {r[11], r[10]} != 0 || r[14] != 0 || flags[5] != 1'b1) begin
            $display("FAIL: status"); errors++;
        end
        if ({r[13], r[12]} == 0) begin $display("FAIL: FULL was never exercised"); errors++; end

        // The DAC: density of ones = 1/2 + x/2^16 with x = last left sample * gain/256.
        begin
            ones = 0; n = 65536;
            last = sent[WORDS-1];
            x = real'($signed(last[15:0])) * 82.0 / 256.0;
            want = 0.5 + x / 65536.0;
            repeat (n) begin @(posedge clk); ones += audio_l; end
            got = real'(ones) / n;
            $display("DAC: density %.4f, expected %.4f", got, want);
            if (got - want > 0.002 || want - got > 0.002) begin $display("FAIL: DAC density"); errors++; end
        end

        if (errors == 0) $display("PASS");
        else $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
