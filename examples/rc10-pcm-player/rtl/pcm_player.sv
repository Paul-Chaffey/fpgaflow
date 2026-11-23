`timescale 1ns/1ps

// RC10 PCM player: 16-bit stereo streamed over USB
// (`fpgaflow-celoxica play FILE`), played through 2nd-order sigma-delta DACs
// into the board's audio output.
//
// Two clocks: the FX2's 48 MHz (USB bus, FIFO writes, status, controls) and
// the DACs' clock from `audio_clock` (AUDIO_CLK: "48" = 48 MHz for 48 kHz,
// "A" = 44.1 MHz exactly, "B" = 44.16 MHz; one sample every TICK clocks).
// Samples cross in a dual-clock FIFO.
//
// Stream: s16le, left then right, at the rate in status byte 13. A gap of
// 22 ms with no bytes ends a stream: a partial sample is dropped and the next
// byte starts a new one (counters cleared). FULL goes high when fewer than
// MARGIN words are free.
//
// Status (any USB read, 16 bytes; reading doesn't disturb playback):
//   0-1   "P2"
//   2-5   bytes received in this stream
//   6-7   underruns: times playback ran dry and then resumed
//   8-9   samples missed in those underruns
//   10-11 bytes dropped because the buffer was full
//   12    most bytes accepted in one go after FULL went high (saturates)
//   13    sample rate: 0 = 48 kHz, 1 = 44.1 kHz (clock A), 2 = 44.16 kHz (clock B)
//   14    buffer fill, 0-255
//   15    {idle, paused, empty, rz, volume[3:0]}
//
// Joystick: up = louder, down = quieter (16 steps of 3 dB, 0 = mute),
// left/right = DAC output coding, centre = pause. Display: volume; dot =
// paused. LEDs: 0 = NRZ, 1 = return-to-zero (see sigma_delta2), 4-7 = buffer
// fill. NRZ is played 6 dB down so both codings give the same level.
module pcm_player #(
    parameter AUDIO_CLK = "A",
    parameter int TICK      = 1000,   // DAC clocks per sample
    parameter int AW        = 12,     // 4096 samples = 16 KB = 93 ms
    parameter int MARGIN    = 128,    // words free when FULL rises (512 bytes)
    parameter int IDLE_BITS = 20,
    parameter int MS        = 48000   // clocks per joystick sample
) (
    input  logic       clk,        // 48 MHz IFCLK from the FX2
    input  logic       fx2_ncs,    // T14, GPIF CTL2
    input  logic       fx2_nwr,    // V4,  GPIF CTL1
    input  logic       fx2_seln,   // P13, GPIF2: 0 = FPGA, 1 = flash
    inout  wire  [7:0] fx2_d,
    output logic       fx2_full,   // R14, GPIF RDY1
    output logic       fx2_empty,  // P14, GPIF RDY0
    input  logic [4:0] joy,        // {select, down, up, right, left}; pressed = 1
    output logic [7:0] led,
    output logic [7:0] seg0,       // left digit  {dp, g, f, e, d, c, b, a}, active high
    output logic [7:0] seg1,
    output logic       audio_l,
    output logic       audio_r,
    output logic [1:0] piezo
);
    localparam int DEPTH = 1 << AW;
    localparam logic [7:0] RATE = (AUDIO_CLK == "48") ? 8'd0 : (AUDIO_CLK == "A") ? 8'd1 : 8'd2;

    logic aclk;
    audio_clock #(.MODE(AUDIO_CLK)) u_aclk (.clk48(clk), .aclk);

    // =============================== 48 MHz side ===============================

    // ---- USB bus
    logic         rx_valid, idle;
    logic [7:0]   rx_data;
    logic [127:0] status;
    fx2_bus #(.IDLE_BITS(IDLE_BITS)) u_bus (
        .clk, .fx2_ncs, .fx2_nwr, .fx2_seln, .fx2_d,
        .rx_valid, .rx_data, .idle, .status
    );
    assign fx2_empty = 1'b0;   // status can always be read
    wire new_stream = rx_valid & idle;

    // ---- Bytes to samples, into the FIFO
    logic [1:0]  phase = '0;
    logic [23:0] part = '0;
    wire  [1:0]  ph = idle ? 2'd0 : phase;
    wire         word_done = rx_valid & (ph == 2'd3);
    logic [AW:0] count;
    wire         room = count != (AW+1)'(DEPTH);
    always_ff @(posedge clk)
        if (rx_valid) begin
            phase <= ph + 2'd1;
            part  <= {rx_data, part[23:8]};
        end else if (idle) begin
            phase <= '0;
        end

    logic        avail, re;
    logic [31:0] rdata;
    async_fifo #(.AW(AW), .W(32)) u_fifo (
        .wclk(clk), .we(word_done & room), .wdata({rx_data, part}), .count,
        .rclk(aclk), .re, .rdata, .avail
    );

    logic full = 1'b0;
    always_ff @(posedge clk) full <= ((AW+1)'(DEPTH) - count) < (AW+1)'(MARGIN);
    assign fx2_full = full;

    // ---- Controls
    logic [3:0] vol;
    logic       paused, rz;
    controls #(.MS(MS)) u_ctl (.clk, .joy, .vol, .paused, .rz);

    // ---- Events from the audio side (toggles, synchronised here)
    logic t_dry, t_resume;                  // audio side
    logic [2:0] dry_s1 = '0, resume_s1 = '0;  // [0] is the first stage
    always_ff @(posedge clk) begin
        dry_s1    <= {dry_s1[1:0], t_dry};
        resume_s1 <= {resume_s1[1:0], t_resume};
    end
    wire ev_dry    = dry_s1[2] ^ dry_s1[1];
    wire ev_resume = resume_s1[2] ^ resume_s1[1];

    // ---- Counters
    logic [31:0] rx_bytes = '0;
    logic [15:0] underruns = '0, missed = '0, pending = '0, dropped = '0;
    logic [7:0]  late = '0, late_max = '0;
    logic        t_new = 1'b0;              // tells the audio side a stream began
    always_ff @(posedge clk) begin
        if (rx_valid) begin
            rx_bytes <= rx_bytes + 32'd1;
            if (word_done & ~room) dropped <= dropped + 16'd4;
            if (full && late != 8'hFF) begin
                late <= late + 8'd1;
                if (late + 8'd1 > late_max) late_max <= late + 8'd1;
            end
        end
        if (!full) late <= '0;
        if (ev_dry && pending != 16'hFFFF) pending <= pending + 16'd1;
        if (ev_resume) begin
            underruns <= underruns + 16'd1;
            missed    <= (missed + pending < missed) ? 16'hFFFF : missed + pending;
            pending   <= '0;
        end
        if (new_stream) begin
            rx_bytes <= 32'd1; underruns <= '0; missed <= '0; pending <= '0;
            dropped <= '0; late_max <= '0;
            t_new <= ~t_new;
        end
    end

    // ---- Status
    wire [7:0] fill = count[AW] ? 8'hFF : 8'(count[AW-1:0] >> (AW - 8));
    assign status = {
        {idle, paused, count == '0, rz, vol}, fill, RATE, late_max,
        dropped, missed, underruns, rx_bytes, 8'h32, 8'h50
    };

    // ---- Display and LEDs
    function automatic logic [7:0] digit(input logic [3:0] v);
        case (v)
            4'd0: digit = 8'h3F; 4'd1: digit = 8'h06; 4'd2: digit = 8'h5B; 4'd3: digit = 8'h4F;
            4'd4: digit = 8'h66; 4'd5: digit = 8'h6D; 4'd6: digit = 8'h7D; 4'd7: digit = 8'h07;
            4'd8: digit = 8'h7F; default: digit = 8'h6F;
        endcase
    endfunction
    always_ff @(posedge clk) begin
        seg0 <= (vol >= 4'd10) ? digit(4'd1) : 8'h00;
        seg1 <= digit((vol >= 4'd10) ? vol - 4'd10 : vol) | {paused, 7'b0};
        led[0] <= ~rz;
        led[1] <= rz;
        led[3:2] <= 2'b00;
        for (int i = 0; i < 4; i++) led[4 + i] <= fill > 8'(i * 64);
    end

    // ============================== audio side ==============================

    // Controls change rarely: two flip-flops each.
    logic [3:0] vol_s1 = '0, vol_a = '0;
    logic       rz_s1 = 1'b1, rz_a = 1'b1, paused_s1 = 1'b0, paused_a = 1'b0;
    logic [2:0] new_s1 = '0;
    always_ff @(posedge aclk) begin
        vol_s1 <= vol;       vol_a <= vol_s1;
        rz_s1 <= rz;         rz_a <= rz_s1;
        paused_s1 <= paused; paused_a <= paused_s1;
        new_s1 <= {new_s1[1:0], t_new};
    end
    wire a_new = new_s1[2] ^ new_s1[1];

    logic [$clog2(TICK)-1:0] div = '0;
    logic tick = 1'b0;
    always_ff @(posedge aclk) begin
        tick <= div == '0;
        div  <= (div == $clog2(TICK)'(TICK - 1)) ? '0 : div + 1'b1;
    end

    logic signed [15:0] smp_l = '0, smp_r = '0;
    logic played = 1'b0, was_dry = 1'b0;
    logic t_dry_r = 1'b0, t_resume_r = 1'b0;
    assign re = tick & ~paused_a & avail;
    always_ff @(posedge aclk) begin
        if (re) begin
            smp_l  <= rdata[15:0];
            smp_r  <= rdata[31:16];
            played <= 1'b1;
            if (was_dry) t_resume_r <= ~t_resume_r;
            was_dry <= 1'b0;
        end else if (tick & ~paused_a & played) begin
            t_dry_r <= ~t_dry_r;     // ran dry; counted if more data comes
            was_dry <= 1'b1;
        end
        if (a_new) begin
            played  <= 1'b0;
            was_dry <= 1'b0;
        end
    end
    assign t_dry = t_dry_r;
    assign t_resume = t_resume_r;

    // ---- Volume (3 dB steps, top step 0.9 of full scale) and DACs
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
        prod_l <= smp_l * $signed({1'b0, gain});
        prod_r <= smp_r * $signed({1'b0, gain});
    end
    // RZ swings half as far as NRZ: play NRZ 6 dB down to match.
    wire signed [15:0] x_l = rz_a ? prod_l[23:8] : prod_l[24:9];
    wire signed [15:0] x_r = rz_a ? prod_r[23:8] : prod_r[24:9];
    sigma_delta2 u_dac_l (.clk(aclk), .rz(rz_a), .x(x_l), .y(audio_l));
    sigma_delta2 u_dac_r (.clk(aclk), .rz(rz_a), .x(x_r), .y(audio_r));
    assign piezo = 2'b00;
endmodule

// Joystick, sampled every MS clocks; a press counts once when it has been
// seen on two samples running.
module controls #(
    parameter int MS = 48000
) (
    input  logic       clk,
    input  logic [4:0] joy,            // {select, down, up, right, left}
    output logic [3:0] vol,
    output logic       paused,
    output logic       rz              // DAC coding: 1 = return to zero
);
    logic [$clog2(MS)-1:0] div = '0;
    logic [4:0] s1 = '0, s2 = '0, held = '0;
    logic [3:0] v = 4'd14;
    logic       p = 1'b0, z = 1'b1;
    wire  [4:0] press = s1 & s2 & ~held;
    always_ff @(posedge clk) begin
        div <= (div == $clog2(MS)'(MS - 1)) ? '0 : div + 1'b1;
        if (div == '0) begin
            s1 <= joy;
            s2 <= s1;
            held <= s1 & s2;
            if (press[2] && v != 4'd15) v <= v + 4'd1;
            if (press[3] && v != 4'd0)  v <= v - 4'd1;
            if (press[1] | press[0]) z <= ~z;
            if (press[4]) p <= ~p;
        end
    end
    assign vol = v;
    assign paused = p;
    assign rz = z;
endmodule
