`timescale 1ns/1ps

// RC10 MP3 player: an MPEG-1 Layer III decoder in the FPGA, streamed over
// USB, played through the board's sigma-delta DACs (docs/mp3-decoder-plan.md):
// front end (milestone 1), scale factors + Huffman decoding (2), requantise,
// stereo, reorder, antialias (3), IMDCT, overlap-add, frequency inversion (4),
// polyphase synthesis to 16-bit PCM and the audio output (5).
// `fpgaflow-celoxica mp3-play FILE` plays an MP3; `mp3-check FILE` also
// compares every stage's counts and CRCs with crates/mp3model.
//
// The DACs' clock follows the stream: clock A (44.1 MHz from the 48 MHz IFCLK,
// audio_clock) for 44.1 kHz, the 48 MHz itself for 48 and 32 kHz, switched
// by a BUFGMUX; a sample every 1000 clocks (1500 for 32 kHz), so all three
// rates are exact. Each PCM word carries its rate through the FIFO, and the
// clock switches when the first word of a new rate reaches the DACs.
// Decoding is paced by the PCM FIFO:
// the synthesis waits for room, which stalls the stages before it and, at
// last, the USB bus (FULL), so the host streams at playback speed.
//
// Status (USB read, 128 bytes):
//   0-1   "M6" (the stream is in records: rtl/stream_demux.sv)
//   2-5   bytes received in this stream
//   6-9   frames found
//   10-13 bytes inside those frames
//   14-17 CRC-32 of the frames' records (see crates/mp3model/src/frontend.rs)
//   18-21 last frame header
//   22-23 frames whose main data begins before the stream
//   24    input buffer fill, 0-255
//   25    {idle, full, 6'b0}
//   26-29 granules decoded (Huffman)
//   30-33 CRC-32 of the granule records (scale factors + 576 values)
//   34-37 spectra (IMDCT inputs: 576 values per granule and channel)
//   38-41 CRC-32 of the spectra (each value as 32-bit LE)
//   42-45 subband sets (IMDCT outputs: 576 values per granule and channel)
//   46-49 CRC-32 of the subband values (32-bit LE)
//   50-53 PCM samples (per channel: a stereo pair counts 2)
//   54-57 CRC-32 of the PCM (16-bit LE, left then right)
//   58-59 underruns: times playback ran dry and then resumed
//   60    PCM FIFO fill, 0-255
//   61    {paused, rz, bass[1:0], volume[3:0]}
//   62    rate the DACs play at: 0 = 44.1, 1 = 48, 2 = 32 kHz
//   63    C3
//   64    EQ coefficient sets taken (wrapping)
//   65    other settings records taken (bass, pause, EQ off, volume; wrapping)
//   66    {5'b0, paused by the host, EQ on, EQ bank in use}
//   67    0
//   68-87 left band levels, 31 Hz..16 kHz (16-bit LE, peak envelope in sample
//         units: 32768 = full scale; rtl/eq.sv's analyser)
//   88-107 right band levels, likewise
//   108   level snapshots taken (wrapping; about 170 a second)
//   109-127 0
// Joystick (as the PCM player): up/down = volume in 3 dB steps, left/right =
// DAC coding (NRZ/RZ), right = bass boost (rtl/eq.sv: off, +4, +8, +12 dB
// with automatic headroom), centre = pause. Display: "M" (across both digits) and
// "P3" by turns, a second each; the volume for 3 s after up/down; dot = paused.
// LEDs: 0 = idle, 1 = RZ, 4-7 = input buffer fill.
module mp3_top #(
    parameter int IDLE_BITS = 20,
    parameter int AW        = 10,     // PCM FIFO: 1024 stereo words (23 ms)
    parameter int TICK      = 1000,   // DAC clocks per sample (44.1 and 48 kHz)
    parameter int TICK32    = 1500,   // (32 kHz, on the 48 MHz)
    parameter int MS        = 48000,  // clocks per joystick sample
    parameter logic [3:0] VOL0 = 4'd14 // volume at configuration (0 = mute)
) (
    input  logic       clk,        // 48 MHz IFCLK from the FX2
    input  logic       fx2_ncs,
    input  logic       fx2_nwr,
    input  logic       fx2_seln,   // 0 = FPGA, 1 = NAND flash
    inout  wire  [7:0] fx2_d,
    output logic       fx2_full,
    output logic       fx2_empty,
    output logic [7:0] led,
    output logic [7:0] seg0,
    output logic [7:0] seg1,
    input  logic [4:0] joy,        // {select, down, up, right, left}; pressed = 1
    output logic       audio_l,
    output logic       audio_r,
    output logic [1:0] piezo
);
    logic aclk, use48;
    audio_clock #(.MODE("AS")) u_aclk (.clk48(clk), .use48, .aclk);

    logic         rx_valid, idle;
    logic [7:0]   rx_data;
    logic [1023:0] status;
    logic          usb_hold;          // paused by the host (below)
    logic [319:0]  lvl_s1 = '0;       // the band levels (below)
    logic [7:0]    snaps_s1 = '0;
    fx2_bus #(.IDLE_BITS(IDLE_BITS), .SB(7)) u_bus (
        .clk, .fx2_ncs, .fx2_nwr, .fx2_seln, .fx2_d,
        .rx_valid, .rx_data, .idle, .hold(full | usb_hold), .status
    );
    assign fx2_empty = 1'b0;
    wire new_stream = rx_valid & idle;

    logic        full, rec_valid;
    logic [7:0]  rec_data, res_rdata;
    logic [31:0] frames, frame_bytes, crc, last_header;
    logic [15:0] undecodable;
    logic [12:0] fill;
    // The stream's records: MP3 bytes to the front end, settings to the
    // controls and the EQ (rtl/stream_demux.sv).
    logic        mp3_valid, cw_en, eq_on, eq_bank, set_bass, set_pause, pause_val, set_vol;
    logic [3:0]  vol_val;
    logic [7:0]  mp3_data, eq_sets, settings;
    logic [6:0]  cw_addr;
    logic [24:0] cw_data;
    logic [1:0]  bass_val;
    stream_demux u_dmx (
        .clk, .new_stream, .rx_valid, .rx_data, .mp3_valid, .mp3_data,
        .cw_en, .cw_addr, .cw_data, .eq_on, .eq_bank,
        .set_bass, .bass_val, .set_pause, .pause_val, .set_vol, .vol_val, .eq_sets, .settings
    );
    mp3_frontend u_fe (
        .clk, .in_valid(mp3_valid), .in_data(mp3_data), .new_stream, .idle, .full,
        .rec_valid, .rec_data, .frames, .frame_bytes, .undecodable, .crc,
        .last_header, .fill, .res_raddr, .res_rdata,
        .side, .fd_valid, .fd_ready
    );
    logic [11:0]  res_raddr;
    logic [357:0] side;
    logic         fd_valid, fd_ready, g_valid, s_ready, s_valid, rec_ch;
    logic [7:0]   g_data, s_data;
    logic [31:0]  granules, gcrc, rec_hdr, spectra, scrc;
    logic [73:0]  rec_ent;
    mp3_granule u_gr (
        .clk, .new_stream, .side, .fd_valid, .fd_ready, .res_raddr, .res_rdata,
        .rec_valid(g_valid), .rec_data(g_data), .rec_ready(s_ready),
        .rec_hdr, .rec_ent, .rec_ch, .granules, .crc(gcrc)
    );
    logic         i_ready, b_valid, spec_ch, spec_mono;
    logic [1:0]   spec_rate, sub_rate, pcm_rate;
    logic [7:0]   b_data;
    logic [73:0]  spec_ent;
    logic [31:0]  subs, bcrc;
    mp3_spectral u_sp (
        .clk, .new_stream, .in_valid(g_valid), .in_data(g_data), .in_hdr(rec_hdr),
        .in_ent(rec_ent), .in_ch(rec_ch), .ready(s_ready),
        .spec_valid(s_valid), .spec_data(s_data), .spec_ready(i_ready),
        .spec_ent, .spec_ch, .spec_mono, .spec_rate, .spectra, .crc(scrc)
    );
    mp3_imdct u_im (
        .clk, .new_stream, .in_valid(s_valid), .in_data(s_data), .in_ent(spec_ent),
        .in_ch(spec_ch), .in_mono(spec_mono), .in_rate(spec_rate), .ready(i_ready), .sub_valid(b_valid),
        .sub_data(b_data), .sub_ready(y_ready), .sub_ch, .sub_mono, .sub_rate,
        .subbands(subs), .crc(bcrc)
    );
    logic         y_ready, sub_ch, sub_mono, pcm_valid;
    logic [31:0]  pcm_word, nsamp, pcrc;
    logic [AW:0]  pcount;
    logic         out_room = 1'b0;
    always_ff @(posedge clk) out_room <= (AW+1)'(1 << AW) - pcount >= (AW+1)'(32);
    mp3_synth u_sy (
        .clk, .new_stream, .in_valid(b_valid), .in_data(b_data), .in_ch(sub_ch),
        .in_mono(sub_mono), .in_rate(sub_rate), .ready(y_ready), .pcm_valid, .pcm_word, .pcm_rate, .out_room,
        .samples(nsamp), .crc(pcrc)
    );

    // ---- PCM FIFO into the DAC clock (36 bits wide: no unconnected parity)
    logic        avail, re;
    logic [35:0] rdata;
    async_fifo #(.AW(AW), .W(36)) u_fifo (
        .wclk(clk), .we(pcm_valid), .wdata({2'd0, pcm_rate, pcm_word}), .count(pcount),
        .rclk(aclk), .re, .rdata, .avail
    );

    // ---- Controls, and playback events from the audio side
    logic [3:0] vol;
    logic       paused, rz;
    logic       vol_key, bass_key;
    logic [1:0] bass;
    controls #(.MS(MS), .VOL0(VOL0)) u_ctl (.clk, .joy, .vol, .paused, .rz, .vol_key, .bass, .bass_key,
                                            .set_bass, .bass_val, .set_pause, .pause_val,
                                            .set_vol, .vol_val);
    logic t_resume;                             // audio side: toggles
    logic [2:0] resume_s1 = '0;
    always_ff @(posedge clk) resume_s1 <= {resume_s1[1:0], t_resume};
    logic [15:0] underruns = '0;
    logic        started = 1'b0;      // (a stream's first samples aren't a resume)
    always_ff @(posedge clk)
        if (new_stream) begin
            underruns <= '0;
            started <= 1'b0;
        end else if (resume_s1[2] ^ resume_s1[1]) begin
            if (started) underruns <= underruns + 1'b1;
            started <= 1'b1;
        end
    wire [7:0] pfill = pcount[AW] ? 8'hFF : 8'(pcount[AW-1:0] >> (AW - 8));
    // Paused by the host (a pause record), FULL is let go so the record that
    // resumes can get in: the host sends nothing else meanwhile. The stream
    // doesn't time out either. (Paused by the joystick, FULL holds as usual.)
    assign usb_hold = pause_val & paused;
    assign fx2_full = full & ~usb_hold;

    logic [31:0] rx_bytes = '0;
    always_ff @(posedge clk)
        if (new_stream) rx_bytes <= 32'd1;
        else if (rx_valid) rx_bytes <= rx_bytes + 1'b1;

    wire [7:0] fill8 = fill[12] ? 8'hFF : fill[11:4];
    // Byte i of the status is status[8i +: 8]. Listed from byte 31 down, a
    // 32-bit value placed as it is comes out little-endian; the header is
    // reversed so its bytes come out in stream order.
    assign status = {
        152'd0, snaps_s1, lvl_s1, 8'd0, {5'd0, usb_hold, eq_on, eq_bank}, settings, eq_sets,
        8'hC3, {6'd0, arate_s2}, {paused, rz, bass, vol}, pfill, underruns, pcrc, nsamp,
        bcrc, subs, scrc, spectra, gcrc, granules, {idle, full, 6'd0}, fill8, undecodable,
        last_header[7:0], last_header[15:8], last_header[23:16], last_header[31:24],
        crc, frame_bytes, frames, rx_bytes, 8'h36, 8'h4D
    };

    // ---- Display and LEDs
    display #(.MS(MS)) u_disp (.clk, .vol_key, .vol, .bass_key, .bass, .paused, .seg0, .seg1);
    always_ff @(posedge clk) begin
        led[0] <= idle;
        led[1] <= rz;
        led[3:2] <= '0;
        for (int i = 0; i < 4; i++) led[4 + i] <= fill8 > 8'(i * 64);
    end

    // ============================== audio side ==============================
    logic [3:0] vol_s1 = '0, vol_a = '0;
    logic [1:0] bass_s1 = '0, bass_a = '0;
    logic       eqon_s1 = 1'b0, eqon_a = 1'b0, bank_s1 = 1'b0, bank_a = 1'b0;
    logic       rz_s1 = 1'b1, rz_a = 1'b1, paused_s1 = 1'b0, paused_a = 1'b0;
    always_ff @(posedge aclk) begin
        vol_s1 <= vol;       vol_a <= vol_s1;
        bass_s1 <= bass;     bass_a <= bass_s1;
        eqon_s1 <= eq_on;    eqon_a <= eqon_s1;
        bank_s1 <= eq_bank;  bank_a <= bank_s1;
        rz_s1 <= rz;         rz_a <= rz_s1;
        paused_s1 <= paused; paused_a <= paused_s1;
    end
    // Sample ticks at the current rate; a word of another rate first switches
    // the clock (use48, the BUFGMUX select) and the divider, then plays.
    localparam int DW = $clog2(TICK32);
    logic [1:0]    arate = '0;
    logic [DW-1:0] div = '0;
    logic tick = 1'b0;
    wire  [1:0] head_rate = rdata[33:32];
    wire        switch = avail && head_rate != arate;
    logic use48_q = 1'b0;                    // drives the BUFGMUX select alone
    assign use48 = use48_q;
    always_ff @(posedge aclk) begin
        tick <= div == '0 && !switch;
        div  <= (div == DW'((arate == 2'd2 ? TICK32 : TICK) - 1)) ? '0 : div + 1'b1;
        use48_q <= arate != 2'd0;
        if (switch) begin
            arate <= head_rate;
            div <= DW'(1);                       // the first tick a whole sample later
        end
    end
    logic [1:0] arate_s1 = '0, arate_s2 = '0;   // (to the 48 MHz side)
    always_ff @(posedge clk) begin
        arate_s1 <= arate;
        arate_s2 <= arate_s1;
    end
    logic signed [15:0] smp_l = '0, smp_r = '0;
    logic dry = 1'b0, t_resume_r = 1'b0;
    assign re = tick & ~paused_a & avail & ~switch;
    always_ff @(posedge aclk)
        if (re) begin
            smp_l <= rdata[15:0];
            smp_r <= rdata[31:16];
            if (dry) t_resume_r <= ~t_resume_r;
            dry <= 1'b0;
        end else if (tick & ~paused_a) begin
            dry <= 1'b1;             // (counted when samples come again)
        end
    assign t_resume = t_resume_r;
    // Bass boost (after the PCM CRC point: the decoder checks are unchanged).
    logic re_d = 1'b0;
    always_ff @(posedge aclk) re_d <= re;
    logic signed [15:0] eq_l, eq_r;
    logic [319:0] levels;
    logic [7:0]   snaps;
    eq u_eq (.clk(aclk), .start(re_d), .rate(arate), .level(bass_a), .on(eqon_a), .bank(bank_a),
             .in_l(smp_l), .in_r(smp_r), .out_l(eq_l), .out_r(eq_r), .done(), .busy(),
             .levels, .snaps, .wclk(clk), .cw_en, .cw_addr, .cw_data);
    // The levels to the 48 MHz side: they change once every 256 samples, so
    // they are copied once snaps[0] (which toggles with each snapshot) has
    // come through two flip-flops (bundled data; the copies end in _s1, so
    // the UCF's synchroniser TIG covers them).
    logic [2:0]   snapt_s1 = '0;
    always_ff @(posedge clk) begin
        snapt_s1 <= {snapt_s1[1:0], snaps[0]};
        if (snapt_s1[2] ^ snapt_s1[1]) begin
            lvl_s1 <= levels;
            snaps_s1 <= snaps;
        end
    end
    // Volume (3 dB steps, top step 0.9 of full scale) and the DACs.
    logic [7:0] gain;
    always_comb
        case (vol_a)
            4'd0:  gain = 8'd0;   4'd1:  gain = 8'd2;   4'd2:  gain = 8'd3;   4'd3:  gain = 8'd4;
            4'd4:  gain = 8'd5;   4'd5:  gain = 8'd7;   4'd6:  gain = 8'd10;  4'd7:  gain = 8'd14;
            4'd8:  gain = 8'd20;  4'd9:  gain = 8'd29;  4'd10: gain = 8'd41;  4'd11: gain = 8'd58;
            4'd12: gain = 8'd82;  4'd13: gain = 8'd115; 4'd14: gain = 8'd163; default: gain = 8'd230;
        endcase
    logic signed [25:0] prod_l = '0, prod_r = '0;
    always_ff @(posedge aclk) begin
        prod_l <= eq_l * $signed({1'b0, gain});
        prod_r <= eq_r * $signed({1'b0, gain});
    end
    // RZ swings half as far as NRZ: NRZ plays 6 dB down to match.
    wire signed [15:0] x_l = rz_a ? prod_l[23:8] : prod_l[24:9];
    wire signed [15:0] x_r = rz_a ? prod_r[23:8] : prod_r[24:9];
    sigma_delta2 u_dac_l (.clk(aclk), .rz(rz_a), .x(x_l), .y(audio_l));
    sigma_delta2 u_dac_r (.clk(aclk), .rz(rz_a), .x(x_r), .y(audio_r));
    assign piezo = 2'b00;
endmodule
