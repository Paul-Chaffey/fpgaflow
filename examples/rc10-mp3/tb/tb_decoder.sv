`timescale 1ns/1ps

// The decoder so far (mp3_frontend + mp3_granule) against mp3model: every
// stream in build/vectors/list.txt (made by tools/make-vectors.sh) is fed in
// with random gaps, honouring `full` with the FX2's measured 3-byte overrun.
// Every frame record (70 bytes) and every granule record (61 scale factors
// and 576 Huffman values) must equal the model's, and the counts and CRCs
// its summary.
module tb_decoder;
    localparam string DIR = "../../vectors/";
    logic clk = 0;
    always #10.4167 clk = ~clk;

    logic in_valid = 0, new_stream = 0, idle = 1;
    logic [7:0] in_data = '0;
    logic full, rec_valid;
    logic [7:0] rec_data;
    logic [31:0] frames, frame_bytes, crc, last_header;
    logic [15:0] undecodable;
    logic [12:0] fill;
    logic [7:0] res_rdata;
    logic [11:0] res_raddr;
    logic [357:0] side;
    logic fd_valid, fd_ready, g_valid;
    logic [7:0] g_data;
    logic [31:0] granules, gcrc;

    mp3_frontend dut (.clk, .in_valid, .in_data, .new_stream, .idle, .full,
                      .rec_valid, .rec_data, .frames, .frame_bytes, .undecodable,
                      .crc, .last_header, .fill, .res_raddr, .res_rdata,
                      .side, .fd_valid, .fd_ready);
    mp3_granule gdut (.clk, .new_stream, .side, .fd_valid, .fd_ready, .res_raddr, .res_rdata,
                      .rec_valid(g_valid), .rec_data(g_data), .granules, .crc(gcrc));

    integer errors = 0;
    // Watchdog: where things are, every 100k cycles (on when +watch is given).
    integer wd = 0, shown_pair = 0, shown_fd = 0;
    always @(posedge clk) if (fd_valid && shown_fd < 1 && $test$plusargs("watch")) begin
        shown_fd = shown_fd + 1;
        $display("  [fd] side %h", side);
        $display("  [fd] p23[0] %h entry0 %h hdr %h", dut.p23[0], side[73:0], side[357:326]);
        $fflush();
    end
    always @(posedge clk) if (gdut.st == 3 && shown_pair < 2 && $test$plusargs("watch")) begin
        shown_pair = shown_pair + 1;
        $display("  [pair] i %0d big %0d bv %0d tbl %0d empty %b reg1 %0d reg2 %0d ts %0d %0d %0d ws %b gr %b ch %b mono %b",
                 gdut.i, gdut.big, gdut.bv, gdut.tbl, gdut.tbl_empty, gdut.reg1, gdut.reg2,
                 gdut.ts0, gdut.ts1, gdut.ts2, gdut.ws, gdut.gr, gdut.ch, gdut.mono);
        $display("  [pair] ent %h", gdut.ent);
        $fflush();
    end
    always @(posedge clk) begin
        wd = wd + 1;
        if (wd % 100000 == 0 && $test$plusargs("watch")) begin
            $display("  [watch %0d] frames %0d granules %0d fe.st %0d gr.st %0d bcnt %0d inf %0d pos %0d i %0d full %b",
                     wd, frames, granules, dut.st, gdut.st, gdut.bcnt, gdut.inf, gdut.pos, gdut.i, full);
            $fflush();
        end
    end
    // Granule records: 61 scale factor bytes, then 576 values (16-bit LE).
    integer gfd, gn = 0, gb = 0, want_v, gerr_shown = 0;
    reg [7:0] lo;
    always @(posedge clk) if (g_valid) begin
        if (gb < 61) begin
            void'($fscanf(gfd, "%d", want_v));
            if (g_data !== want_v[7:0]) begin
                if (gerr_shown < 5) $display("FAIL: granule %0d scale factor %0d = %0d, model %0d", gn, gb, g_data, want_v);
                gerr_shown = gerr_shown + 1;
                errors = errors + 1;
            end
        end else if ((gb - 61) % 2 == 0) begin
            lo = g_data;
        end else begin
            void'($fscanf(gfd, "%d", want_v));
            if ($signed({g_data, lo}) != want_v) begin
                if (gerr_shown < 5) $display("FAIL: granule %0d value %0d = %0d, model %0d", gn, (gb - 61) / 2, $signed({g_data, lo}), want_v);
                gerr_shown = gerr_shown + 1;
                errors = errors + 1;
            end
        end
        gb = gb + 1;
        if (gb == 61 + 1152) begin
            gb = 0;
            gn = gn + 1;
            if (gn % 50 == 0) $display("  ... %0d granules at %0t", gn, $time);
        end
    end

    // FULL as the FX2 sees it: it may still send 3 bytes.
    logic [2:0] full_hist = '0;
    always @(posedge clk) full_hist <= {full_hist[1:0], full};

    // Records as hex text, compared line by line with the model.
    function automatic [15:0] hex2(input [7:0] b);
        hex2 = {hexc(b[7:4]), hexc(b[3:0])};
    endfunction
    function automatic [7:0] hexc(input [3:0] v);
        hexc = v < 10 ? 8'd48 + v : 8'd87 + v;
    endfunction

    integer total_frames = 0, total_grans = 0, mfd, nrec, nbyte;
    reg [8*140-1:0] got_line;
    reg [8*141-1:0] want_line;
    always @(posedge clk) if (rec_valid) begin
        got_line = {got_line[8*138-1:0], hex2(rec_data)};
        nbyte = nbyte + 1;
        if (nbyte == 70) begin
            nbyte = 0;
            nrec = nrec + 1;
            if ($fgets(want_line, mfd) == 0) begin
                if (errors < 10) $display("FAIL: record %0d not in the model", nrec);
                errors = errors + 1;
            end else if (want_line[8*141-1:8] !== got_line) begin
                if (errors < 10) $display("FAIL: record %0d\n  rtl   %s\n  model %s", nrec, got_line, want_line[8*141-1:8]);
                errors = errors + 1;
            end
        end
    end

    integer lfd, fd, c, sent, gaps, want_frames, want_grans, cyc;
    reg [8*64-1:0] name;
    reg [31:0] want_crc, want_gcrc;
    string path;
    initial begin
        lfd = $fopen({DIR, "list.txt"}, "r");
        if (lfd == 0) begin
            $display("FAIL: no %slist.txt; run tools/make-vectors.sh", DIR);
            $finish;
        end
        repeat (4) @(posedge clk);
        while ($fscanf(lfd, "%s\n", name) == 1) begin
            path = $sformatf("%0s", name);
            fd  = $fopen({DIR, path, ".mp3"}, "rb");
            mfd = $fopen({DIR, path, ".model.txt"}, "r");
            gfd = $fopen({DIR, path, ".gran.txt"}, "r");
            gn = 0; gb = 0; gerr_shown = 0;
            c   = $fopen({DIR, path, ".sum"}, "r");
            void'($fscanf(c, "%d %h %d %h", want_frames, want_crc, want_grans, want_gcrc));
            $fclose(c);
            nrec = 0; nbyte = 0; sent = 0;
            idle = 0;
            c = $fgetc(fd);
            while (c != -1) begin
                @(posedge clk);
                #1;
                in_valid = 0;
                new_stream = 0;
                // Random gaps, and none while the FX2 would have seen FULL.
                if (!full_hist[2] && ($urandom % 8 != 0)) begin
                    in_valid = 1;
                    in_data = c[7:0];
                    new_stream = sent == 0;
                    sent = sent + 1;
                    c = $fgetc(fd);
                end
            end
            @(posedge clk); #1 in_valid = 0; new_stream = 0;
            $fclose(fd);
            // The stream ends: let the parser drain, then raise idle.
            cyc = 0;
            while (frames < want_frames && cyc < 400000) begin @(posedge clk); cyc = cyc + 1; end
            idle = 1;
            cyc = 0;
            while (granules < want_grans && cyc < 400000) begin @(posedge clk); cyc = cyc + 1; end
            repeat (20000) @(posedge clk);
            if (frames !== want_frames || crc !== want_crc || nrec != want_frames
                || granules !== want_grans || gcrc !== want_gcrc) begin
                $display("FAIL: %0s: %0d frames (model %0d), CRC %h (model %h); %0d granules (model %0d), CRC %h (model %h)",
                         path, frames, want_frames, crc, want_crc, granules, want_grans, gcrc, want_gcrc);
                errors = errors + 1;
            end else begin
                $display("  %0s: %0d frames, %0d granules, CRCs %h %h: match the model", path, frames, granules, crc, gcrc);
            end
            $fclose(gfd);
            total_frames = total_frames + frames;
            total_grans = total_grans + granules;
            $fclose(mfd);
        end
        if (errors == 0) $display("PASS: %0d frames and %0d granules in all streams match the model", total_frames, total_grans);
        else $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
