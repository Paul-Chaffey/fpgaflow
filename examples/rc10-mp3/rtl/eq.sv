`timescale 1ns/1ps

// Bass boost (stage 1 of the RC10 EQ plan): a low-shelf biquad per channel
// with automatic headroom, bit-exact with crates/mp3model/src/eq.rs.
//
// On `start` it takes a stereo sample and the settings, then for each
// channel runs one 26 x 25 MAC over the five terms of
//   acc = e + b0 x + b1 x1 + b2 x2 + (-a1) y1 + (-a2) y2      (Q2.23 coefs)
//   y   = sat26((acc + 2^22) >>> 23);  e = acc - ((acc + 2^22) >>> 23 << 23)
// (first-order error feedback) and outputs sat16((y + 128) >>> 8), the data
// path being the 16-bit sample << 8 with 2 bits of headroom. About 20 clocks
// per stereo sample; one sample arrives every 1000 or 1500.
module eq (
    input  logic               clk,
    input  logic               start,       // a new sample pair
    input  logic [1:0]         rate,        // 0 = 44.1, 1 = 48, 2 = 32 kHz
    input  logic [1:0]         level,       // bass boost 0..3 (0 = identity)
    input  logic signed [15:0] in_l,
    input  logic signed [15:0] in_r,
    output logic signed [15:0] out_l = '0,
    output logic signed [15:0] out_r = '0,
    output logic               done = 1'b0
);
    localparam int DW = 26, CW = 25, AW = 53;

    // Per channel state.
    logic signed [DW-1:0] x1 [0:1], x2 [0:1], y1 [0:1], y2 [0:1];
    logic signed [23:0]   e  [0:1];             // |e| <= 2^22
    initial for (int i = 0; i < 2; i++) begin
        x1[i] = '0; x2[i] = '0; y1[i] = '0; y2[i] = '0; e[i] = '0;
    end

    logic [1:0] rate_r = '0, level_r = '0;
    logic signed [DW-1:0] xin [0:1];
    initial begin xin[0] = '0; xin[1] = '0; end

    // Coefficient ROM, term k.
    logic [2:0] k = '0;
    logic signed [CW-1:0] coef;
    eq_bass_coef u_coef (.rate(rate_r), .level(level_r), .k, .c(coef));

    // MAC: operands registered, product registered, then accumulated.
    logic                 ch = 1'b0;
    logic                 issuing = 1'b0;
    logic signed [DW-1:0] d_r = '0;
    logic signed [CW-1:0] c_r = '0;
    logic signed [DW+CW-1:0] prod = '0;
    logic signed [AW-1:0] acc = '0;
    logic [2:0] vp = '0;                        // valid down the pipe
    logic       lastp = 1'b0, last1 = 1'b0, last2 = 1'b0;

    function automatic logic signed [DW-1:0] operand(input logic [2:0] t, input logic c);
        case (t)
            3'd0: operand = xin[c];
            3'd1: operand = x1[c];
            3'd2: operand = x2[c];
            3'd3: operand = y1[c];
            default: operand = y2[c];
        endcase
    endfunction

    typedef enum logic [2:0] { E_IDLE, E_MAC, E_WAIT, E_RND, E_OUT } estate_t;
    logic signed [DW-1:0] y_r = '0;             // the rounded, saturated sum
    estate_t st = E_IDLE;

    always_ff @(posedge clk) begin
        done <= 1'b0;
        // Pipeline: [issue] -> d_r/c_r -> prod -> acc.
        vp <= {vp[1:0], issuing};
        last1 <= lastp;
        last2 <= last1;
        if (issuing) begin
            d_r <= operand(k, ch);
            c_r <= coef;
        end
        if (vp[0]) prod <= d_r * c_r;
        if (vp[1]) acc <= acc + AW'(prod);
        case (st)
            E_IDLE:
                if (start) begin
                    rate_r  <= rate;
                    level_r <= level;
                    xin[0]  <= DW'(in_l) <<< 8;
                    xin[1]  <= DW'(in_r) <<< 8;
                    ch <= 1'b0;
                    k  <= '0;
                    acc <= AW'(e[0]);
                    issuing <= 1'b1;
                    lastp <= 1'b0;
                    st <= E_MAC;
                end
            E_MAC: begin                    // terms 0..4 of channel ch
                k <= k + 1'b1;
                lastp <= k == 3'd4;
                if (k == 3'd4) begin
                    issuing <= 1'b0;
                    st <= E_WAIT;
                end
            end
            E_WAIT:                         // the last product reaches acc
                if (last2) st <= E_RND;
            E_RND: begin                    // round, error feedback, new state
                logic signed [AW-1:0] full;
                logic signed [DW-1:0] y;
                full = (acc + (AW'(1) <<< 22)) >>> 23;
                y = full > AW'((1 <<< (DW - 1)) - 1) ? DW'((1 <<< (DW - 1)) - 1)
                  : full < -AW'(1 <<< (DW - 1)) ? -DW'(1 <<< (DW - 1)) : DW'(full);
                e[ch]  <= 24'(acc - (full <<< 23));
                x2[ch] <= x1[ch];
                x1[ch] <= xin[ch];
                y2[ch] <= y1[ch];
                y1[ch] <= y;
                y_r <= y;
                st <= E_OUT;
            end
            E_OUT: begin                    // back to 16 bits
                logic signed [DW-8:0] o;    // (y + 128) >>> 8: 19 bits
                o = 19'(((DW+1)'(y_r) + (DW+1)'(128)) >>> 8);   // (27 bits: no wrap)
                if (o > 19'sd32767)       o = 19'sd32767;
                else if (o < -19'sd32768) o = -19'sd32768;
                if (!ch) out_l <= 16'(o); else out_r <= 16'(o);
                if (!ch) begin
                    ch <= 1'b1;
                    k <= '0;
                    acc <= AW'(e[1]);
                    issuing <= 1'b1;
                    lastp <= 1'b0;
                    st <= E_MAC;
                end else begin
                    done <= 1'b1;
                    st <= E_IDLE;
                end
            end
            default: st <= E_IDLE;
        endcase
    end
endmodule
