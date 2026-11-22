`timescale 1ns/1ps

// The band: a plucked pulse-wave melody, a triangle bass, and kick, snare
// and hi-hat, mixed into two 10-bit unsigned samples (512 = silence).
//
// Oscillators are 32-bit phase accumulators clocked at 48 MHz (frequency
// resolution 0.011 Hz). Envelopes decay exponentially on the 1 kHz tick:
// e -= e >> k gives a time constant of 2^k ms. Samples update at 48 kHz in a
// two-stage pipeline (products, then the mix).
module synth (
    input  logic       clk,
    input  logic       rst,
    input  logic       run,
    input  logic       tick_ms,
    input  logic       tick_smp,
    input  logic       note_on,
    input  logic       eighth,
    input  logic [2:0] pos,
    input  logic [5:0] mel,       // 63 = rest
    input  logic [5:0] bass,      // 63 = rest
    output logic [9:0] sample_l,
    output logic [9:0] sample_r,
    output logic [9:0] level,     // |mix|, for the meter
    output logic [7:0] mel_env,
    output logic [7:0] kick_env
);
    localparam logic [5:0]  REST       = 6'd63;
    localparam logic [31:0] KICK_START = 32'd13422;  // 150 Hz
    localparam logic [31:0] KICK_END   = 32'd4027;   // 45 Hz

    logic [31:0] mel_inc, bass_inc;
    note_table u_mel_inc  (.n(mel),  .inc(mel_inc));
    note_table u_bass_inc (.n(bass), .inc(bass_inc));

    logic [31:0] mel_ph, bass_ph, kick_ph, kick_inc;
    logic [15:0] mel_e, kick_e, snare_e, hat_e;
    logic [15:0] lfsr;

    wire kick_hit  = eighth && run && (pos == 3'd0 || pos == 3'd4);
    wire snare_hit = eighth && run && (pos == 3'd2 || pos == 3'd6);
    wire hat_hit   = eighth && run && pos[0];
    wire decay     = tick_ms && run;

    always_ff @(posedge clk) begin
        if (rst) begin
            mel_ph   <= 32'd0;
            bass_ph  <= 32'd0;
            kick_ph  <= 32'd0;
            kick_inc <= KICK_END;
            mel_e    <= 16'd0;
            kick_e   <= 16'd0;
            snare_e  <= 16'd0;
            hat_e    <= 16'd0;
            lfsr     <= 16'hace1;
        end else begin
            if (run) begin
                mel_ph  <= mel_ph + mel_inc;
                bass_ph <= bass_ph + bass_inc;
                kick_ph <= kick_ph + kick_inc;
            end
            // Galois LFSR, x^16 + x^14 + x^13 + x^11 + 1.
            if (tick_smp) lfsr <= {1'b0, lfsr[15:1]} ^ (lfsr[0] ? 16'hb400 : 16'h0000);

            if (note_on && mel != REST) mel_e <= 16'hffff;
            else if (decay) mel_e <= mel_e - (mel_e >> 7);          // 128 ms

            if (kick_hit) begin
                kick_e   <= 16'hffff;
                kick_inc <= KICK_START;
            end else if (decay) begin
                kick_e <= kick_e - (kick_e >> 5);                     // 32 ms
                if (kick_inc > KICK_END) kick_inc <= kick_inc - (kick_inc >> 4);
            end

            if (snare_hit) snare_e <= 16'hffff;
            else if (decay) snare_e <= snare_e - (snare_e >> 5);      // 32 ms

            if (hat_hit) hat_e <= 16'hffff;
            else if (decay) hat_e <= hat_e - (hat_e >> 3);            // 8 ms
        end
    end

    // Triangle from a phase: -128 .. 126.
    function automatic logic signed [8:0] tri_wave(input logic [7:0] ph);  // phase[31:24]
        logic [6:0] t;
        t = ph[7] ? ~ph[6:0] : ph[6:0];
        tri_wave = $signed({1'b0, t, 1'b0}) - 9'sd128;
    endfunction

    logic signed [8:0] mel_w, bass_w, kick_w, noise_w;
    always_comb begin
        // 25 % pulse with no DC: +96 for a quarter of the cycle, -32 otherwise.
        if (mel == REST) mel_w = 9'sd0;
        else if (mel_ph[31:30] == 2'b00) mel_w = 9'sd96;
        else mel_w = -9'sd32;
        bass_w  = (bass == REST) ? 9'sd0 : tri_wave(bass_ph[31:24]);
        kick_w  = tri_wave(kick_ph[31:24]);
        noise_w = $signed({lfsr[7], lfsr[7:0]});
    end

    // 9-bit signed x 0..255: |product| <= 32640, 17 bits signed.
    logic signed [16:0] m_mel, m_kick, m_snare, m_hat;
    always_comb begin
        m_mel   = mel_w   * $signed({1'b0, mel_e[15:8]});
        m_kick  = kick_w  * $signed({1'b0, kick_e[15:8]});
        m_snare = noise_w * $signed({1'b0, snare_e[15:8]});
        m_hat   = noise_w * $signed({1'b0, hat_e[15:8]});
    end

    // Stage 1: scaled voices. Stage 2: the mix. Stage 3: clamp and level.
    // (Samples change at 48 kHz, so the pipeline costs nothing.)
    logic signed [8:0] r_mel, r_bass, r_kick, r_snare, r_hat;
    logic smp_d, smp_d2;
    logic signed [11:0] sum_l, sum_r, mix_l, mix_r;
    logic signed [12:0] off_l, off_r;
    // Explicit sign extension (no reliance on context-sized signed arithmetic).
    function automatic logic signed [11:0] sx(input logic signed [8:0] v);
        sx = {{3{v[8]}}, v};
    endfunction
    always_comb begin
        sum_l = (sx(r_mel) <<< 1) + sx(r_bass) + sx(r_kick) + (sx(r_snare) >>> 1) + (sx(r_hat) >>> 2);
        sum_r = (sx(r_mel) <<< 1) + sx(r_bass) + sx(r_kick) + (sx(r_snare) >>> 1) + (sx(r_hat) >>> 1);
        off_l = {mix_l[11], mix_l} + 13'sd512;
        off_r = {mix_r[11], mix_r} + 13'sd512;
    end

    function automatic logic [9:0] clamp10(input logic signed [12:0] v);
        if (v < 0) clamp10 = 10'd0;
        else if (v > 13'sd1023) clamp10 = 10'd1023;
        else clamp10 = v[9:0];
    endfunction

    function automatic logic [9:0] mag10(input logic signed [11:0] v);
        logic [11:0] a;
        a = v < 0 ? 12'(-v) : 12'(v);
        mag10 = (a > 12'd1023) ? 10'd1023 : a[9:0];
    endfunction

    always_ff @(posedge clk) begin
        smp_d  <= tick_smp;
        smp_d2 <= smp_d;
        if (rst) begin
            mix_l <= 12'sd0;
            mix_r <= 12'sd0;
            r_mel <= 9'sd0; r_bass <= 9'sd0; r_kick <= 9'sd0; r_snare <= 9'sd0; r_hat <= 9'sd0;
            sample_l <= 10'd512;
            sample_r <= 10'd512;
            level    <= 10'd0;
        end else begin
            if (tick_smp) begin
                r_mel   <= 9'(m_mel >>> 8);
                r_bass  <= bass_w;
                r_kick  <= 9'(m_kick >>> 8);
                r_snare <= 9'(m_snare >>> 8);
                r_hat   <= 9'(m_hat >>> 8);
            end
            if (smp_d) begin
                mix_l <= sum_l;
                mix_r <= sum_r;
            end
            if (smp_d2) begin
                if (run) begin
                    sample_l <= clamp10(off_l);
                    sample_r <= clamp10(off_r);
                    level    <= (mag10(mix_l) > mag10(mix_r)) ? mag10(mix_l) : mag10(mix_r);
                end else begin
                    sample_l <= 10'd512;
                    sample_r <= 10'd512;
                    level    <= 10'd0;
                end
            end
        end
    end

    assign mel_env  = mel_e[15:8];
    assign kick_env = kick_e[15:8];
endmodule
