`timescale 1ns/1ps

// MP3 front end (docs/mp3-decoder-plan.md, milestone 1): MPEG-1 Layer III
// frames from a byte stream, with their side information and the bit
// reservoir. Matches crates/mp3model/src/frontend.rs record for record.
//
// - Bytes go into a 2^AW-byte ring buffer; `full` asks the sender to pause
//   when fewer than MARGIN bytes are free.
// - A header counts as a frame only when the next header (same sample rate)
//   follows where its length says, or the stream ends there (`idle`).
//   Otherwise the search moves on one byte.
// - Side info (32 bytes read; 17 used for mono) is parsed one bit a cycle.
// - Main data is appended to the reservoir, a 4096-byte ring; each frame's
//   main data starts main_data_begin bytes before its own.
// - Each frame gives a 70-byte record (layout in frontend.rs), sent one
//   byte a cycle on rec_*, with a running CRC-32 over all records.
// A new stream (`new_stream`: first byte after an idle gap) starts afresh.
module mp3_frontend #(
    parameter int AW     = 12,
    parameter int MARGIN = 512
) (
    input  logic        clk,
    input  logic        in_valid,
    input  logic [7:0]  in_data,
    input  logic        new_stream,
    input  logic        idle,
    output logic        full = 1'b0,
    // Records
    output logic        rec_valid = 1'b0,
    output logic [7:0]  rec_data = '0,
    // Totals for this stream
    output logic [31:0] frames = '0,
    output logic [31:0] frame_bytes = '0, // bytes inside accepted frames
    output logic [15:0] undecodable = '0,
    output logic [31:0] crc,             // CRC-32 of all records so far (final form)
    output logic [31:0] last_header = '0,
    output logic [AW:0] fill,
    // Reservoir read port (for the granule decoder)
    input  logic [11:0] res_raddr,
    output logic [7:0]  res_rdata
);
    // ------------------------------------------------------------ helpers
    function automatic logic hdr_ok(input logic [31:0] h);
        hdr_ok = h[31:24] == 8'hFF && h[23:21] == 3'b111 && h[20:19] == 2'b11   // MPEG-1
              && h[18:17] == 2'b01                                             // Layer III
              && h[15:12] != 4'd0 && h[15:12] != 4'd15 && h[11:10] != 2'd3;
    endfunction

    // Frame length without padding: 144000 * bitrate / sample rate.
    function automatic logic [10:0] frame_len(input logic [1:0] sr, input logic [3:0] br);
        case ({sr, br})
            // 44100 Hz
            {2'd0, 4'd1}: frame_len = 11'd104;
            {2'd0, 4'd2}: frame_len = 11'd130;
            {2'd0, 4'd3}: frame_len = 11'd156;
            {2'd0, 4'd4}: frame_len = 11'd182;
            {2'd0, 4'd5}: frame_len = 11'd208;
            {2'd0, 4'd6}: frame_len = 11'd261;
            {2'd0, 4'd7}: frame_len = 11'd313;
            {2'd0, 4'd8}: frame_len = 11'd365;
            {2'd0, 4'd9}: frame_len = 11'd417;
            {2'd0, 4'd10}: frame_len = 11'd522;
            {2'd0, 4'd11}: frame_len = 11'd626;
            {2'd0, 4'd12}: frame_len = 11'd731;
            {2'd0, 4'd13}: frame_len = 11'd835;
            {2'd0, 4'd14}: frame_len = 11'd1044;
            // 48000 Hz
            {2'd1, 4'd1}: frame_len = 11'd96;
            {2'd1, 4'd2}: frame_len = 11'd120;
            {2'd1, 4'd3}: frame_len = 11'd144;
            {2'd1, 4'd4}: frame_len = 11'd168;
            {2'd1, 4'd5}: frame_len = 11'd192;
            {2'd1, 4'd6}: frame_len = 11'd240;
            {2'd1, 4'd7}: frame_len = 11'd288;
            {2'd1, 4'd8}: frame_len = 11'd336;
            {2'd1, 4'd9}: frame_len = 11'd384;
            {2'd1, 4'd10}: frame_len = 11'd480;
            {2'd1, 4'd11}: frame_len = 11'd576;
            {2'd1, 4'd12}: frame_len = 11'd672;
            {2'd1, 4'd13}: frame_len = 11'd768;
            {2'd1, 4'd14}: frame_len = 11'd960;
            // 32000 Hz
            {2'd2, 4'd1}: frame_len = 11'd144;
            {2'd2, 4'd2}: frame_len = 11'd180;
            {2'd2, 4'd3}: frame_len = 11'd216;
            {2'd2, 4'd4}: frame_len = 11'd252;
            {2'd2, 4'd5}: frame_len = 11'd288;
            {2'd2, 4'd6}: frame_len = 11'd360;
            {2'd2, 4'd7}: frame_len = 11'd432;
            {2'd2, 4'd8}: frame_len = 11'd504;
            {2'd2, 4'd9}: frame_len = 11'd576;
            {2'd2, 4'd10}: frame_len = 11'd720;
            {2'd2, 4'd11}: frame_len = 11'd864;
            {2'd2, 4'd12}: frame_len = 11'd1008;
            {2'd2, 4'd13}: frame_len = 11'd1152;
            {2'd2, 4'd14}: frame_len = 11'd1440;
            default: frame_len = 11'd0;
        endcase
    endfunction

    function automatic logic [31:0] crc_byte(input logic [31:0] c, input logic [7:0] d);
        logic [31:0] x;
        x = c ^ {24'd0, d};
        for (int k = 0; k < 8; k++) x = x[0] ? (x >> 1) ^ 32'hEDB88320 : x >> 1;
        crc_byte = x;
    endfunction

    // ------------------------------------------------------------ input ring
    logic [7:0]  mem [0:(1 << AW) - 1];
    logic [AW:0] wp = '0, rp = '0;
    logic [AW-1:0] raddr = '0;
    logic [7:0]  rdata;
    always_ff @(posedge clk) begin
        if (in_valid) mem[wp[AW-1:0]] <= in_data;
        rdata <= mem[raddr];
    end
    always_ff @(posedge clk) if (in_valid) wp <= wp + 1'b1;
    wire [AW:0] avail = wp - rp;
    assign fill = avail;
    always_ff @(posedge clk) full <= ((AW+1)'(1 << AW) - avail) < (AW+1)'(MARGIN);

    // ------------------------------------------------------------ reservoir
    logic [7:0]  res_mem [0:4095];
    logic [11:0] res_wp = '0;
    logic        res_we = 1'b0;
    logic [11:0] res_waddr = '0;
    logic [7:0]  res_wdata = '0;
    always_ff @(posedge clk) begin
        if (res_we) res_mem[res_waddr] <= res_wdata;
        res_rdata <= res_mem[res_raddr];
    end

    // ------------------------------------------------------------ parser
    typedef enum logic [3:0] {
        S_WAIT_HDR, S_HDR, S_CHECK, S_WAIT_NEXT, S_NEXT, S_NEXT_CHECK,
        S_SIDE, S_PARSE, S_COPY, S_REC
    } state_t;
    state_t st = S_WAIT_HDR;

    logic [31:0]  hdr = '0, nh = '0;
    logic [10:0]  len = '0;
    wire  [10:0]  len_now = frame_len(hdr[11:10], hdr[15:12]) + 11'(hdr[9]);
    wire          mono = hdr[7:6] == 2'b11;
    wire  [5:0]   silen = mono ? 6'd17 : 6'd32;
    wire  [2:0]   side_at = hdr[16] ? 3'd4 : 3'd6;   // protection bit 0 = CRC present
    wire  [10:0]  md_len_now = len - 11'(side_at) - 11'(silen);

    // Byte fetch: `start_n` bytes from `start_at`, one a cycle. Each byte is
    // on `rdata` while `got_d` is high (`got_last_d` with the last one).
    logic         start_fetch, side_go = 1'b0, copy_go = 1'b0;
    logic         fetching = 1'b0, got = 1'b0, got_last = 1'b0, got_d = 1'b0, got_last_d = 1'b0;
    logic [AW:0]  start_at;
    logic [10:0]  start_n;
    always_comb begin
        start_fetch = 1'b0;
        start_at = rp;
        start_n = 11'd4;
        if (side_go) begin
            start_fetch = 1'b1;
            start_at = rp + (AW+1)'(side_at);
            start_n = 11'd32;
        end else if (copy_go) begin
            start_fetch = 1'b1;
            start_at = rp + (AW+1)'(side_at) + (AW+1)'(silen);
            start_n = md_len_now;
        end else if (st == S_WAIT_HDR) begin
            start_fetch = avail >= 4 && !fetching;
        end else if (st == S_WAIT_NEXT) begin
            start_fetch = avail >= (AW+1)'(len) + 4 && !fetching;
            start_at = rp + (AW+1)'(len);
        end
    end

    logic [AW:0]  fetch_at = '0;
    logic [10:0]  fetch_n = '0, fetch_i = '0;
    always_ff @(posedge clk) begin
        got <= 1'b0;
        got_last <= 1'b0;
        if (fetching) begin
            raddr <= fetch_at[AW-1:0] + AW'(fetch_i);
            got <= 1'b1;
            got_last <= fetch_i == fetch_n - 1'b1;
            fetch_i <= fetch_i + 1'b1;
            if (fetch_i == fetch_n - 1'b1) fetching <= 1'b0;
        end
        if (start_fetch && start_n != 0) begin
            fetching <= 1'b1;
            fetch_i <= '0;
            fetch_at <= start_at;
            fetch_n <= start_n;
        end
        // raddr is registered, then the RAM: data arrives one cycle after `got`.
        got_d <= got;
        got_last_d <= got_last;
        if (new_stream) begin
            fetching <= 1'b0;
            got <= 1'b0;
            got_d <= 1'b0;
        end
    end

    // Side info fields: [entry] with entry = 2 * granule + channel.
    logic [255:0] sireg = '0;
    logic [8:0]   mdb = '0;
    logic [7:0]   scfsi = '0;
    logic [11:0]  p23 [4];
    logic [8:0]   bv [4];
    logic [7:0]   gg [4];
    logic [3:0]   sfc [4];
    logic         ws [4], mix [4], pre [4], scs [4], c1 [4];
    logic [1:0]   bt [4];
    logic [4:0]   ts0 [4], ts1 [4], ts2 [4];
    logic [2:0]   sbg0 [4], sbg1 [4], sbg2 [4];
    logic [3:0]   r0 [4];
    logic [5:0]   r1 [4];

    typedef enum logic [4:0] {
        F_MDB, F_PRIV, F_SCFSI, F_P23, F_BV, F_GG, F_SFC, F_WS, F_BT, F_MIX,
        F_TS0, F_TS1, F_TS2, F_SBG0, F_SBG1, F_SBG2, F_R0, F_R1, F_PRE, F_SCS, F_C1
    } field_t;
    field_t fld = F_MDB;
    logic [4:0]  bitn = '0;          // bits of this field read so far
    logic [11:0] acc = '0;
    logic        gr = 1'b0, ch = 1'b0;
    wire  [1:0]  e = {gr, ch};

    function automatic logic [4:0] width(input field_t f, input logic m);
        case (f)
            F_MDB: width = 9;   F_PRIV: width = m ? 5 : 3;   F_SCFSI: width = m ? 4 : 8;
            F_P23: width = 12;  F_BV: width = 9;   F_GG: width = 8;   F_SFC: width = 4;
            F_BT: width = 2;    F_TS0, F_TS1, F_TS2: width = 5;
            F_SBG0, F_SBG1, F_SBG2: width = 3;     F_R0: width = 4;   F_R1: width = 3;
            default: width = 1;   // F_WS, F_MIX, F_PRE, F_SCS, F_C1
        endcase
    endfunction

    wire        bit_in = sireg[255];
    wire [11:0] val_now = {acc[10:0], bit_in};
    wire        field_done = bitn == width(fld, mono) - 1'b1;
    // Two cycles a bit: shift in and decide (phase 0), then store (phase 1).
    logic        ph = 1'b0, done_r = 1'b0;
    logic [11:0] val = '0;

    logic [10:0] md_len = '0;
    logic [12:0] res_total = '0;     // main-data bytes so far, saturating
    logic [11:0] md_start = '0;
    logic        decodable = 1'b0;

    // Record out.
    logic [559:0] rec = '0;
    logic [6:0]   rec_left = '0;
    logic [31:0]  crc_r = 32'hFFFFFFFF;
    assign crc = ~crc_r;

    always_ff @(posedge clk) begin
        rec_valid <= 1'b0;
        res_we <= 1'b0;
        side_go <= 1'b0;
        copy_go <= 1'b0;
        if (rec_left != 0) begin
            rec_valid <= 1'b1;
            rec_data <= rec[559:552];
            crc_r <= crc_byte(crc_r, rec[559:552]);
            rec <= {rec[551:0], 8'h00};
            rec_left <= rec_left - 1'b1;
        end
        case (st)
            S_WAIT_HDR:
                if (start_fetch) st <= S_HDR;
            S_HDR:
                if (got_d) begin
                    hdr <= {hdr[23:0], rdata};
                    if (got_last_d) st <= S_CHECK;
                end
            S_CHECK:
                if (hdr_ok(hdr)) begin
                    len <= len_now;
                    st <= S_WAIT_NEXT;
                end else begin
                    rp <= rp + 1'b1;
                    st <= S_WAIT_HDR;
                end
            S_WAIT_NEXT:
                if (start_fetch) begin
                    st <= S_NEXT;
                end else if (idle && !fetching) begin
                    // End of the stream: the frame stands if it is all here.
                    if (avail >= (AW+1)'(len)) begin
                        side_go <= 1'b1;
                        st <= S_SIDE;
                    end else begin
                        rp <= rp + 1'b1;
                        st <= S_WAIT_HDR;
                    end
                end
            S_NEXT:
                if (got_d) begin
                    nh <= {nh[23:0], rdata};
                    if (got_last_d) st <= S_NEXT_CHECK;
                end
            S_NEXT_CHECK:
                if (hdr_ok(nh) && nh[11:10] == hdr[11:10]) begin
                    side_go <= 1'b1;
                    st <= S_SIDE;
                end else begin
                    rp <= rp + 1'b1;
                    st <= S_WAIT_HDR;
                end
            S_SIDE:
                if (got_d) begin
                    sireg <= {sireg[247:0], rdata};
                    if (got_last_d) begin
                        st <= S_PARSE;
                        ph <= 1'b0;
                        fld <= F_MDB;
                        bitn <= '0;
                        acc <= '0;
                        gr <= 1'b0;
                        ch <= 1'b0;
                        scfsi <= '0;
                        for (int i = 0; i < 4; i++) begin
                            p23[i] <= '0; bv[i] <= '0; gg[i] <= '0; sfc[i] <= '0; ws[i] <= 1'b0;
                            mix[i] <= 1'b0; pre[i] <= 1'b0; scs[i] <= 1'b0; c1[i] <= 1'b0;
                            bt[i] <= '0; ts0[i] <= '0; ts1[i] <= '0; ts2[i] <= '0;
                            sbg0[i] <= '0; sbg1[i] <= '0; sbg2[i] <= '0; r0[i] <= '0; r1[i] <= '0;
                        end
                    end
                end
            S_PARSE: if (!ph) begin
                sireg <= {sireg[254:0], 1'b0};
                acc <= val_now;
                val <= val_now;
                bitn <= bitn + 1'b1;
                done_r <= field_done;
                ph <= 1'b1;
            end else begin
                ph <= 1'b0;
                if (done_r) begin
                    bitn <= '0;
                    acc <= '0;
                    case (fld)
                        F_MDB:   begin mdb <= val[8:0]; fld <= F_PRIV; end
                        F_PRIV:  fld <= F_SCFSI;
                        F_SCFSI: begin scfsi <= mono ? {val[3:0], 4'b0} : val[7:0]; fld <= F_P23; end
                        F_P23:   begin p23[e] <= val; fld <= F_BV; end
                        F_BV:    begin bv[e] <= val[8:0]; fld <= F_GG; end
                        F_GG:    begin gg[e] <= val[7:0]; fld <= F_SFC; end
                        F_SFC:   begin sfc[e] <= val[3:0]; fld <= F_WS; end
                        F_WS:    begin ws[e] <= val[0]; if (val[0]) fld <= F_BT; else fld <= F_TS0; end
                        F_BT:    begin bt[e] <= val[1:0]; fld <= F_MIX; end
                        F_MIX:   begin mix[e] <= val[0]; fld <= F_TS0; end
                        F_TS0:   begin ts0[e] <= val[4:0]; fld <= F_TS1; end
                        F_TS1:   begin ts1[e] <= val[4:0]; if (ws[e]) fld <= F_SBG0; else fld <= F_TS2; end
                        F_TS2:   begin ts2[e] <= val[4:0]; fld <= F_R0; end
                        F_SBG0:  begin sbg0[e] <= val[2:0]; fld <= F_SBG1; end
                        F_SBG1:  begin sbg1[e] <= val[2:0]; fld <= F_SBG2; end
                        F_SBG2:  begin
                            sbg2[e] <= val[2:0];
                            // Implicit region counts with window switching.
                            r0[e] <= (bt[e] == 2'd2 && !mix[e]) ? 4'd8 : 4'd7;
                            r1[e] <= 6'd36;
                            fld <= F_PRE;
                        end
                        F_R0:    begin r0[e] <= val[3:0]; fld <= F_R1; end
                        F_R1:    begin r1[e] <= {3'b0, val[2:0]}; fld <= F_PRE; end
                        F_PRE:   begin pre[e] <= val[0]; fld <= F_SCS; end
                        F_SCS:   begin scs[e] <= val[0]; fld <= F_C1; end
                        default: begin   // F_C1: next channel or granule, or done
                            c1[e] <= val[0];
                            fld <= F_P23;
                            if (!mono && !ch) begin
                                ch <= 1'b1;
                            end else begin
                                ch <= 1'b0;
                                if (!gr) begin
                                    gr <= 1'b1;
                                end else begin
                                    md_len <= md_len_now;
                                    md_start <= res_wp - 12'(mdb);
                                    decodable <= 13'(mdb) <= res_total;
                                    copy_go <= md_len_now != 0;
                                    if (md_len_now != 0) st <= S_COPY; else st <= S_REC;
                                end
                            end
                        end
                    endcase
                end
            end
            S_COPY:
                if (got_d) begin
                    res_we <= 1'b1;
                    res_waddr <= res_wp;
                    res_wdata <= rdata;
                    res_wp <= res_wp + 1'b1;
                    if (got_last_d) st <= S_REC;
                end
            S_REC: begin
                rec <= record();
                rec_left <= 7'd70;
                frames <= frames + 1'b1;
                frame_bytes <= frame_bytes + 32'(len);
                if (!decodable) undecodable <= undecodable + 1'b1;
                last_header <= hdr;
                res_total <= (res_total + 13'(md_len) > 13'd4095) ? 13'd4095 : res_total + 13'(md_len);
                rp <= rp + (AW+1)'(len);
                st <= S_WAIT_HDR;
            end
            default: st <= S_WAIT_HDR;
        endcase
        if (new_stream) begin
            st <= S_WAIT_HDR;
            rp <= wp;
            res_wp <= '0;
            res_total <= '0;
            frames <= '0;
            frame_bytes <= '0;
            undecodable <= '0;
            crc_r <= 32'hFFFFFFFF;
            rec_left <= '0;
        end
    end

    // ------------------------------------------------------------ record
    function automatic logic [559:0] record();
        logic [559:0] r;
        r = '0;
        r[559 -: 32] = hdr;
        r[527 -: 16] = 16'(mdb);
        r[511 -: 8]  = scfsi;
        for (int i = 0; i < 4; i++) begin
            r[503 - 120 * i -: 16] = 16'(p23[i]);
            r[487 - 120 * i -: 16] = 16'(bv[i]);
            r[471 - 120 * i -: 8]  = gg[i];
            r[463 - 120 * i -: 8]  = 8'(sfc[i]);
            r[455 - 120 * i -: 8]  = {ws[i], mix[i], bt[i], 1'b0, pre[i], scs[i], c1[i]};
            r[447 - 120 * i -: 8]  = 8'(ts0[i]);
            r[439 - 120 * i -: 8]  = 8'(ts1[i]);
            r[431 - 120 * i -: 8]  = 8'(ts2[i]);
            r[423 - 120 * i -: 16] = {7'd0, sbg0[i], sbg1[i], sbg2[i]};
            r[407 - 120 * i -: 8]  = 8'(r0[i]);
            r[399 - 120 * i -: 8]  = 8'(r1[i]);
        end
        r[23 -: 16] = 16'(md_start);
        r[7 -: 8]   = 8'(decodable);
        record = r;
    endfunction
endmodule
