`timescale 1ns/1ps

// MP3 granule decoder, part 1 (docs/mp3-decoder-plan.md, milestone 2):
// scale factors and Huffman decoding, reading the bit reservoir.
// Matches crates/mp3model (frontend::granules) granule for granule.
//
// Per frame from the front end (`side`, layout in mp3_frontend.sv), for each
// granule and channel:
//   - part 2: scale factors (long, short or mixed layout; scfsi reuses
//     granule 0's long ones), into a small RAM: 0..21 long, 22 + 3 * band +
//     window short; a mask marks the ones written (the rest are 0)
//   - part 3: big-value pairs (three regions, table per region, linbits,
//     signs) then count1 quadruples until part2_3_length bits are used (a
//     quadruple that runs past the end is dropped), zero to 576
//   - a record: 61 scale factor bytes then 576 values as 16-bit LE, one byte
//     a cycle on rec_*, with a running CRC-32
// Frames whose main data begins before the stream are skipped.
module mp3_granule (
    input  logic         clk,
    input  logic         new_stream,
    input  logic [357:0] side,
    input  logic         fd_valid,
    output logic         fd_ready,
    output logic [11:0]  res_raddr,
    input  logic [7:0]   res_rdata,
    output logic         rec_valid,
    output logic [7:0]   rec_data,
    output logic [31:0]  granules,
    output logic [31:0]  crc
);
    // ------------------------------------------------------------ frame
    logic [357:0] fr = '0;
    wire  [31:0]  hdr      = fr[357:326];
    wire  [7:0]   scfsi    = fr[316:309];
    wire  [11:0]  md_start = fr[308:297];
    wire          mono     = hdr[7:6] == 2'b11;
    wire  [1:0]   rate     = hdr[11:10];

    logic gr = 1'b0, ch = 1'b0;
    // This granule's side info, latched when it starts (G_SEEK).
    logic [73:0] ent = '0;
    wire  [11:0] p23  = ent[73:62];
    wire  [8:0]  bv   = ent[61:53];
    wire  [3:0]  sfc  = ent[44:41];
    wire         ws   = ent[40];
    wire         mix  = ent[39];
    wire  [1:0]  bt   = ent[38:37];
    wire  [4:0]  ts0  = ent[36:32], ts1 = ent[31:27], ts2 = ent[26:22];
    wire  [3:0]  r0   = ent[12:9];
    wire  [5:0]  r1   = ent[8:3];
    wire         c1t  = ent[0];
    wire         short_blk = ws && bt == 2'd2;

    // Region boundaries (samples).
    logic [9:0] e_r1, e_r2;
    wire  [5:0] r12 = 6'(r0) + r1 + 6'd2;
    mp3_sfb_long u_sfb1 (.rate, .idx(5'(r0) + 5'd1), .edge_(e_r1));
    mp3_sfb_long u_sfb2 (.rate, .idx(r12 > 6'd22 ? 5'd22 : r12[4:0]), .edge_(e_r2));
    // Registered in G_SEEK2, once per granule.
    logic [9:0] reg1 = '0, reg2 = '0, big = '0;

    logic [2:0] slen1, slen2;
    mp3_slen u_slen (.sfc, .slen1, .slen2);

    // ------------------------------------------------------------ bit reader
    // A 16-bit buffer refilled from the reservoir (2-cycle read latency).
    // `seek` restarts it at a bit address; reads issued before it are
    // ignored when they arrive (generation tag).
    logic [15:0] bbuf = '0;
    logic [4:0]  bcnt = '0;
    logic [11:0] baddr = '0;
    logic [2:0]  drop = '0;
    logic        gen = 1'b0;
    logic [1:0]  inf = '0;                 // reads in flight
    logic        rd1 = 1'b0, rd2 = 1'b0, g1 = 1'b0, g2 = 1'b0;
    logic        seek;
    logic [14:0] seek_bit;
    logic        take;                     // a consumer uses bit_now this cycle
    wire         bit_ok = bcnt != 0 && drop == 0;
    wire         bit_now = bbuf[15];
    wire         issue = !seek && ({1'b0, bcnt} + {inf, 3'b000}) <= 6'd8;
    wire         arrive = rd2 && g2 == gen;
    always_ff @(posedge clk) begin
        logic [15:0] b;
        logic [4:0]  c;
        // Merge an arriving byte behind the bits held (position from the
        // registered count), then take one bit if wanted: `take` only drives
        // the last shift.
        b = bbuf | (arrive ? ({res_rdata, 8'h00} >> bcnt) : 16'h0000);
        c = bcnt + (arrive ? 5'd8 : 5'd0);
        if (bcnt != 0 && (drop != 0 || take)) begin
            b = {b[14:0], 1'b0};
            c = c - 1'b1;
            if (drop != 0) drop <= drop - 1'b1;
        end
        bbuf <= b;
        bcnt <= c;
        rd1 <= issue;
        g1 <= gen;
        rd2 <= rd1;
        g2 <= g1;
        if (issue) begin
            res_raddr <= baddr;
            baddr <= baddr + 1'b1;
        end
        inf <= inf + issue - (rd2 ? 2'd1 : 2'd0);
        if (seek) begin
            baddr <= seek_bit[14:3];
            drop <= seek_bit[2:0];
            bcnt <= '0;
            bbuf <= '0;
            gen <= ~gen;
        end
    end

    // ------------------------------------------------------------ Huffman trees
    logic [10:0] rom_addr, node = '0, root;
    logic [23:0] rom_data;
    logic        tbl_empty;
    // The table for value i, registered (G_PREP makes sure it is current).
    logic [5:0]  tbl = '0;
    huff_rom  u_rom  (.clk, .addr(rom_addr), .data(rom_data));
    huff_root u_root (.table_no(tbl), .root, .empty(tbl_empty));
    logic [3:0] linbits;
    mp3_linbits u_lin (.table_no(tbl[4:0]), .linbits);
    wire [11:0] child = bit_now ? rom_data[11:0] : rom_data[23:12];

    // ------------------------------------------------------------ values RAM
    // 18 bits wide so a RAMB16 in its x18 mode has every data input used
    // (bitgen rejects unconnected parity inputs); the top 2 bits are 0.
    logic [17:0] is_mem [0:575];
    logic        is_we = 1'b0;
    logic [9:0]  is_waddr = '0, is_raddr = '0;
    logic [15:0] is_wdata = '0, is_rdata;
    logic [17:0] is_rd18;
    always_ff @(posedge clk) begin
        if (is_we) is_mem[is_waddr] <= {2'b00, is_wdata};
        is_rd18 <= is_mem[is_raddr];
    end
    assign is_rdata = is_rd18[15:0];

    // ------------------------------------------------------------ control
    typedef enum logic [4:0] {
        G_IDLE, G_SEEK, G_SEEK2, G_SF, G_PREP, G_PAIR, G_WAIT, G_WALK, G_LIN, G_SIGN, G_WPAIR,
        G_C1, G_C1SIGN, G_WQUAD, G_ZERO, G_EMIT_SF, G_EMIT_IS, G_SAVE, G_NEXT
    } gstate_t;
    gstate_t st = G_IDLE;

    // Scale factors: LUT RAMs with one write port each.
    logic [3:0]  sf_mem [0:63];          // 0..21 long, 22 + 3 * band + window short
    logic [63:0] sf_set = '0;            // written in this granule (else 0)
    logic [3:0]  sav_mem [0:63];         // granule 0's long factors, {channel, band}
    logic        sf_we;
    logic [5:0]  sf_widx;
    logic [3:0]  sf_wval;
    wire  [3:0]  sav_r = sav_mem[{ch, band}];
    always_ff @(posedge clk) if (sf_we) sf_mem[sf_widx] <= sf_wval;
    logic        sav_we = 1'b0;
    logic [5:0]  sav_widx = '0;
    logic [3:0]  sav_wval = '0;
    always_ff @(posedge clk) if (sav_we) sav_mem[sav_widx] <= sav_wval;
    logic [14:0] gstart = '0;            // granule start, bit address in the ring
    logic [12:0] pos = '0;               // bits used in this granule
    logic [9:0]  i = '0;                 // value index

    // Scale factor sequencing.
    logic        sf_short = 1'b0;        // in the short part
    logic [4:0]  band = '0;
    logic [1:0]  win = '0;
    logic [2:0]  nb = '0, k = '0;        // bits in this factor, bits read
    logic [3:0]  sacc = '0;
    wire  [1:0]  group = band < 6 ? 2'd0 : band < 11 ? 2'd1 : band < 16 ? 2'd2 : 2'd3;
    // scfsi: bits 7..4 = channel 0 groups 0..3, bits 3..0 = channel 1.
    // Only plain long blocks reuse granule 0's factors.
    wire         reuse = !short_blk && gr && scfsi[3'd7 - (ch ? 3'd4 : 3'd0) - 3'(group)];

    // Values being decoded.
    logic        quad = 1'b0;            // count1 region
    logic [3:0]  vx = '0, vy = '0;
    logic [15:0] mag [0:3];              // up to 4 magnitudes, then signed
    logic [1:0]  which = '0;             // value being given linbits / sign
    logic [12:0] lacc = '0;
    logic [3:0]  lk = '0;
    logic [2:0]  wn = '0;

    // Emit.
    logic [5:0]  ek = '0;
    wire  [3:0]  sf_r = sf_set[ek] ? sf_mem[ek] : 4'd0;
    logic        hi = 1'b0;
    logic [31:0] crc_r = 32'hFFFFFFFF;
    assign crc = ~crc_r;
    function automatic logic [31:0] crc_byte(input logic [31:0] c, input logic [7:0] d);
        logic [31:0] x;
        x = c ^ {24'd0, d};
        for (int j = 0; j < 8; j++) x = x[0] ? (x >> 1) ^ 32'hEDB88320 : x >> 1;
        crc_byte = x;
    endfunction

    assign fd_ready = st == G_IDLE;

    // Scale factor completion and its write.
    wire sf_done = st == G_SF && (reuse || nb == 0 || k == nb);
    assign sf_we = sf_done;
    assign sf_widx = sf_short ? 6'd22 + 6'(3 * band) + 6'(win) : 6'(band);
    assign sf_wval = reuse ? sav_r : (nb == 0 ? 4'd0 : sacc);

    always_comb begin
        take = 1'b0;
        seek = 1'b0;
        seek_bit = gstart;
        rom_addr = node;
        case (st)
            G_SEEK: seek = 1'b1;
            G_SF:   take = bit_ok && nb != 0 && !reuse && k < nb;
            G_WALK: begin
                take = bit_ok;
                if (bit_ok && !child[11]) rom_addr = child[10:0];
            end
            // Only when a bit is actually due: linbits for a 15 in a table
            // that has them, a sign for a non-zero value.
            G_LIN:            take = bit_ok && mag[which] == 16'd15 && linbits != 0;
            G_SIGN, G_C1SIGN: take = bit_ok && mag[which] != 0;
            default: ;
        endcase
    end

    always_ff @(posedge clk)
        tbl <= quad ? 6'd32 + 6'(c1t) : (i < reg1 ? 6'(ts0) : i < reg2 ? 6'(ts1) : 6'(ts2));

    always_ff @(posedge clk) begin
        is_we <= 1'b0;
        sav_we <= 1'b0;
        rec_valid <= 1'b0;
        if (take) pos <= pos + 1'b1;
        case (st)
            G_IDLE:
                if (fd_valid) begin
                    fr <= side;
                    gr <= 1'b0;
                    ch <= 1'b0;
                    gstart <= {side[308:297], 3'b000};
                    // Undecodable: the reservoir doesn't reach back far enough.
                    if (side[296]) st <= G_SEEK;
                end
            G_SEEK: begin
                ent <= fr[74 * {gr, ch} +: 74];
                st <= G_SEEK2;
            end
            G_SEEK2: begin
                reg1 <= ws ? 10'd36 : e_r1;
                reg2 <= ws ? 10'd576 : e_r2;
                big  <= (bv > 9'd288) ? 10'd576 : {bv, 1'b0};
                pos <= '0;
                i <= '0;
                quad <= 1'b0;
                sf_set <= '0;
                sf_short <= short_blk && !mix;
                band <= '0;
                win <= '0;
                k <= '0;
                sacc <= '0;
                nb <= slen1;
                st <= G_SF;
            end
            // ---- scale factors: one factor at a time, nb bits each
            G_SF: begin
                if (take) begin
                    sacc <= {sacc[2:0], bit_now};
                    k <= k + 1'b1;
                end
                if (sf_done) begin
                    // This factor is complete (stored through sf_we) : move on.
                    sf_set[sf_widx] <= 1'b1;
                    k <= '0;
                    sacc <= '0;
                    if (!sf_short) begin
                        // Long part: bands 0..20, or 0..7 for mixed blocks.
                        if ((short_blk && band == 5'd7) || band == 5'd20) begin
                            if (short_blk) begin
                                sf_short <= 1'b1;
                                band <= 5'd3;
                                win <= '0;
                                nb <= slen1;
                            end else begin
                                st <= G_PREP;
                            end
                        end else begin
                            band <= band + 1'b1;
                            nb <= (band + 1'b1 < 11) ? slen1 : slen2;
                        end
                    end else begin
                        if (win != 2'd2) begin
                            win <= win + 1'b1;
                        end else begin
                            win <= '0;
                            if (band == 5'd11) begin
                                st <= G_PREP;
                            end else begin
                                band <= band + 1'b1;
                                nb <= (band + 1'b1 < 6) ? slen1 : slen2;
                            end
                        end
                    end
                end
            end
            // ---- big values: pick the table and start the walk
            G_PREP: if (quad) st <= G_C1; else st <= G_PAIR;   // `tbl` follows i and quad
            G_PAIR:
                if (i >= big) begin
                    quad <= 1'b1;
                    st <= G_PREP;
                end else if (tbl_empty) begin
                    vx <= '0;
                    vy <= '0;
                    mag[0] <= '0;
                    mag[1] <= '0;
                    st <= G_WPAIR;
                    wn <= '0;
                end else begin
                    node <= root;
                    st <= G_WAIT;
                end
            G_WAIT: st <= G_WALK;       // the ROM word for `node` arrives
            G_WALK:
                if (bit_ok) begin
                    if (child[11]) begin
                        if (!quad) begin
                            mag[0] <= 16'(child[7:4]);
                            mag[1] <= 16'(child[3:0]);
                        end else begin
                            // count1 leaves hold v w x y in bits 7..4.
                            mag[0] <= 16'(child[7]);
                            mag[1] <= 16'(child[6]);
                            mag[2] <= 16'(child[5]);
                            mag[3] <= 16'(child[4]);
                        end
                        which <= '0;
                        lk <= '0;
                        lacc <= '0;
                        if (!quad) st <= G_LIN; else st <= G_C1SIGN;
                    end else begin
                        node <= child[10:0];
                    end
                end
            // ---- pairs: linbits (if 15 and the table has them), then sign
            G_LIN:
                if (mag[which] != 16'd15 || linbits == 0) begin
                    st <= G_SIGN;
                end else if (bit_ok) begin
                    lacc <= {lacc[11:0], bit_now};
                    lk <= lk + 1'b1;
                    if (lk + 1'b1 == linbits) begin
                        mag[which] <= 16'd15 + 16'({lacc[11:0], bit_now});
                        st <= G_SIGN;
                    end
                end
            G_SIGN:
                if (mag[which] == 0) begin
                    if (which == 2'd1) st <= G_WPAIR; else begin which <= 2'd1; lk <= '0; lacc <= '0; st <= G_LIN; end
                    wn <= '0;
                end else if (bit_ok) begin
                    if (bit_now) mag[which] <= -mag[which];
                    if (which == 2'd1) st <= G_WPAIR; else begin which <= 2'd1; lk <= '0; lacc <= '0; st <= G_LIN; end
                    wn <= '0;
                end
            G_WPAIR: begin
                is_we <= 1'b1;
                is_waddr <= i + 10'(wn);
                is_wdata <= mag[wn[1:0]];
                wn <= wn + 1'b1;
                if (wn == 3'd1) begin
                    i <= i + 10'd2;
                    st <= G_PREP;
                end
            end
            // ---- count1 quadruples until part2_3_length bits are used
            G_C1:
                if (i + 10'd4 <= 10'd576 && pos < 13'(p23)) begin
                    node <= root;
                    st <= G_WAIT;
                end else begin
                    st <= G_ZERO;
                end
            G_C1SIGN:
                if (which == 2'd3 && mag[3] == 0) begin
                    st <= G_WQUAD;
                    wn <= '0;
                end else if (mag[which] == 0) begin
                    which <= which + 1'b1;
                end else if (bit_ok) begin
                    if (bit_now) mag[which] <= -mag[which];
                    if (which == 2'd3) begin
                        st <= G_WQUAD;
                        wn <= '0;
                    end else begin
                        which <= which + 1'b1;
                    end
                end
            G_WQUAD:
                if (pos > 13'(p23)) begin
                    st <= G_ZERO;            // ran past the granule: dropped
                end else begin
                    is_we <= 1'b1;
                    is_waddr <= i + 10'(wn);
                    is_wdata <= mag[wn[1:0]];
                    wn <= wn + 1'b1;
                    if (wn == 3'd3) begin
                        i <= i + 10'd4;
                        st <= G_C1;
                    end
                end
            // ---- the rest are zero
            G_ZERO:
                if (i == 10'd576) begin
                    st <= G_EMIT_SF;
                    ek <= '0;
                end else begin
                    is_we <= 1'b1;
                    is_waddr <= i;
                    is_wdata <= '0;
                    i <= i + 1'b1;
                end
            // ---- record: scale factors, then the values (16-bit LE)
            G_EMIT_SF: begin
                rec_valid <= 1'b1;
                rec_data <= 8'(sf_r);
                crc_r <= crc_byte(crc_r, 8'(sf_r));
                ek <= ek + 1'b1;
                if (ek == 6'd60) begin
                    st <= G_EMIT_IS;
                    is_raddr <= '0;
                    i <= '0;
                    hi <= 1'b0;
                    wn <= '0;
                end
            end
            G_EMIT_IS:
                // is_raddr set one cycle before its data is used.
                if (wn == 0) begin
                    wn <= 3'd1;
                end else begin
                    rec_valid <= 1'b1;
                    rec_data <= hi ? is_rdata[15:8] : is_rdata[7:0];
                    crc_r <= crc_byte(crc_r, hi ? is_rdata[15:8] : is_rdata[7:0]);
                    hi <= ~hi;
                    if (hi) begin
                        i <= i + 1'b1;
                        is_raddr <= i + 1'b1;
                        wn <= '0;
                        if (i == 10'd575) begin
                            ek <= '0;
                            if (!gr) st <= G_SAVE; else st <= G_NEXT;
                        end
                    end
                end
            // Granule 0: keep its long factors for scfsi in granule 1.
            G_SAVE: begin
                sav_we <= 1'b1;
                sav_widx <= {ch, ek[4:0]};
                sav_wval <= sf_r;
                ek <= ek + 1'b1;
                if (ek == 6'd20) st <= G_NEXT;
            end
            G_NEXT: begin
                granules <= granules + 1'b1;
                gstart <= gstart + 15'(p23);
                if (!mono && !ch) begin
                    ch <= 1'b1;
                    st <= G_SEEK;
                end else begin
                    ch <= 1'b0;
                    if (!gr) begin
                        gr <= 1'b1;
                        st <= G_SEEK;
                    end else begin
                        st <= G_IDLE;
                    end
                end
            end
            default: st <= G_IDLE;
        endcase
        if (new_stream) begin
            st <= G_IDLE;
            granules <= '0;
            crc_r <= 32'hFFFFFFFF;
        end
    end
    initial granules = '0;
endmodule
