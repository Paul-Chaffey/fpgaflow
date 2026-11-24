`timescale 1ns/1ps

// MP3 polyphase synthesis filterbank (docs/mp3-decoder-plan.md, milestone 5).
// Bit-exact with FixedBackend::synth in crates/mp3model/src/fixed.rs.
//
// Takes a granule's channels from mp3_imdct (576 subband samples each,
// [18 sb + t], 32-bit LE), then for t = 0..17 and each channel:
//   V shifts by 64 (a ring: the offset moves back 64);
//   V[i] = sat26(round17(sum_k N[i][k] s[k])), i = 0..63, s[k] = sub[18 k + t],
//          N[i][k] = c[(16 + i)(2k + 1) mod 128] (mp3_cos128);
//   pcm[j] = clamp16(round23(sum_q V[128 q + j] D[64 q + j] + V[128 q + 96 + j] D[64 q + 32 + j])).
// After each t the 32 samples go out as stereo words {right, left} (mono:
// left = right) when `out_room` says the FIFO has space for 32. The CRC-32
// covers the samples in the model's order (left, right; or the one channel).
module mp3_synth (
    input  logic        clk,
    input  logic        new_stream,
    input  logic        in_valid,
    input  logic [7:0]  in_data,
    input  logic        in_ch,
    input  logic        in_mono,
    input  logic [1:0]  in_rate,
    output logic        ready,
    output logic        pcm_valid,
    output logic [31:0] pcm_word,     // {right, left}
    output logic [1:0]  pcm_rate,     // its sampling-rate index (0 44.1, 1 48, 2 32 kHz)
    input  logic        out_room,     // the FIFO can take 32 words
    output logic [31:0] samples,      // PCM values (per channel) so far
    output logic [31:0] crc
);
    function automatic logic signed [25:0] sat26(input logic signed [49:0] v);
        sat26 = v > 50'sd33554431 ? 26'sd33554431 : v < -50'sd33554432 ? -26'sd33554432 : v[25:0];
    endfunction
    function automatic logic signed [15:0] sat16(input logic signed [49:0] v);
        sat16 = v > 50'sd32767 ? 16'sd32767 : v < -50'sd32768 ? -16'sd32768 : v[15:0];
    endfunction
    function automatic logic [31:0] crc_byte(input logic [31:0] c, input logic [7:0] d);
        logic [31:0] x;
        x = c ^ {24'd0, d};
        for (int j = 0; j < 8; j++) x = x[0] ? (x >> 1) ^ 32'hEDB88320 : x >> 1;
        crc_byte = x;
    endfunction

    // ------------------------------------------------------------ RAMs
    // Subband samples of both channels: {channel, 18 sb + t}, 27 bits.
    logic [26:0] sub [0:2047];
    logic        s_we = 1'b0;
    logic [10:0] s_waddr = '0, s_raddr = '0;
    logic [26:0] s_wdata = '0, s_rd;
    always_ff @(posedge clk) begin
        if (s_we) sub[s_waddr] <= s_wdata;
        s_rd <= sub[s_raddr];
    end
    // V rings: {channel, physical index}, 26 bits stored in 27.
    logic [26:0] vmem [0:2047];
    logic        v_we = 1'b0;
    logic [10:0] v_waddr = '0, v_raddr = '0;
    logic [26:0] v_wdata = '0, v_rd;
    always_ff @(posedge clk) begin
        if (v_we) vmem[v_waddr] <= v_wdata;
        v_rd <= vmem[v_raddr];
    end
    // One time slot's samples per channel (LUT RAM).
    logic [15:0] obuf [0:63];             // {channel, j}
    logic        o_we = 1'b0;
    logic [5:0]  o_waddr = '0;
    logic [15:0] o_wdata = '0;
    always_ff @(posedge clk) if (o_we) obuf[o_waddr] <= o_wdata;

    // ------------------------------------------------------------ coefficients
    logic [6:0]  m = '0;
    logic signed [17:0] cosv, dval;
    mp3_cos128 u_cos (.m, .c(cosv));
    logic [8:0]  d_addr = '0;
    mp3_synth_d u_d (.clk, .addr(d_addr), .data(dval));

    // ------------------------------------------------------------ MAC
    // Terms issue one a cycle; the data (and D) arrive two cycles later, the
    // cosine is delayed to match; product one cycle after, then the sum.
    logic        win_phase = 1'b0;        // 0: matrixing (sub x cos), 1: window (V x D)
    logic        issuing = 1'b0;
    logic [4:0]  kk = '0, klast = '0;     // term, last term
    logic [3:0]  vp = '0, fp = '0, lp = '0;
    logic signed [17:0] c1 = '0, c2 = '0;
    logic signed [44:0] prod = '0;
    logic signed [49:0] acc = '0;
    logic        acc_done = 1'b0;
    wire  signed [26:0] mac_a = win_phase ? $signed(v_rd) : $signed(s_rd);
    wire  signed [17:0] mac_b = win_phase ? dval : c2;

    // ------------------------------------------------------------ control
    typedef enum logic [3:0] {
        Y_CLR, Y_IN, Y_SLOT, Y_MOUT, Y_MAC, Y_VW, Y_WOUT, Y_PW, Y_EMIT, Y_EMIT1
    } ystate_t;
    ystate_t st = Y_CLR;

    logic [11:0] nin = '0;
    logic [23:0] part = '0;
    logic        mono = 1'b0;
    logic [4:0]  t = '0;                  // time slot
    logic        c = 1'b0;                // channel
    logic [9:0]  voff [0:1];
    logic [5:0]  i = '0;                  // V index (matrixing) / j (window)
    logic [11:0] clr = '0;
    logic [31:0] crc_r = 32'hFFFFFFFF;
    logic [1:0]  eb = '0;                 // byte of the sample(s) for the CRC
    assign crc = ~crc_r;
    assign ready = st == Y_IN && nin == 0;
    initial samples = '0;
    initial pcm_rate = '0;

    // The term being issued: matrixing k = kk: data sub[c][18 k + t], m = (16+i)(2k+1)
    // mod 128 (stepped by 2(16+i)); window q = kk >> 1, half = kk[0].
    wire [9:0]  vlog = 10'(128 * kk[4:1]) + (kk[0] ? 10'd96 : 10'd0) + 10'(i);

    always_ff @(posedge clk) begin
        s_we <= 1'b0;
        v_we <= 1'b0;
        o_we <= 1'b0;
        pcm_valid <= 1'b0;
        acc_done <= 1'b0;
        // MAC pipeline.
        vp <= {vp[2:0], issuing};
        fp <= {fp[2:0], issuing && kk == 0};
        lp <= {lp[2:0], issuing && kk == klast};
        c1 <= cosv;
        c2 <= c1;
        if (vp[1]) prod <= mac_a * mac_b;
        if (vp[2]) acc <= (fp[2] ? 50'sd0 : acc) + 50'(prod);
        if (vp[2] && lp[2]) acc_done <= 1'b1;
        case (st)
            // ---- after reset or a new stream: clear the V rings
            Y_CLR: begin
                v_we <= 1'b1;
                v_waddr <= clr[10:0];
                v_wdata <= '0;
                clr <= clr + 1'b1;
                voff[0] <= '0;
                voff[1] <= '0;
                if (clr == 12'd2047) st <= Y_IN;
            end
            // ---- a channel's 576 subband samples
            Y_IN:
                if (in_valid) begin
                    if (nin == 0) begin
                        mono <= in_mono;
                        pcm_rate <= in_rate;
                    end
                    if (nin[1:0] != 2'd3) begin
                        part <= {in_data, part[23:8]};
                    end else begin
                        s_we <= 1'b1;
                        s_waddr <= {in_ch, 10'(nin >> 2)};
                        s_wdata <= {in_data[2:0], part};
                    end
                    nin <= nin + 1'b1;
                    if (nin == 12'd2303) begin
                        nin <= '0;
                        if (in_mono || in_ch) begin
                            t <= '0;
                            c <= 1'b0;
                            st <= Y_SLOT;
                        end
                    end
                end
            // ---- a time slot of channel c: shift V, then the 64 new values
            Y_SLOT: begin
                voff[c] <= voff[c] - 10'd64;
                i <= '0;
                win_phase <= 1'b0;
                st <= Y_MOUT;
            end
            Y_MOUT: begin                     // start V[i]: 32 terms
                kk <= '0;
                klast <= 5'd31;               // 32 terms
                m <= 7'(16 + i);              // (16 + i) * 1
                issuing <= 1'b1;
                st <= Y_MAC;
            end
            Y_MAC: begin
                if (issuing) begin
                    if (!win_phase) begin
                        s_raddr <= {c, 10'(18 * kk) + 10'(t)};
                        m <= m + 7'(2 * (16 + i));   // next k
                    end else begin
                        v_raddr <= {c, voff[c] + vlog};
                        d_addr <= 9'(64 * kk[4:1]) + (kk[0] ? 9'd32 : 9'd0) + 9'(i);
                    end
                    kk <= kk + 1'b1;
                    if (kk == klast) issuing <= 1'b0;
                end
                if (acc_done) st <= win_phase ? Y_PW : Y_VW;
            end
            Y_VW: begin                       // V[i] = sat26(round17(acc))
                logic signed [49:0] r;
                r = (acc + 50'sd65536) >>> 17;
                v_we <= 1'b1;
                v_waddr <= {c, voff[c] + 10'(i)};
                v_wdata <= 27'(sat26(r));      // (sign-extended)
                if (i == 6'd63) begin
                    i <= '0;
                    win_phase <= 1'b1;
                    st <= Y_WOUT;
                end else begin
                    i <= i + 1'b1;
                    st <= Y_MOUT;
                end
            end
            Y_WOUT: begin                     // start sample j = i: 16 terms
                kk <= '0;
                klast <= 5'd15;               // 16 terms
                issuing <= 1'b1;
                st <= Y_MAC;
            end
            Y_PW: begin                       // pcm[j] = clamp16(round23(acc))
                logic signed [49:0] r;
                r = (acc + 50'sd4194304) >>> 23;
                o_we <= 1'b1;
                o_waddr <= {c, i[4:0]};
                o_wdata <= sat16(r);
                if (i == 6'd31) begin
                    if (!mono && !c) begin
                        c <= 1'b1;
                        st <= Y_SLOT;
                    end else begin
                        c <= 1'b0;
                        i <= '0;
                        st <= Y_EMIT;
                    end
                end else begin
                    i <= i + 1'b1;
                    st <= Y_WOUT;
                end
            end
            // ---- the slot's 32 samples to the FIFO (when it has room)
            Y_EMIT:
                if (i != 0 || out_room) begin
                    eb <= '0;
                    st <= Y_EMIT1;
                end
            Y_EMIT1: begin
                // CRC over the model's order: left (2 bytes), then right for stereo.
                logic [15:0] l, r16;
                l = obuf[{1'b0, i[4:0]}];
                r16 = mono ? l : obuf[{1'b1, i[4:0]}];
                crc_r <= crc_byte(crc_r, eb == 0 ? l[7:0] : eb == 1 ? l[15:8] : eb == 2 ? r16[7:0] : r16[15:8]);
                eb <= eb + 1'b1;
                if (eb == (mono ? 2'd1 : 2'd3)) begin
                    pcm_valid <= 1'b1;
                    pcm_word <= {r16, l};
                    samples <= samples + (mono ? 32'd1 : 32'd2);
                    if (i == 6'd31) begin
                        i <= '0;
                        if (t == 5'd17) begin
                            st <= Y_IN;
                        end else begin
                            t <= t + 1'b1;
                            st <= Y_SLOT;
                        end
                    end else begin
                        i <= i + 1'b1;
                        st <= Y_EMIT;
                    end
                end
            end
            default: st <= Y_IN;
        endcase
        if (new_stream) begin
            st <= Y_CLR;
            clr <= '0;
            nin <= '0;
            issuing <= 1'b0;
            samples <= '0;
            crc_r <= 32'hFFFFFFFF;
        end
    end
endmodule
