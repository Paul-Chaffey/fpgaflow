`timescale 1ns/1ps

// RC10 PCM player: 48 kHz 16-bit stereo streamed over USB
// (`fpgaflow-celoxica play FILE`), played through 2nd-order sigma-delta DACs
// into the board's audio output.
//
// Stream: s16le, left then right. A gap of 22 ms with no bytes ends a stream:
// a partial sample is dropped and the next byte starts a new one (counters
// cleared). FULL goes high when fewer than MARGIN words are free.
//
// Status (any USB read, 16 bytes; reading doesn't disturb playback):
//   0-1   "P1"
//   2-5   bytes received in this stream
//   6-7   underruns: times playback ran dry and then resumed
//   8-9   samples missed in those underruns
//   10-11 bytes dropped because the buffer was full
//   12-13 most bytes accepted in one go after FULL went high (the FX2's latency)
//   14    buffer fill, 0-255
//   15    {idle, paused, empty, rz, volume[3:0]}
//
// Joystick: up = louder, down = quieter (16 steps of 3 dB, 0 = mute),
// left/right = DAC output coding, centre = pause. Display: volume; dot =
// paused. LEDs: 0 = NRZ, 1 = return-to-zero (see sigma_delta2), 4-7 = buffer
// fill. NRZ is played 6 dB down so both codings give the same level.
module pcm_player #(
    parameter int TICK      = 1000,   // 48 MHz / 48 kHz
    parameter int AW        = 12,     // 4096 samples = 16 KB = 85 ms
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

    // ---- Bytes to samples
    logic [1:0]  phase = '0;
    logic [23:0] part = '0;
    wire  [1:0]  ph = idle ? 2'd0 : phase;
    wire         word_done = rx_valid & (ph == 2'd3);
    wire  [31:0] word = {rx_data, part};

    logic [AW:0] count;
    logic        avail, re;
    logic [31:0] rdata;
    wire         room = count != (AW+1)'(DEPTH);
    sample_fifo #(.AW(AW)) u_fifo (
        .clk, .we(word_done & room), .wdata(word), .re, .rdata, .avail, .count
    );

    always_ff @(posedge clk)
        if (rx_valid) begin
            phase <= ph + 2'd1;
            part  <= {rx_data, part[23:8]};
        end else if (idle) begin
            phase <= '0;
        end

    logic full = 1'b0;
    always_ff @(posedge clk) full <= ((AW+1)'(DEPTH) - count) < (AW+1)'(MARGIN);
    assign fx2_full = full;

    // ---- Controls
    logic [3:0] vol;
    logic       paused, rz;
    controls #(.MS(MS)) u_ctl (.clk, .joy, .vol, .paused, .rz);

    // ---- Playback at 48 kHz
    logic [$clog2(TICK)-1:0] div = '0;
    logic tick = 1'b0;
    always_ff @(posedge clk) begin
        tick <= div == '0;
        div  <= (div == $clog2(TICK)'(TICK - 1)) ? '0 : div + 1'b1;
    end

    logic signed [15:0] smp_l = '0, smp_r = '0;
    logic        played = 1'b0;
    logic [15:0] dry = '0;
    logic [31:0] rx_bytes = '0;
    logic [15:0] underruns = '0, missed = '0, dropped = '0, late = '0, late_max = '0;
    assign re = tick & ~paused & avail;
    always_ff @(posedge clk) begin
        if (re) begin
            smp_l <= rdata[15:0];
            smp_r <= rdata[31:16];
            played <= 1'b1;
            dry <= '0;
            if (dry != '0) begin
                underruns <= underruns + 16'd1;
                missed    <= (missed + dry < missed) ? 16'hFFFF : missed + dry;
            end
        end else if (tick & ~paused & played & (dry != 16'hFFFF)) begin
            dry <= dry + 16'd1;      // ran dry; counted if more data comes
        end
        if (rx_valid) begin
            rx_bytes <= rx_bytes + 32'd1;
            if (word_done & ~room) dropped <= dropped + 16'd4;
            if (full) begin
                late <= late + 16'd1;
                if (late + 16'd1 > late_max) late_max <= late + 16'd1;
            end
        end
        if (!full) late <= '0;
        if (new_stream) begin          // a new stream: start the counts again
            rx_bytes <= 32'd1; underruns <= '0; missed <= '0; dropped <= '0;
            late_max <= '0; played <= 1'b0; dry <= '0;
        end
    end

    // ---- Volume (3 dB steps, top step 0.9 of full scale) and DACs
    logic [7:0] gain;
    always_comb
        case (vol)
            4'd0:  gain = 8'd0;   4'd1:  gain = 8'd2;   4'd2:  gain = 8'd3;   4'd3:  gain = 8'd4;
            4'd4:  gain = 8'd5;   4'd5:  gain = 8'd7;   4'd6:  gain = 8'd10;  4'd7:  gain = 8'd14;
            4'd8:  gain = 8'd20;  4'd9:  gain = 8'd29;  4'd10: gain = 8'd41;  4'd11: gain = 8'd58;
            4'd12: gain = 8'd82;  4'd13: gain = 8'd115; 4'd14: gain = 8'd163; default: gain = 8'd230;
        endcase
    logic signed [25:0] prod_l = '0, prod_r = '0;
    always_ff @(posedge clk) begin
        prod_l <= smp_l * $signed({1'b0, gain});
        prod_r <= smp_r * $signed({1'b0, gain});
    end
    // RZ swings half as far as NRZ: play NRZ 6 dB down to match.
    wire signed [15:0] x_l = rz ? prod_l[23:8] : prod_l[24:9];
    wire signed [15:0] x_r = rz ? prod_r[23:8] : prod_r[24:9];
    sigma_delta2 u_dac_l (.clk, .rz, .x(x_l), .y(audio_l));
    sigma_delta2 u_dac_r (.clk, .rz, .x(x_r), .y(audio_r));
    assign piezo = 2'b00;

    // ---- Status
    wire [7:0] fill = count[AW] ? 8'hFF : 8'(count[AW-1:0] >> (AW - 8));
    assign status = {
        {idle, paused, count == '0, rz, vol}, fill,
        late_max, dropped, missed, underruns, rx_bytes, 8'h31, 8'h50
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
