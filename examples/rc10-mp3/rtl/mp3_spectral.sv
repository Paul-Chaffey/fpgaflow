`timescale 1ns/1ps

// MP3 spectral stages (docs/mp3-decoder-plan.md, milestone 3): requantise,
// stereo, reorder, antialias, in 26-bit fixed point (3 integer bits, 22
// fraction bits; coefficients 18-bit with 17 fraction bits). Bit-exact with
// crates/mp3model/src/fixed.rs at the hardware word lengths.
//
// Input: mp3_granule's records (61 scale factors, then 576 values as 16-bit
// LE) with the granule's side info alongside. A granule's channels arrive in
// turn; once the last is in:
//   - requantise each channel in place: |n|^(4/3) from mp3_pow43 (entries k
//     and k + 1 interpolated above 1023), times 2^(q mod 4 / 4), shifted by
//     q div 4, rounded, saturated, signed;
//   - stereo (joint stereo frames): intensity above the right channel's last
//     non-zero band (per window for short blocks), mid/side elsewhere;
//   - antialias butterflies (long blocks; the first boundary of mixed ones);
//   - out: each channel's 576 values in IMDCT order (short blocks read
//     through the reorder map) as 32-bit LE on spec_*, with a CRC-32.
module mp3_spectral (
    input  logic        clk,
    input  logic        new_stream,
    // Granule records from mp3_granule
    input  logic        in_valid,
    input  logic [7:0]  in_data,
    input  logic [31:0] in_hdr,
    input  logic [73:0] in_ent,
    input  logic        in_ch,
    output logic        ready,        // may take a new record
    // Spectra out
    output logic        spec_valid,
    output logic [7:0]  spec_data,
    input  logic        spec_ready,   // the next stage can take a channel
    output logic [73:0] spec_ent,     // that channel's side info
    output logic        spec_ch,
    output logic        spec_mono,
    output logic [1:0]  spec_rate,    // the frame's sampling-rate index
    output logic [31:0] spectra,
    output logic [31:0] crc
);
    localparam logic signed [25:0] MAX = 26'sd33554431;   // 2^25 - 1

    // Data x coefficient sum (46 bits), back to data: round half up at 17
    // bits, saturate to 26.
    function automatic logic signed [25:0] rnd17(input logic signed [46:0] acc);
        logic signed [46:0] r;
        r = (acc + 47'sd65536) >>> 17;
        rnd17 = r > 47'sd33554431 ? MAX : r < -47'sd33554432 ? -26'sd33554432 : r[25:0];
    endfunction
    function automatic logic [31:0] crc_byte(input logic [31:0] c, input logic [7:0] d);
        logic [31:0] x;
        x = c ^ {24'd0, d};
        for (int j = 0; j < 8; j++) x = x[0] ? (x >> 1) ^ 32'hEDB88320 : x >> 1;
        crc_byte = x;
    endfunction

    // ------------------------------------------------------------ granule info
    logic [31:0] hdr = '0;
    logic [73:0] ent [0:1];
    wire        mono  = hdr[7:6] == 2'b11;
    wire  [1:0] rate  = hdr[11:10];
    wire        joint = hdr[7:6] == 2'b01;
    wire        ms_on = joint && hdr[5];
    wire        is_on = joint && hdr[4];

    logic       c = 1'b0;                 // channel being worked on
    wire [73:0] e   = ent[c];
    wire [7:0]  gg  = e[52:45];
    wire        ws  = e[40], mix = e[39];
    wire [1:0]  bt  = e[38:37];
    wire [2:0]  sbg [0:2];
    assign sbg[0] = e[21:19];
    assign sbg[1] = e[18:16];
    assign sbg[2] = e[15:13];
    wire        pre = e[2], scs = e[1];
    wire        short_blk = ws && bt == 2'd2;

    // ------------------------------------------------------------ multiplier
    // One 27 x 19 signed multiplier, shared by requantise, stereo and
    // antialias: operands set in one cycle, product two cycles later.
    logic signed [26:0] m_a = '0;
    logic signed [18:0] m_b = '0;
    logic signed [45:0] m_p = '0;
    always_ff @(posedge clk) m_p <= m_a * m_b;
    logic signed [46:0] acc = '0, acc2 = '0;

    // ------------------------------------------------------------ RAMs
    // Values of both channels: {channel, index}.
    // 27 bits stored (the top one always 0): three x9 block RAMs with every
    // data input used, or bitgen's DRC rejects an unconnected parity input.
    logic [26:0] xr [0:2047];
    logic        x_we = 1'b0;
    logic [10:0] x_waddr = '0, x_raddr = '0;
    logic signed [25:0] x_wdata = '0, x_rdata;
    logic [26:0] x_rd27;
    always_ff @(posedge clk) begin
        if (x_we) xr[x_waddr] <= {1'b0, x_wdata};
        x_rd27 <= xr[x_raddr];
    end
    assign x_rdata = $signed(x_rd27[25:0]);
    // Scale factors of the last channel in (the right one for stereo).
    logic [3:0]  sf_mem [0:63];
    logic        sf_we = 1'b0;
    logic [5:0]  sf_widx = '0, sf_ridx;
    logic [3:0]  sf_wval = '0;
    always_ff @(posedge clk) if (sf_we) sf_mem[sf_widx] <= sf_wval;
    wire  [3:0]  sf_r = sf_mem[sf_ridx];

    // ------------------------------------------------------------ tables
    logic [10:0] pa_addr, pb_addr;
    logic [21:0] pa, pb;
    mp3_pow43 u_pow (.clk, .addr_a(pa_addr), .addr_b(pb_addr), .data_a(pa), .data_b(pb));
    logic [17:0] frac;
    logic [1:0]  frac_k;
    mp3_frac4 u_frac (.k(frac_k), .v(frac));

    // Coded-order position of value i: band, window, index in the window.
    logic [4:0] band = '0;
    logic [1:0] win = '0;
    logic [7:0] j = '0;
    logic       in_short = 1'b0;          // in the short part
    logic [9:0] lend;                     // end of long band `band`
    logic [7:0] s_lo, s_hi;               // short band `band` edges
    mp3_sfb_long  u_lb (.rate, .idx(band + 5'd1), .edge_(lend));
    mp3_sfb_short u_sl (.rate, .idx(band[3:0]), .edge_(s_lo));
    mp3_sfb_short u_sh (.rate, .idx(band[3:0] + 4'd1), .edge_(s_hi));
    // The short band's width, registered: the position steps at most once
    // every few cycles (T_RQW, T_ST7), long after `band` settles.
    logic [7:0] width = '0;
    always_ff @(posedge clk) width <= s_hi - s_lo;
    logic [1:0] pretab;
    mp3_pretab u_pre (.band, .pre(pretab));

    // The scale factor index for the current position, and the exponent q
    // (quarter powers of two) of the current value.
    assign sf_ridx = st == T_ST ? is_sf_idx : in_short ? 6'd22 + 6'(3 * band) + 6'(win) : 6'(band);
    wire signed [9:0] base  = 10'(gg) - 10'sd210;
    wire        [2:0] scale = scs ? 3'd4 : 3'd2;
    // From the scale factor and pretab registered a cycle earlier (sfv, ptv).
    logic [3:0] sfv = '0, sfq = '0;
    logic [1:0] ptv = '0;
    logic       ist_pre = 1'b0;
    wire signed [9:0] q_now = in_short
        ? base - 10'(8 * sbg[win]) - 10'(scale * sfv)
        : base - 10'(scale * (sfv + (pre ? 4'(ptv) : 4'd0)));

    // Advance the coded-order position by one value.
    task automatic step_pos(input logic [9:0] i_now);
        if (!in_short) begin
            if (10'(i_now) + 10'd1 == lend) begin
                if (short_blk && mix && band == 5'd7) begin
                    in_short <= 1'b1;
                    band <= 5'd3;
                    win <= '0;
                    j <= '0;
                end else begin
                    band <= band + 1'b1;
                end
            end
        end else if (j + 8'd1 == width) begin
            j <= '0;
            if (win == 2'd2) begin
                win <= '0;
                band <= band + 1'b1;
            end else begin
                win <= win + 1'b1;
            end
        end else begin
            j <= j + 1'b1;
        end
    endtask

    // ------------------------------------------------------------ control
    typedef enum logic [5:0] {
        T_IN, T_RQ0, T_RQ1, T_RQ2, T_RQ3, T_RQ4, T_RQ4B, T_RQ4C, T_RQ5, T_RQ6, T_RQ7, T_RQ8, T_RQW,
        T_ST, T_ST1, T_ST2, T_ST3, T_ST4, T_ST5, T_ST6, T_ST7,
        T_AA, T_AA1, T_AA2, T_AA3, T_AA4, T_AA5, T_AA6, T_AA7, T_AA8, T_AA9, T_AAW,
        T_OUT, T_OUT1, T_OUT2
    } tstate_t;
    tstate_t st = T_IN;
    assign ready = st == T_IN && nin == 0;   // (as in mp3_imdct: only before a record)
    assign spec_ent = ent[c];
    assign spec_ch = c;
    assign spec_mono = mono;
    assign spec_rate = rate;

    logic        got = 1'b0, pend = 1'b0;
    logic signed [8:0]  sh = '0;
    logic [18:0] m0r = '0, m1r = '0;
    logic [4:0]  e0r = '0;
    logic [21:0] dm = '0;
    logic [40:0] sum = '0;
    logic [25:0] rq = '0;
    logic signed [25:0] yl = '0, yr = '0;
    logic [10:0] nin = '0;                // bytes of the record so far
    logic [7:0]  lo = '0;
    logic [9:0]  i = '0;

    // Requantise pipeline registers.
    logic        neg = 1'b0;
    logic [13:0] n = '0;
    logic signed [9:0] q = '0;
    logic [18:0] mant = '0;
    logic [4:0]  ex = '0;
    logic [35:0] prod = '0;

    // Intensity-stereo bookkeeping for the right channel.
    logic        any_long = 1'b0;
    logic [4:0]  lnz_long = '0;
    logic [2:0]  any_short = '0;
    logic [4:0]  lnz_short [0:2];
    wire  [4:0]  first_short = (short_blk && mix) ? 5'd3 : 5'd0;
    wire  [4:0]  start_long  = any_long ? lnz_long + 1'b1 : 5'd0;
    wire  [4:0]  start_w     = any_short[win] ? lnz_short[win] + 1'b1 : first_short;
    wire         short_zero  = any_short == 3'b000;
    logic        is_here;                 // the current value is intensity-coded
    logic [5:0]  is_sf_idx;
    always_comb begin
        is_here = 1'b0;
        is_sf_idx = '0;
        if (is_on) begin
            if (!short_blk) begin
                is_here = band >= start_long;
                is_sf_idx = 6'(band > 5'd20 ? 5'd20 : band);
            end else if (in_short) begin
                is_here = band >= start_w;
                is_sf_idx = 6'd22 + 6'(3 * (band > 5'd11 ? 5'd11 : band)) + 6'(win);
            end else begin                // long part of a mixed block
                is_here = short_zero && band >= start_long;
                is_sf_idx = 6'(band);
            end
        end
    end
    logic        ist = 1'b0;              // registered: intensity here, and its position
    logic [2:0]  ipos = '0;
    logic signed [25:0] xa = '0, xb = '0;
    logic signed [17:0] kl, kr, ms_k, aa_cs, aa_ca;
    logic [2:0]  aa_i = '0;
    mp3_coefs u_coefs (.aa_i, .is_pos(ipos), .aa_cs, .aa_ca, .ms(ms_k), .is_kl(kl), .is_kr(kr));

    // Antialias.
    logic [4:0]  sb = '0;
    logic [4:0]  sb_end;
    assign sb_end = !short_blk ? 5'd31 : 5'd1;   // last boundary (mixed: the first only)

    // Output.
    logic [9:0]  k = '0, coded;
    logic [1:0]  ob = '0;
    mp3_reorder u_ro (.clk, .rate, .k, .coded);
    logic [31:0] crc_r = 32'hFFFFFFFF;
    assign crc = ~crc_r;
    initial spectra = '0;

    // Reset the position sequencer at the start of a pass.
    task automatic start_pos();
        i <= '0;
        band <= '0;
        win <= '0;
        j <= '0;
        in_short <= short_blk && !mix;
    endtask

    always_ff @(posedge clk) begin
        x_we <= 1'b0;
        sf_we <= 1'b0;
        spec_valid <= 1'b0;
        pend <= 1'b0;
        if (pend) begin                               // stereo: the right value
            x_we <= 1'b1;
            x_waddr <= {1'b1, i - 1'b1};
            x_wdata <= yr;
        end
        case (st)
            // ---- a record comes in: scale factors, then raw values
            T_IN:
                if (in_valid) begin
                    if (nin == 0) begin
                        hdr <= in_hdr;
                        ent[in_ch] <= in_ent;
                        c <= in_ch;
                    end
                    if (nin < 61) begin
                        sf_we <= 1'b1;
                        sf_widx <= 6'(nin);
                        sf_wval <= in_data[3:0];
                    end else if (nin[0] == 1'b1) begin
                        lo <= in_data;                // values start at byte 61: low bytes at odd counts
                    end else begin
                        x_we <= 1'b1;
                        x_waddr <= {in_ch, 10'((nin - 11'd62) >> 1)};
                        x_wdata <= 26'($signed({in_data, lo}));
                    end
                    nin <= nin + 1'b1;
                    if (nin == 11'd61 + 11'd1151) begin
                        nin <= '0;
                        st <= T_RQ0;
                        start_pos();
                        any_long <= 1'b0;
                        any_short <= '0;
                    end
                end
            // ---- requantise channel c in place
            T_RQ0: begin
                x_raddr <= {c, i};
                st <= T_RQ1;
            end
            T_RQ1: begin                              // x_rdata arrives; take the scale factor
                sfv <= sf_r;
                ptv <= pretab;
                st <= T_RQ2;
            end
            T_RQ2: begin
                neg <= x_rdata < 0;
                n <= 14'(x_rdata < 0 ? -x_rdata : x_rdata);
                q <= q_now;
                // Bookkeeping for intensity stereo (right channel).
                if (x_rdata != 0) begin
                    if (!in_short) begin
                        any_long <= 1'b1;
                        lnz_long <= band;
                    end else begin
                        any_short[win] <= 1'b1;
                        lnz_short[win] <= band;
                    end
                end
                st <= T_RQ3;
            end
            T_RQ3: st <= T_RQ4;                       // table entries arrive
            // |n|^(4/3): the entry itself below 1024, else entries k and k + 1
            // interpolated over three cycles (align, scale the difference, add).
            T_RQ4: begin
                m0r <= {1'b0, pa[17:0]};
                e0r <= {1'b0, pa[21:18]};
                m1r <= {1'b0, pb[17:0]} << (pb[21:18] - pa[21:18]);
                st <= T_RQ4B;
            end
            T_RQ4B: begin
                // (difference) * f, f = 0..7, as shifts and adds: a multiplier
                // block here shares routing with the busy table RAM beside it.
                logic [21:0] d;
                d = 22'(m1r - m0r);
                dm <= (n[0] ? d : 22'd0) + (n[1] ? d << 1 : 22'd0) + (n[2] ? d << 2 : 22'd0);
                st <= T_RQ4C;
            end
            T_RQ4C: begin
                logic [18:0] v;
                if (n < 14'd1024) begin
                    mant <= m0r;
                    ex <= e0r;
                end else begin
                    v = m0r + 19'((dm + 22'd4) >> 3);
                    if (v >= 19'h40000) begin
                        mant <= (v + 19'd1) >> 1;
                        ex <= e0r + 5'd5;
                    end else begin
                        mant <= v;
                        ex <= e0r + 5'd4;
                    end
                end
                st <= T_RQ5;
            end
            T_RQ5: begin
                m_a <= 27'(mant[17:0]);
                m_b <= 19'(frac);
                sh <= 9'sd12 - 9'($signed({1'b0, ex})) - 9'(q >>> 2);
                st <= T_RQ6;
            end
            T_RQ6: st <= T_RQ7;                       // the product arrives
            T_RQ7: begin
                // Add the rounding constant for a right shift by sh (10..39).
                sum <= 41'(m_p[35:0]) + (sh >= 10 && sh < 40 ? (41'd1 << (sh - 1)) : 41'd0);
                st <= T_RQ8;
            end
            T_RQ8: begin
                logic [40:0] r;
                if (n == 0 || sh >= 40) r = '0;
                else if (sh < 10)       r = 41'd33554431;
                else                    r = sum >> sh;
                if (r > 41'd33554431) r = 41'd33554431;
                rq <= r[25:0];
                st <= T_RQW;
            end
            T_RQW: begin
                x_we <= 1'b1;
                x_waddr <= {c, i};
                x_wdata <= neg ? -$signed(rq) : $signed(rq);
                step_pos(i);
                i <= i + 1'b1;
                if (i == 10'd575) begin
                    // The last channel of this granule? Then the rest.
                    if (mono || c) begin
                        c <= 1'b0;
                        start_pos();
                        if (!mono && (ms_on || is_on)) st <= T_ST;
                        else begin st <= T_AA; sb <= 5'd1; aa_i <= '0; end
                    end else begin
                        st <= T_IN;
                    end
                end else begin
                    st <= T_RQ0;
                end
            end
            // ---- stereo: both channels, value by value (sf_mem holds the right channel's)
            T_ST: begin
                ist_pre <= is_here;
                sfq <= sf_r;
                x_raddr <= {1'b0, i};
                st <= T_ST1;
            end
            T_ST1: begin
                ist <= ist_pre && sfq != 4'd7;
                ipos <= sfq[2:0];
                x_raddr <= {1'b1, i};
                st <= T_ST2;
            end
            T_ST2: begin
                xa <= x_rdata;                        // left (mid)
                st <= T_ST3;
            end
            T_ST3: begin
                xb <= x_rdata;                        // right (side)
                if (ist) begin
                    m_a <= 27'(xa);
                    m_b <= 19'(kl);
                end else begin
                    m_a <= 27'(xa) + 27'(x_rdata);
                    m_b <= 19'(ms_k);
                end
                st <= T_ST4;
            end
            T_ST4: begin
                if (ist) begin
                    m_a <= 27'(xa);
                    m_b <= 19'(kr);
                end else begin
                    m_a <= 27'(xa) - 27'(xb);
                    m_b <= 19'(ms_k);
                end
                st <= T_ST5;
            end
            T_ST5: begin                              // left product
                yl <= (ist && ipos == 3'd6) ? xa : rnd17(47'(m_p));
                st <= T_ST6;
            end
            T_ST6: begin                              // right product
                yr <= (ist && ipos == 3'd0) ? xa : rnd17(47'(m_p));
                st <= T_ST7;
            end
            T_ST7: begin
                // Intensity, or mid/side; otherwise the values stay.
                if (ist || ms_on) begin
                    x_we <= 1'b1;
                    x_waddr <= {1'b0, i};
                    x_wdata <= yl;
                    pend <= 1'b1;                     // the right one next cycle
                end
                step_pos(i);
                i <= i + 1'b1;
                if (i == 10'd575) begin
                    st <= T_AA;
                    c <= 1'b0;
                    sb <= 5'd1;
                    aa_i <= '0;
                end else begin
                    st <= T_ST;
                end
            end
            // ---- antialias channel c
            T_AA:
                if (short_blk && !mix) begin
                    st <= T_OUT;                      // pure short: no antialias
                end else begin
                    x_raddr <= {c, 10'(18 * sb) - 10'd1 - 10'(aa_i)};
                    st <= T_AA1;
                end
            T_AA1: begin
                x_raddr <= {c, 10'(18 * sb) + 10'(aa_i)};
                st <= T_AA2;
            end
            T_AA2: begin
                xa <= x_rdata;                        // a = lower subband's top value
                st <= T_AA3;
            end
            T_AA3: begin                              // b arrives; products a*cs, b*ca, b*cs, a*ca
                xb <= x_rdata;
                m_a <= 27'(xa);
                m_b <= 19'(aa_cs);
                st <= T_AA4;
            end
            T_AA4: begin
                m_a <= 27'(xb);
                m_b <= 19'(aa_ca);
                st <= T_AA5;
            end
            T_AA5: begin
                acc <= 47'(m_p);                      // a*cs
                m_a <= 27'(xb);
                m_b <= 19'(aa_cs);
                st <= T_AA6;
            end
            T_AA6: begin
                acc <= acc - 47'(m_p);                // - b*ca
                m_a <= 27'(xa);
                m_b <= 19'(aa_ca);
                st <= T_AA7;
            end
            T_AA7: begin
                acc2 <= 47'(m_p);                     // b*cs
                st <= T_AA8;
            end
            T_AA8: begin
                acc2 <= acc2 + 47'(m_p);              // + a*ca
                st <= T_AA9;
            end
            T_AA9: begin
                x_we <= 1'b1;
                x_waddr <= {c, 10'(18 * sb) - 10'd1 - 10'(aa_i)};
                x_wdata <= rnd17(acc);
                st <= T_AAW;
            end
            T_AAW: begin
                x_we <= 1'b1;
                x_waddr <= {c, 10'(18 * sb) + 10'(aa_i)};
                x_wdata <= rnd17(acc2);
                if (aa_i != 3'd7) begin
                    aa_i <= aa_i + 1'b1;
                    st <= T_AA;
                end else begin
                    aa_i <= '0;
                    if (sb != sb_end) begin
                        sb <= sb + 1'b1;
                        st <= T_AA;
                    end else if (!mono && !c) begin
                        c <= 1'b1;
                        sb <= 5'd1;
                        st <= T_AA;
                    end else begin
                        c <= 1'b0;
                        k <= '0;
                        st <= T_OUT;
                    end
                end
            end
            // ---- out: each channel in IMDCT order, 32-bit LE
            T_OUT:
                // (also reached from T_AA for pure short blocks: one channel at a time)
                if (k != 0 || spec_ready) begin
                    st <= T_OUT1;                     // `coded` for k arrives
                    ob <= '0;
                end
            T_OUT1: begin
                x_raddr <= {c, (short_blk && !(mix && k < 10'd36)) ? coded : k};
                st <= T_OUT2;
            end
            T_OUT2:
                if (ob == 2'd0 && !got) begin
                    got <= 1'b1;                      // x_rdata arrives next cycle
                end else begin
                    logic [31:0] w;
                    logic [7:0]  b;
                    w = 32'($signed(x_rdata));
                    b = w[8 * ob +: 8];
                    spec_valid <= 1'b1;
                    spec_data <= b;
                    crc_r <= crc_byte(crc_r, b);
                    ob <= ob + 1'b1;
                    if (ob == 2'd3) begin
                        got <= 1'b0;
                        k <= k + 1'b1;
                        if (k == 10'd575) begin
                            spectra <= spectra + 1'b1;
                            k <= '0;
                            if (!mono && !c) begin
                                c <= 1'b1;
                                st <= short_blk && !mix ? T_AA : T_OUT;
                            end else begin
                                c <= 1'b0;
                                st <= T_IN;
                            end
                        end else begin
                            st <= T_OUT;
                        end
                    end
                end
            default: st <= T_IN;
        endcase
        if (new_stream) begin
            st <= T_IN;
            nin <= '0;
            spectra <= '0;
            crc_r <= 32'hFFFFFFFF;
        end
    end
    // Table addresses: entry n (or k = n div 8 and k + 1 above 1023).
    assign pa_addr = n < 14'd1024 ? 11'(n) : 11'(n >> 3);
    assign pb_addr = 11'(n >> 3) + 11'd1;
    assign frac_k = q[1:0];
endmodule
