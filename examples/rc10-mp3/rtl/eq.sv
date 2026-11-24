`timescale 1ns/1ps

// Tone control (the RC10 EQ plan, stages 1-2): per channel the bass shelf
// (section 0, built-in table, level from the joystick or USB) and, while the
// EQ is on, a 10-band octave graphic equaliser (sections 1-10, coefficients
// from the host). Bit-exact with crates/mp3model/src/eq.rs (`Equalizer`).
//
// Each section is a biquad on one 26 x 25 MAC:
//   acc = e + b0 x + b1 x1 + b2 x2 + (-a1) y1 + (-a2) y2      (Q2.23 coefs)
//   y   = sat26((acc + 2^22) >>> 23);  e = acc - ((acc + 2^22) >>> 23 << 23)
// (first-order error feedback); a section's y is the next one's x. The data
// path is the 16-bit sample << 8 with 2 bits of headroom; the output is
// sat16((y + 128) >>> 8). Section states sit in LUT RAM, {section, channel}.
//
// Coefficients: two banks of 10 x 5 in LUT RAM, written on `wclk` (the USB
// side) at {bank, 5 band + k} and read here from bank `bank`, latched with
// each sample: the host writes the bank not in use, then swaps.
//
// Stage 3, the analyser: after both channels (`done`), 10 band-passes (Q 2,
// 31 Hz..16 kHz) of the mono sum (out_l + out_r) >>> 1 run on the same MAC,
// their states in the spare slots {11 + band/2, band % 2}; per band a peak
// envelope env = max(|y|, env - env >>> 13) (about 190 ms release). Every
// 256 samples `levels` takes min(env >>> 8, 65535) per band and `snaps`
// counts up.
//
// About 10 clocks per section: about 125 per stereo sample with the EQ off,
// about 330 with it on; a sample comes every 1000 or 1500 clocks.
module eq (
    input  logic               clk,         // the DAC clock
    input  logic               start,       // a new sample pair
    input  logic [1:0]         rate,        // 0 = 44.1, 1 = 48, 2 = 32 kHz
    input  logic [1:0]         level,       // bass boost 0..3 (0 = identity)
    input  logic               on,          // graphic EQ on
    input  logic               bank,        // coefficient bank to use
    input  logic signed [15:0] in_l,
    input  logic signed [15:0] in_r,
    output logic signed [15:0] out_l = '0,
    output logic signed [15:0] out_r = '0,
    output logic               done = 1'b0,  // the outputs are ready
    output logic               busy,         // (the analyser may still run)
    // the analyser's snapshot: band b at [16 b +: 16]
    output logic [159:0]       levels = '0,
    output logic [7:0]         snaps = '0,
    // coefficient writes, on the USB side's clock
    input  logic               wclk,
    input  logic               cw_en,
    input  logic [6:0]         cw_addr,     // {bank, 5 band + k}
    input  logic signed [24:0] cw_data
);
    localparam int DW = 26, CW = 25, AW = 53;

    // ---- coefficient banks (written on wclk; read asynchronously here)
    (* ram_style = "distributed" *) logic [CW-1:0] cram [0:127];
    initial for (int i = 0; i < 128; i++) cram[i] = '0;
    always_ff @(posedge wclk) if (cw_en) cram[cw_addr] <= cw_data;

    // ---- section states, {section, channel}
    (* ram_style = "distributed" *) logic [DW-1:0] sx1 [0:31];
    (* ram_style = "distributed" *) logic [DW-1:0] sx2 [0:31];
    (* ram_style = "distributed" *) logic [DW-1:0] sy1 [0:31];
    (* ram_style = "distributed" *) logic [DW-1:0] sy2 [0:31];
    (* ram_style = "distributed" *) logic [23:0]   se  [0:31];
    initial for (int i = 0; i < 32; i++) begin
        sx1[i] = '0; sx2[i] = '0; sy1[i] = '0; sy2[i] = '0; se[i] = '0;
    end

    logic [3:0] s = '0;                          // section
    logic       ch = 1'b0;
    wire  [4:0] sa = {s, ch};
    wire signed [DW-1:0] x1 = $signed(sx1[sa]), x2 = $signed(sx2[sa]);
    wire signed [DW-1:0] y1 = $signed(sy1[sa]), y2 = $signed(sy2[sa]);
    wire signed [23:0]   ein = $signed(se[sa]);

    logic [1:0] rate_r = '0, level_r = '0;
    logic       on_r = 1'b0, bank_r = 1'b0;
    logic signed [DW-1:0] xin = '0;              // this section's input
    logic signed [15:0]   in_r_r = '0;

    // ---- coefficients for term k of section s
    logic [2:0] k = '0;
    logic [5:0] cbase = '0;                      // 5 (s - 1)
    logic signed [CW-1:0] bass_c;
    eq_bass_coef u_coef (.rate(rate_r), .level(level_r), .k, .c(bass_c));
    wire signed [CW-1:0] band_c = $signed(cram[{bank_r, cbase + 6'(k)}]);
    logic       an = 1'b0;                       // analysing
    logic [3:0] ab = '0;                         // analyser band
    logic signed [CW-1:0] bpf_c;
    eq_bpf_coef u_bpf (.rate(rate_r), .band(ab), .k, .c(bpf_c));
    wire signed [CW-1:0] coef = an ? bpf_c : s == 4'd0 ? bass_c : band_c;
    // Envelopes in LUT RAM: updated a band at a time, read out a band at a
    // time for the snapshot.
    (* ram_style = "distributed" *) logic [26:0] env [0:15];
    initial for (int i = 0; i < 16; i++) env[i] = '0;
    logic [3:0] sb = '0;                         // snapshot band
    logic [7:0] cnt = '0;                        // samples, for the snapshots

    // ---- MAC: operands registered, product registered, then accumulated
    logic                 issuing = 1'b0;
    logic signed [DW-1:0] d_r = '0;
    logic signed [CW-1:0] c_r = '0;
    logic signed [DW+CW-1:0] prod = '0;
    logic signed [AW-1:0] acc = '0;
    logic [1:0] vp = '0;
    logic       lastp = 1'b0, last1 = 1'b0, last2 = 1'b0;
    logic signed [DW-1:0] y_r = '0;

    typedef enum logic [3:0] { E_IDLE, E_SEC, E_MAC, E_WAIT, E_RND, E_OUT, E_ENV, E_SNAP } estate_t;
    estate_t st = E_IDLE;
    assign busy = st != E_IDLE;
    wire [26:0] env_rd = env[st == E_SNAP ? sb : ab];

    always_ff @(posedge clk) begin
        done <= 1'b0;
        vp <= {vp[0], issuing};
        last1 <= lastp;
        last2 <= last1;
        if (issuing) begin
            case (k)
                3'd0: d_r <= xin;
                3'd1: d_r <= x1;
                3'd2: d_r <= x2;
                3'd3: d_r <= y1;
                default: d_r <= y2;
            endcase
            c_r <= coef;
        end
        if (vp[0]) prod <= d_r * c_r;
        if (vp[1]) acc <= acc + AW'(prod);
        case (st)
            E_IDLE:
                if (start) begin
                    rate_r  <= rate;
                    level_r <= level;
                    on_r    <= on;
                    bank_r  <= bank;
                    xin     <= DW'(in_l) <<< 8;
                    in_r_r  <= in_r;
                    ch <= 1'b0;
                    s  <= '0;
                    st <= E_SEC;
                end
            E_SEC: begin                    // states of {s, ch} are on the RAM outputs
                k <= '0;
                cbase <= 6'(5 * (s - 4'd1));
                acc <= AW'(ein);
                issuing <= 1'b1;
                lastp <= 1'b0;
                st <= E_MAC;
            end
            E_MAC: begin
                k <= k + 1'b1;
                lastp <= k == 3'd4;
                if (k == 3'd4) begin
                    issuing <= 1'b0;
                    st <= E_WAIT;
                end
            end
            E_WAIT:
                if (last2) st <= E_RND;
            E_RND: begin                    // round, error feedback, new state
                logic signed [AW-1:0] full;
                logic signed [DW-1:0] y;
                full = (acc + (AW'(1) <<< 22)) >>> 23;
                y = full > AW'((1 <<< (DW - 1)) - 1) ? DW'((1 <<< (DW - 1)) - 1)
                  : full < -AW'(1 <<< (DW - 1)) ? -DW'(1 <<< (DW - 1)) : DW'(full);
                se[sa]  <= 24'(acc - (full <<< 23));
                sx2[sa] <= x1;
                sx1[sa] <= xin;
                sy2[sa] <= y1;
                sy1[sa] <= y;
                if (an) begin               // an analyser band: its envelope next
                    y_r <= y;
                    st <= E_ENV;
                end else if (on_r && s != 4'd10) begin
                    xin <= y;               // the next section's input
                    s <= s + 1'b1;
                    st <= E_SEC;
                end else begin
                    y_r <= y;
                    st <= E_OUT;
                end
            end
            E_ENV: begin                    // peak envelope of band ab
                logic [26:0] a, dec;
                a = y_r < 0 ? -27'(y_r) : 27'(y_r);   // (widen, then negate: |-2^25| fits)
                dec = env_rd - (env_rd >> 13);
                env[ab] <= a > dec ? a : dec;
                if (ab != 4'd9) begin
                    ab <= ab + 1'b1;
                    s  <= 4'(11 + ((ab + 1'b1) >> 1));
                    ch <= ~ch;
                    st <= E_SEC;
                end else begin
                    cnt <= cnt + 1'b1;
                    sb <= '0;
                    if (cnt == 8'd255) begin
                        st <= E_SNAP;           // every 256 samples
                    end else begin
                        an <= 1'b0;
                        st <= E_IDLE;
                    end
                end
            end
            E_SNAP: begin                   // the levels, a band a cycle
                levels[16 * sb +: 16] <= env_rd[26:24] != 3'd0 ? 16'hFFFF : env_rd[23:8];
                sb <= sb + 1'b1;
                if (sb == 4'd9) begin
                    snaps <= snaps + 1'b1;
                    an <= 1'b0;
                    st <= E_IDLE;
                end
            end
            E_OUT: begin                    // back to 16 bits
                logic signed [DW-8:0] o;    // (y + 128) >>> 8: 19 bits
                o = 19'(((DW+1)'(y_r) + (DW+1)'(128)) >>> 8);   // (27 bits: no wrap)
                if (o > 19'sd32767)       o = 19'sd32767;
                else if (o < -19'sd32768) o = -19'sd32768;
                if (!ch) begin
                    out_l <= 16'(o);
                    ch <= 1'b1;
                    s <= '0;
                    xin <= DW'(in_r_r) <<< 8;
                    st <= E_SEC;
                end else begin
                    logic signed [16:0] m;
                    out_r <= 16'(o);
                    done <= 1'b1;
                    // The analyser: band 0 first, on the mono sum.
                    m = (17'(out_l) + 17'(16'(o))) >>> 1;
                    xin <= DW'(m) <<< 8;
                    an <= 1'b1;
                    ab <= '0;
                    s  <= 4'd11;
                    ch <= 1'b0;
                    st <= E_SEC;
                end
            end
            default: st <= E_IDLE;
        endcase
    end
endmodule
