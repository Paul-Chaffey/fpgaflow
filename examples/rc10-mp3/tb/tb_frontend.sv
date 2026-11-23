`timescale 1ns/1ps

// mp3_frontend against mp3model: every stream in build/vectors/list.txt
// (made by tools/make-vectors.sh) is fed in with random gaps, honouring
// `full` with the FX2's measured 3-byte overrun; every 70-byte record must
// equal the model's line, and the frame count and CRC its summary.
module tb_frontend;
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

    mp3_frontend dut (.clk, .in_valid, .in_data, .new_stream, .idle, .full,
                      .rec_valid, .rec_data, .frames, .frame_bytes, .undecodable,
                      .crc, .last_header, .fill, .res_raddr(12'd0), .res_rdata);

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

    integer errors = 0, total_frames = 0, mfd, nrec, nbyte;
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

    integer lfd, fd, c, sent, gaps, want_frames, cyc;
    reg [8*64-1:0] name;
    reg [31:0] want_crc;
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
            c   = $fopen({DIR, path, ".sum"}, "r");
            void'($fscanf(c, "%d %h", want_frames, want_crc));
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
            repeat (20000) @(posedge clk);
            if (frames !== want_frames || crc !== want_crc || nrec != want_frames) begin
                $display("FAIL: %0s: %0d frames (model %0d), CRC %h (model %h)", path, frames, want_frames, crc, want_crc);
                errors = errors + 1;
            end else begin
                $display("  %0s: %0d bytes, %0d frames, CRC %h, %0d undecodable: matches the model", path, sent, frames, crc, undecodable);
            end
            total_frames = total_frames + frames;
            $fclose(mfd);
        end
        if (errors == 0) $display("PASS: %0d frames in all streams match the model", total_frames);
        else $display("FAIL: %0d errors", errors);
        $finish;
    end
endmodule
