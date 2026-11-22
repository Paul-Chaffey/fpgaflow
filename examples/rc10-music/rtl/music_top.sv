`timescale 1ns/1ps

// RC10 music demo.
//
// Plays three public-domain tunes (Ode to Joy, Twinkle Twinkle Little Star,
// Frere Jacques) with melody, bass and drums on the stereo audio outputs.
// The eight LEDs are a level meter and the 7-segment displays show patterns.
//
// Joystick: left/right = previous/next tune, up/down = display mode
// (snake, note name, VU columns, sparkle), centre = pause/play.
module music_top #(
    parameter int unsigned EIGHTH     = 11_250_000, // clocks per eighth note: 128 BPM at 48 MHz
    parameter int unsigned TICK_DIV   = 48_000,     // 1 kHz control tick
    parameter int unsigned SAMPLE_DIV = 1_000       // 48 kHz audio samples
) (
    input  logic       clk,       // 48 MHz from the FX2
    input  logic [4:0] joy,       // {select, down, up, right, left}; pressed = 1
    output logic [7:0] led,       // level meter, led[0] (rightmost) lowest
    output logic [7:0] seg0,      // left digit  {dp, g, f, e, d, c, b, a}
    output logic [7:0] seg1,      // right digit
    output logic       audio_l,   // 1-bit sigma-delta into the board's RC filter
    output logic       audio_r,
    output logic [1:0] piezo      // not used; held low
);
    // Power-on reset: the flip-flops start at their INIT values.
    /* verilator lint_off PROCASSINIT */
    logic [3:0] por = 4'd0;
    /* verilator lint_on PROCASSINIT */
    always_ff @(posedge clk) if (por != 4'hf) por <= por + 4'd1;
    wire rst = (por != 4'hf);

    logic tick_ms, tick_smp;
    divider #(.DIV(TICK_DIV))   u_ms  (.clk(clk), .rst(rst), .tick(tick_ms));
    divider #(.DIV(SAMPLE_DIV)) u_smp (.clk(clk), .rst(rst), .tick(tick_smp));

    // Controls.
    logic [4:0] press;
    buttons u_btn (.clk(clk), .rst(rst), .tick(tick_ms), .raw(joy), .press(press));

    logic [1:0] tune, mode;
    logic       run;
    wire        tune_change = press[0] | press[1];
    always_ff @(posedge clk) begin
        if (rst) begin
            tune <= 2'd0;
            mode <= 2'd0;
            run  <= 1'b1;
        end else begin
            if (press[1]) tune <= (tune == 2'd2) ? 2'd0 : tune + 2'd1;
            else if (press[0]) tune <= (tune == 2'd0) ? 2'd2 : tune - 2'd1;
            if (press[2]) mode <= mode + 2'd1;
            else if (press[3]) mode <= mode - 2'd1;
            if (press[4]) run <= !run;
        end
    end

    // Music.
    logic [5:0] mel, bass;
    logic       note_on, eighth;
    logic [2:0] pos;
    sequencer #(.EIGHTH(EIGHTH)) u_seq (
        .clk(clk), .rst(rst), .run(run), .tune(tune), .restart(tune_change),
        .mel(mel), .bass(bass), .note_on(note_on), .eighth(eighth), .pos(pos)
    );

    logic [9:0] sample_l, sample_r, level;
    logic [7:0] mel_env, kick_env;
    synth u_synth (
        .clk(clk), .rst(rst), .run(run), .tick_ms(tick_ms), .tick_smp(tick_smp),
        .note_on(note_on), .eighth(eighth), .pos(pos), .mel(mel), .bass(bass),
        .sample_l(sample_l), .sample_r(sample_r), .level(level),
        .mel_env(mel_env), .kick_env(kick_env)
    );

    sigma_delta u_dac_l (.clk(clk), .rst(rst), .x(sample_l), .y(audio_l));
    sigma_delta u_dac_r (.clk(clk), .rst(rst), .x(sample_r), .y(audio_r));

    // Lights.
    level_meter u_meter (
        .clk(clk), .rst(rst), .tick_ms(tick_ms), .smp(tick_smp), .level(level), .led(led)
    );
    segments u_seg (
        .clk(clk), .rst(rst), .tick_ms(tick_ms), .eighth(eighth), .pos(pos),
        .mode(mode), .run(run), .tune(tune), .tune_change(tune_change),
        .mel(mel), .mel_env(mel_env), .kick_env(kick_env), .seg0(seg0), .seg1(seg1)
    );

    assign piezo = 2'b00;
endmodule
