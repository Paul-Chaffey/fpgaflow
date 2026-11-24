`timescale 1ns/1ps

// The whole decoder (front end to PCM) against mp3model: every
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

    // The stream as the host sends it: records, through the demux (stage 2
    // of the EQ plan). Settings records go to the demux's outputs only.
    logic mp3_valid, cw_en, eq_on, eq_bank, set_bass, set_pause, pause_val, set_vol;
    logic [3:0] vol_val;
    logic [7:0] mp3_data, eq_sets, settings;
    logic [6:0] cw_addr;
    logic [24:0] cw_data;
    logic [1:0] bass_val;
    stream_demux dmx (.clk, .new_stream, .rx_valid(in_valid), .rx_data(in_data), .mp3_valid, .mp3_data,
                      .cw_en, .cw_addr, .cw_data, .eq_on, .eq_bank, .set_bass, .bass_val,
                      .set_pause, .pause_val, .set_vol, .vol_val, .eq_sets, .settings);
    mp3_frontend dut (.clk, .in_valid(mp3_valid), .in_data(mp3_data), .new_stream, .idle, .full,
                      .rec_valid, .rec_data, .frames, .frame_bytes, .undecodable,
                      .crc, .last_header, .fill, .res_raddr, .res_rdata,
                      .side, .fd_valid, .fd_ready);
    logic s_ready, s_valid, rec_ch;
    logic [7:0] s_data;
    logic [31:0] rec_hdr, spectra, scrc;
    logic [73:0] rec_ent;
    mp3_granule gdut (.clk, .new_stream, .side, .fd_valid, .fd_ready, .res_raddr, .res_rdata,
                      .rec_valid(g_valid), .rec_data(g_data), .rec_ready(s_ready),
                      .rec_hdr, .rec_ent, .rec_ch, .granules, .crc(gcrc));
    logic i_ready, b_valid, spec_ch;
    logic [7:0] b_data;
    logic [73:0] spec_ent;
    logic [31:0] subs, bcrc;
    mp3_spectral sdut (.clk, .new_stream, .in_valid(g_valid), .in_data(g_data), .in_hdr(rec_hdr),
                       .in_ent(rec_ent), .in_ch(rec_ch), .ready(s_ready),
                       .spec_valid(s_valid), .spec_data(s_data), .spec_ready(i_ready),
                       .spec_ent, .spec_ch, .spec_mono, .spec_rate, .spectra, .crc(scrc));
    logic spec_mono, y_ready, sub_ch, sub_mono, pcm_valid;
    logic [1:0] spec_rate, sub_rate, pcm_rate;
    logic [31:0] pcm_word, nsamp, pcrc;
    mp3_imdct idut (.clk, .new_stream, .in_valid(s_valid), .in_data(s_data), .in_ent(spec_ent),
                    .in_ch(spec_ch), .in_mono(spec_mono), .in_rate(spec_rate), .ready(i_ready), .sub_valid(b_valid),
                    .sub_data(b_data), .sub_ready(y_ready), .sub_ch, .sub_mono, .sub_rate,
                    .subbands(subs), .crc(bcrc));
    // The FIFO's room: mostly there, sometimes not, to exercise the wait.
    logic out_room = 1'b1;
    always @(posedge clk) out_room <= ($urandom % 16) != 0;
    mp3_synth ydut (.clk, .new_stream, .in_valid(b_valid), .in_data(b_data), .in_ch(sub_ch),
                    .in_mono(sub_mono), .in_rate(sub_rate), .ready(y_ready), .pcm_valid, .pcm_word, .pcm_rate, .out_room,
                    .samples(nsamp), .crc(pcrc));

    // PCM (stereo words {right, left}; mono has left = right), against .pcm.txt.
    integer pfd, pn = 0, perr_shown = 0, want_p;
    always @(posedge clk) if (pcm_valid) begin
        void'($fscanf(pfd, "%d", want_p));
        if ($signed(pcm_word[15:0]) != want_p) begin
            if (perr_shown < 8) $display("FAIL: PCM sample %0d = %0d, model %0d", pn, $signed(pcm_word[15:0]), want_p);
            perr_shown = perr_shown + 1;
            errors = errors + 1;
        end
        pn = pn + 1;
        if (!ydut.mono) begin
            void'($fscanf(pfd, "%d", want_p));
            if ($signed(pcm_word[31:16]) != want_p) begin
                if (perr_shown < 8) $display("FAIL: PCM sample %0d = %0d, model %0d", pn, $signed(pcm_word[31:16]), want_p);
                perr_shown = perr_shown + 1;
                errors = errors + 1;
            end
            pn = pn + 1;
        end
    end

    // Subband samples (IMDCT output), against the model's .sub.txt.
    integer bfd, bn = 0, bb = 0, berr_shown = 0, want_b;
    reg [31:0] bw;
    always @(posedge clk) if (b_valid) begin
        bw = {b_data, bw[31:8]};
        bb = bb + 1;
        if (bb % 4 == 0) begin
            void'($fscanf(bfd, "%d", want_b));
            if ($signed(bw) != want_b) begin
                if (berr_shown < 8) $display("FAIL: subbands %0d value %0d = %0d, model %0d", bn, bb / 4 - 1, $signed(bw), want_b);
                berr_shown = berr_shown + 1;
                errors = errors + 1;
            end
        end
        if (bb == 2304) begin
            bb = 0;
            bn = bn + 1;
        end
    end

    // Spectra: 576 values as 32-bit LE each, against the model's .spec.txt.
    integer sfd, sn = 0, sb_ = 0, serr_shown = 0, want_s;
    reg [31:0] sw;
    always @(posedge clk) if (s_valid) begin
        sw = {s_data, sw[31:8]};
        sb_ = sb_ + 1;
        if (sb_ % 4 == 0) begin
            void'($fscanf(sfd, "%d", want_s));
            if ($signed(sw) != want_s) begin
                if (serr_shown < 8) $display("FAIL: spectrum %0d value %0d = %0d, model %0d", sn, sb_ / 4 - 1, $signed(sw), want_s);
                serr_shown = serr_shown + 1;
                errors = errors + 1;
            end
        end
        if (sb_ == 2304) begin
            sb_ = 0;
            sn = sn + 1;
        end
    end

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

    integer lfd, fd, c, sent, gaps, want_frames, want_grans, want_spec, want_sub, want_pcm, cyc;
    integer total_spec = 0, total_sub = 0, total_pcm = 0;
    reg [8*64-1:0] name;
    reg [31:0] want_crc, want_gcrc, want_scrc, want_bcrc, want_pcrc;
    logic [7:0] mp3q[$], fq[$];
    integer pos, len, kind, qi, want_sets, want_settings, sets0, settings0;
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
            sfd = $fopen({DIR, path, ".spec.txt"}, "r");
            bfd = $fopen({DIR, path, ".sub.txt"}, "r");
            pfd = $fopen({DIR, path, ".pcm.txt"}, "r");
            pn = 0; perr_shown = 0;
            gn = 0; gb = 0; gerr_shown = 0; sn = 0; sb_ = 0; serr_shown = 0; bn = 0; bb = 0; berr_shown = 0;
            c   = $fopen({DIR, path, ".sum"}, "r");
            void'($fscanf(c, "%d %h %d %h %d %h %d %h %d %h", want_frames, want_crc, want_grans, want_gcrc,
                          want_spec, want_scrc, want_sub, want_bcrc, want_pcm, want_pcrc));
            $fclose(c);
            nrec = 0; nbyte = 0; sent = 0;
            sets0 = eq_sets; settings0 = settings;
            idle = 0;
            // Frame it: MP3 records of random length, with settings records
            // (EQ sets, bass, pause, EQ off) slipped in between at random.
            mp3q.delete();
            c = $fgetc(fd);
            while (c != -1) begin mp3q.push_back(c[7:0]); c = $fgetc(fd); end
            fq.delete();
            want_sets = 0; want_settings = 0;
            pos = 0;
            while (pos < mp3q.size()) begin
                if ($urandom % 6 == 0) begin
                    kind = $urandom % 5;
                    if (kind == 0) begin                       // EQ set: 200 bytes
                        fq.push_back(8'd1); fq.push_back(8'd0); fq.push_back(8'd200);
                        repeat (200) fq.push_back(8'($urandom));
                        want_sets = want_sets + 1;
                    end else if (kind == 1) begin              // bass
                        fq.push_back(8'd2); fq.push_back(8'd0); fq.push_back(8'd1);
                        fq.push_back(8'($urandom % 4));
                        want_settings = want_settings + 1;
                    end else if (kind == 2) begin              // pause / play
                        fq.push_back(8'd3); fq.push_back(8'd0); fq.push_back(8'd1);
                        fq.push_back(8'($urandom % 2));
                        want_settings = want_settings + 1;
                    end else if (kind == 3) begin              // EQ off
                        fq.push_back(8'd4); fq.push_back(8'd0); fq.push_back(8'd0);
                        want_settings = want_settings + 1;
                    end else begin                             // volume
                        fq.push_back(8'd5); fq.push_back(8'd0); fq.push_back(8'd1);
                        fq.push_back(8'($urandom % 16));
                        want_settings = want_settings + 1;
                    end
                end
                len = 1 + $urandom % 3000;
                if (len > mp3q.size() - pos) len = mp3q.size() - pos;
                fq.push_back(8'd0); fq.push_back(8'(len >> 8)); fq.push_back(8'(len));
                for (int k = 0; k < len; k++) fq.push_back(mp3q[pos + k]);
                pos = pos + len;
            end
            qi = 0;
            while (qi < fq.size()) begin
                @(posedge clk);
                #1;
                in_valid = 0;
                new_stream = 0;
                // Random gaps, and none while the FX2 would have seen FULL.
                if (!full_hist[2] && ($urandom % 8 != 0)) begin
                    in_valid = 1;
                    in_data = fq[qi];
                    new_stream = sent == 0;
                    sent = sent + 1;
                    qi = qi + 1;
                end
            end
            @(posedge clk); #1 in_valid = 0; new_stream = 0;
            $fclose(fd);
            // The stream ends: let the parser drain, then raise idle.
            cyc = 0;
            while (frames < want_frames && cyc < 400000) begin @(posedge clk); cyc = cyc + 1; end
            idle = 1;
            cyc = 0;
            while ((granules < want_grans || spectra < want_spec || subs < want_sub || nsamp < want_pcm) && cyc < 8000000) begin @(posedge clk); cyc = cyc + 1; end
            repeat (20000) @(posedge clk);
            if (frames !== want_frames || crc !== want_crc || nrec != want_frames
                || granules !== want_grans || gcrc !== want_gcrc || spectra !== want_spec || scrc !== want_scrc
                || subs !== want_sub || bcrc !== want_bcrc || nsamp !== want_pcm || pcrc !== want_pcrc
                || eq_sets !== 8'(sets0 + want_sets) || settings !== 8'(settings0 + want_settings)) begin
                $display("FAIL:   demux took %0d EQ sets (sent %0d), %0d settings (sent %0d)",
                         8'(eq_sets - sets0), want_sets, 8'(settings - settings0), want_settings);
                $display("FAIL: %0s: %0d frames (model %0d), CRC %h (model %h); %0d granules (model %0d), CRC %h (model %h); %0d spectra (model %0d), CRC %h (model %h)",
                         path, frames, want_frames, crc, want_crc, granules, want_grans, gcrc, want_gcrc,
                         spectra, want_spec, scrc, want_scrc);
                $display("FAIL:   subbands %0d (model %0d), CRC %h (model %h); %0d samples (model %0d), CRC %h (model %h)",
                         subs, want_sub, bcrc, want_bcrc, nsamp, want_pcm, pcrc, want_pcrc);
                errors = errors + 1;
            end else begin
                $display("  %0s: %0d frames, %0d granules, %0d spectra, %0d subbands, %0d samples, CRCs %h %h %h %h %h: match the model",
                         path, frames, granules, spectra, subs, nsamp, crc, gcrc, scrc, bcrc, pcrc);
            end
            total_spec = total_spec + spectra;
            total_sub = total_sub + subs;
            total_pcm = total_pcm + nsamp;
            $fclose(pfd);
            $fclose(bfd);
            $fclose(gfd);
            $fclose(sfd);
            total_frames = total_frames + frames;
            total_grans = total_grans + granules;
            $fclose(mfd);
        end
        if (errors == 0) $display("PASS: %0d frames, %0d granules, %0d spectra, %0d subband sets and %0d PCM samples in all streams match the model",
                                  total_frames, total_grans, total_spec, total_sub, total_pcm);
        else $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
