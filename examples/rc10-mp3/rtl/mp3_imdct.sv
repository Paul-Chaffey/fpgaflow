`timescale 1ns/1ps

// MP3 IMDCT, windowing, overlap-add and frequency inversion
// (docs/mp3-decoder-plan.md, milestone 4). Bit-exact with FixedBackend::imdct
// in crates/mp3model/src/fixed.rs (26-bit data, 18-bit coefficients).
//
// Takes one channel's 576 spectral values from mp3_spectral (32-bit LE),
// then for each subband sb (block type 0 for the first two of a mixed block):
//   long:  z[i] = round17(sum_k x[18 sb + k] * C[w][i][k]),        i = 0..35
//   short: z = 0; z[6 + 6 w + i] += round17(sum_k x[18 sb + 3 k + w] * S[i][k])
//   out[18 sb + t] = sat(z[t] + overlap[ch][sb][t]), negated for odd sb and t;
//   overlap[ch][sb][t] = z[t + 18]
// The output (27 bits: the negation of a saturated -2^25 is +2^25) goes out
// as 32-bit LE values on sub_*, with a CRC-32.
module mp3_imdct (
    input  logic        clk,
    input  logic        new_stream,
    input  logic        in_valid,
    input  logic [7:0]  in_data,
    input  logic [73:0] in_ent,       // the channel's side info (block type)
    input  logic        in_ch,
    output logic        ready,
    output logic        sub_valid,
    output logic [7:0]  sub_data,
    output logic [31:0] subbands,
    output logic [31:0] crc
);
    localparam logic signed [25:0] MAX = 26'sd33554431;
    function automatic logic signed [25:0] sat26(input logic signed [47:0] v);
        sat26 = v > 48'sd33554431 ? MAX : v < -48'sd33554432 ? -26'sd33554432 : v[25:0];
    endfunction
    function automatic logic signed [25:0] rnd17(input logic signed [47:0] acc);
        rnd17 = sat26((acc + 48'sd65536) >>> 17);
    endfunction
    function automatic logic [31:0] crc_byte(input logic [31:0] c, input logic [7:0] d);
        logic [31:0] x;
        x = c ^ {24'd0, d};
        for (int j = 0; j < 8; j++) x = x[0] ? (x >> 1) ^ 32'hEDB88320 : x >> 1;
        crc_byte = x;
    endfunction

    // ------------------------------------------------------------ RAMs
    // Input values (36 bits stored so the block RAMs have no unconnected
    // parity inputs; bitgen's DRC rejects those).
    logic [35:0] xin [0:1023];
    logic        xi_we = 1'b0;
    logic [9:0]  xi_waddr = '0, xi_raddr = '0;
    logic [25:0] xi_wdata = '0;
    logic [35:0] xi_rd36;
    always_ff @(posedge clk) begin
        if (xi_we) xin[xi_waddr] <= {10'd0, xi_wdata};
        xi_rd36 <= xin[xi_raddr];
    end
    wire signed [25:0] xi_rdata = $signed(xi_rd36[25:0]);

    // Overlap: {channel, subband, t} -> 2048 x 27 (three x9 block RAMs).
    logic [26:0] ov [0:2047];
    logic        ov_we = 1'b0;
    logic [10:0] ov_waddr = '0, ov_raddr = '0;
    logic [26:0] ov_wdata = '0, ov_rd27;
    always_ff @(posedge clk) begin
        if (ov_we) ov[ov_waddr] <= ov_wdata;
        ov_rd27 <= ov[ov_raddr];
    end
    wire signed [25:0] ov_rdata = $signed(ov_rd27[25:0]);

    // z[0..35] of the current subband (LUT RAM, asynchronous read).
    logic signed [25:0] z [0:35];
    logic        z_we;
    logic [5:0]  z_widx, z_ridx;
    logic signed [25:0] z_wdata;
    always_ff @(posedge clk) if (z_we) z[z_widx] <= z_wdata;
    wire signed [25:0] z_r = z[z_ridx];

    // Coefficients.
    logic [10:0] c_addr = '0;
    logic signed [17:0] coef;
    mp3_imdct_rom u_rom (.clk, .addr(c_addr), .data(coef));

    // ------------------------------------------------------------ channel
    logic [73:0] ent = '0;
    logic        ch = 1'b0;
    wire         ws = ent[40], mix = ent[39];
    wire  [1:0]  bt = ent[38:37];
    logic [4:0]  sb = '0;
    wire  [1:0]  bt_sb = (ws && mix && sb < 5'd2) ? 2'd0 : bt;
    wire         short_sb = bt_sb == 2'd2;
    wire  [10:0] wbase = bt_sb == 2'd1 ? 11'd648 : bt_sb == 2'd3 ? 11'd1296 : 11'd0;

    // ------------------------------------------------------------ MAC
    // Terms are issued one a cycle (input and coefficient addresses); the
    // data arrive two cycles later, the product a cycle after that.
    logic [4:0]  kk = '0, nterms = '0;
    logic        issuing = 1'b0;
    logic [3:0]  vpipe = '0;            // a term in flight: [1] data here, [2] product here
    logic [3:0]  fpipe = '0;            // ... and it is the first term of a sum
    logic [3:0]  lpipe = '0;            // ... and it is the last
    logic signed [43:0] prod = '0;
    logic signed [47:0] acc = '0;
    logic        acc_done = 1'b0;       // acc holds a finished sum (one cycle)

    // ------------------------------------------------------------ control
    typedef enum logic [3:0] {
        I_CLR, I_IN, I_SB, I_ZCLR, I_OUT, I_MAC, I_ZR, I_ZW, I_EMIT, I_EMIT1, I_EMIT2, I_EMIT2B, I_EMIT3
    } istate_t;
    istate_t st = I_CLR;
    // Ready only for the first byte of a channel: the sender's data lag its
    // state by a cycle, so "in I_IN" alone would also be true while the last
    // byte of the previous channel is arriving.
    assign ready = st == I_IN && nin == 0;

    logic [11:0] nin = '0;
    logic [23:0] part = '0;
    logic [5:0]  oi = '0;               // output index i (0..35) of the current MAC
    logic [1:0]  win = '0;
    logic [5:0]  t = '0;
    logic [1:0]  ob = '0;
    logic [26:0] v27 = '0;
    logic [10:0] clr = '0;
    logic [31:0] crc_r = 32'hFFFFFFFF;
    assign crc = ~crc_r;
    initial subbands = '0;

    // z index of the MAC's output (registered in I_OUT), its rounded sum
    // (registered when done), and the emit step's z[t] and sums.
    logic [5:0] zi = '0;
    logic signed [25:0] rr = '0, zt = '0, sreg = '0, zold = '0;
    always_comb begin
        z_we = 1'b0;
        z_widx = '0;
        z_wdata = '0;
        z_ridx = zi;
        if (st == I_ZCLR) begin
            z_we = 1'b1;
            z_widx = 6'(t);
        end else if (st == I_ZW) begin
            z_we = 1'b1;
            z_widx = zi;
            z_wdata = short_sb ? sat26(48'(zold) + 48'(rr)) : rr;
        end else if (st == I_EMIT) begin
            z_ridx = 6'(t);
        end else if (st == I_EMIT1) begin
            z_ridx = 6'(t) + 6'd18;
        end
    end

    always_ff @(posedge clk) begin
        xi_we <= 1'b0;
        ov_we <= 1'b0;
        sub_valid <= 1'b0;
        acc_done <= 1'b0;
        // MAC pipeline.
        vpipe <= {vpipe[2:0], issuing};
        fpipe <= {fpipe[2:0], issuing && kk == 0};
        lpipe <= {lpipe[2:0], issuing && kk == nterms - 1'b1};
        if (vpipe[1]) prod <= xi_rdata * coef;
        if (vpipe[2]) acc <= (fpipe[2] ? 48'sd0 : acc) + 48'(prod);
        if (vpipe[2] && lpipe[2]) acc_done <= 1'b1;   // acc holds the sum next cycle
        case (st)
            // ---- after reset or a new stream: clear the overlap buffers
            I_CLR: begin
                ov_we <= 1'b1;
                ov_waddr <= clr;
                ov_wdata <= '0;
                clr <= clr + 1'b1;
                if (clr == 11'd2047) st <= I_IN;
            end
            // ---- one channel's 576 values come in
            I_IN:
                if (in_valid) begin
                    if (nin == 0) begin
                        ent <= in_ent;
                        ch <= in_ch;
                    end
                    if (nin[1:0] != 2'd3) begin
                        part <= {in_data, part[23:8]};
                    end else begin
                        xi_we <= 1'b1;
                        xi_waddr <= 10'(nin >> 2);
                        xi_wdata <= {in_data[1:0], part};
                    end
                    nin <= nin + 1'b1;
                    if (nin == 12'd2303) begin
                        nin <= '0;
                        sb <= '0;
                        st <= I_SB;
                    end
                end
            // ---- a subband: clear z for short blocks, then the MACs
            I_SB: begin
                t <= '0;
                oi <= '0;
                win <= '0;
                st <= short_sb ? I_ZCLR : I_OUT;
            end
            I_ZCLR: begin
                t <= t + 1'b1;
                if (t == 6'd35) st <= I_OUT;
            end
            I_OUT: begin                       // start the MAC for output oi
                zi <= short_sb ? 6'd6 + 6'(6 * win) + oi : oi;
                kk <= '0;
                nterms <= short_sb ? 5'd6 : 5'd18;
                issuing <= 1'b1;
                st <= I_MAC;
            end
            I_MAC: begin
                if (issuing) begin
                    xi_raddr <= short_sb ? 10'(18 * sb) + 10'(3 * kk) + 10'(win) : 10'(18 * sb) + 10'(kk);
                    c_addr <= short_sb ? 11'd1944 + 11'(6 * oi) + 11'(kk) : wbase + 11'(18 * oi) + 11'(kk);
                    kk <= kk + 1'b1;
                    if (kk == nterms - 1'b1) issuing <= 1'b0;
                end
                if (acc_done) begin
                    rr <= rnd17(acc);
                    st <= I_ZR;
                end
            end
            I_ZR: begin                        // read z[zi] (short blocks add to it)
                zold <= z_r;
                st <= I_ZW;
            end
            I_ZW: begin                        // z written (above); next output
                begin
                    if (!short_sb) begin
                        if (oi == 6'd35) begin t <= '0; st <= I_EMIT; end
                        else begin oi <= oi + 1'b1; st <= I_OUT; end
                    end else begin
                        if (oi == 6'd11) begin
                            oi <= '0;
                            if (win == 2'd2) begin t <= '0; st <= I_EMIT; end
                            else begin win <= win + 1'b1; st <= I_OUT; end
                        end else begin
                            oi <= oi + 1'b1;
                            st <= I_OUT;
                        end
                    end
                end
            end
            // ---- out: overlap-add, frequency inversion, new overlap
            I_EMIT: begin
                ov_raddr <= {ch, sb, t[4:0]};             // (t < 18)
                zt <= z_r;                                 // z[t]
                st <= I_EMIT1;
            end
            I_EMIT1: begin                     // keep z[t + 18] as the next overlap
                ov_we <= 1'b1;
                ov_waddr <= {ch, sb, t[4:0]};
                ov_wdata <= 27'($signed(z_r));
                st <= I_EMIT2;
            end
            I_EMIT2: begin                     // ov_rdata (the old overlap) is here
                sreg <= sat26(48'(zt) + 48'(ov_rdata));
                st <= I_EMIT2B;
            end
            I_EMIT2B: begin                    // frequency inversion
                v27 <= (sb[0] && t[0]) ? -27'(sreg) : 27'(sreg);
                ob <= '0;
                st <= I_EMIT3;
            end
            I_EMIT3: begin
                logic [31:0] w;
                logic [7:0]  b;
                w = 32'($signed(v27));
                b = w[8 * ob +: 8];
                sub_valid <= 1'b1;
                sub_data <= b;
                crc_r <= crc_byte(crc_r, b);
                ob <= ob + 1'b1;
                if (ob == 2'd3) begin
                    if (t == 6'd17) begin
                        if (sb == 5'd31) begin
                            subbands <= subbands + 1'b1;
                            st <= I_IN;
                        end else begin
                            sb <= sb + 1'b1;
                            st <= I_SB;
                        end
                    end else begin
                        t <= t + 1'b1;
                        st <= I_EMIT;
                    end
                end
            end
            default: st <= I_IN;
        endcase
        if (new_stream) begin
            st <= I_CLR;
            clr <= '0;
            nin <= '0;
            issuing <= 1'b0;
            subbands <= '0;
            crc_r <= 32'hFFFFFFFF;
        end
    end
endmodule
