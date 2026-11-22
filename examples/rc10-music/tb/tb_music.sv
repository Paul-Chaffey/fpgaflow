`timescale 1ns/1ps

// Self-checking testbench for the RC10 music demo.
//
// Runs the real 48 MHz clock with time scaled down: an eighth note is 3000
// clocks, a "millisecond" 40 clocks and an audio sample 50 clocks. Checks the
// note sequence and timing, the pitch table against the equal-temperament
// formula (computed here independently), the oscillator, the audio and LED
// meter, the joystick controls (volume, tune, display mode) and the
// 7-segment note names and overlays.
module tb_music;
    localparam int EIGHTH = 3000;
    localparam int TICK   = 40;
    localparam int SMP    = 50;

    logic clk = 1'b0;
    always #10.4167 clk = ~clk;   // 48 MHz

    logic [4:0] joy = 5'b0;       // {select, down, up, right, left}
    logic [7:0] led, seg0, seg1;
    logic       audio_l, audio_r;
    logic [1:0] piezo;

    music_top #(.EIGHTH(EIGHTH), .TICK_DIV(TICK), .SAMPLE_DIV(SMP)) dut (
        .clk(clk), .joy(joy), .led(led), .seg0(seg0), .seg1(seg1),
        .audio_l(audio_l), .audio_r(audio_r), .piezo(piezo)
    );

    // An independent instance of the pitch table, swept by the test.
    logic [5:0]  tn = 6'd0;
    logic [31:0] tinc;
    note_table u_table (.n(tn), .inc(tinc));

    int errors = 0;
    task automatic check(input bit ok, input string what);
        if (!ok) begin
            $display("FAIL: %s (t=%0t)", what, $time);
            errors++;
        end
    endtask

    // Wait for the next note; returns its melody note and the clocks waited.
    task automatic next_note(output logic [5:0] m, output int waited);
        waited = 0;
        do begin
            @(posedge clk);
            waited++;
        end while (dut.u_seq.note_on !== 1'b1);
        m = dut.u_seq.mel;
    endtask

    // Hold a joystick contact long enough for the debouncer, then release.
    localparam int SELECT = 4, DOWN = 3, UP = 2, RIGHT = 1, LEFT = 0;
    task automatic press(input int b);
        joy[b] = 1'b1;
        repeat (8 * TICK) @(posedge clk);
        joy[b] = 1'b0;
        repeat (8 * TICK) @(posedge clk);
    endtask

    function automatic logic [7:0] letter(input int pc);
        case (pc)
            0: letter = 8'h39;  2: letter = 8'h5e;  4: letter = 8'h79;  5: letter = 8'h71;
            7: letter = 8'h3d;  9: letter = 8'h77;  11: letter = 8'h7c;
            default: letter = 8'hxx;
        endcase
    endfunction
    function automatic logic [7:0] digit(input int d);
        case (d)
            0: digit = 8'h3f;  1: digit = 8'h06;  2: digit = 8'h5b;  3: digit = 8'h4f;
            4: digit = 8'h66;  5: digit = 8'h6d;  6: digit = 8'h7d;  7: digit = 8'h07;
            8: digit = 8'h7f;  9: digit = 8'h6f;  default: digit = 8'hxx;
        endcase
    endfunction

    // Ode to Joy, first 15 notes (n = MIDI - 36) and lengths in eighths.
    localparam int ODE_N   [15] = '{40, 40, 41, 43, 43, 41, 40, 38, 36, 36, 38, 40, 40, 38, 38};
    localparam int ODE_LEN [15] = '{2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 1, 4};

    initial begin
        string wave;
        if ($value$plusargs("wave=%s", wave)) begin
            $dumpfile(wave);
            $dumpvars(0, tb_music);
        end
    end

    initial begin
        logic [5:0] m;
        int w, toggles, ones;
        logic prev;
        logic [7:0] seen;
        logic [31:0] p0, p1, inc;
        longint expected;

        // 1. Pitch table: every note against 440 * 2^((MIDI-69)/12), 48 MHz, 32 bits.
        for (int n = 0; n < 63; n++) begin
            tn = 6'(n);
            #1;
            expected = longint'($rtoi(440.0 * (2.0 ** ((n - 33) / 12.0)) * 4294967296.0 / 48.0e6 + 0.5));
            check(longint'(tinc) >= expected - 1 && longint'(tinc) <= expected + 1,
                  $sformatf("pitch table n=%0d: %0d, expected %0d", n, tinc, expected));
        end
        tn = 6'd63;
        #1 check(tinc == 32'd0, "rest has no pitch");

        // 2. Tune 0 (Ode to Joy): notes in order, each lasting its length.
        next_note(m, w);
        check(m == 6'(ODE_N[0]), $sformatf("first note %0d, expected %0d", m, ODE_N[0]));
        for (int i = 1; i < 15; i++) begin
            next_note(m, w);
            check(m == 6'(ODE_N[i]), $sformatf("note %0d is %0d, expected %0d", i, m, ODE_N[i]));
            check(w == ODE_LEN[i - 1] * EIGHTH,
                  $sformatf("note %0d came after %0d clocks, expected %0d", i, w, ODE_LEN[i - 1] * EIGHTH));
        end

        // 3. The melody oscillator advances by its table value every clock.
        next_note(m, w);
        p0  = dut.u_synth.mel_ph;
        inc = dut.u_synth.mel_inc;
        repeat (1000) @(posedge clk);
        p1 = dut.u_synth.mel_ph;
        check(p1 - p0 == 32'(1000 * longint'(inc)), "melody phase advances by inc per clock");

        // 4. Audio is active and the LED meter rises while playing.
        toggles = 0;
        seen = 8'h00;
        prev = audio_l;
        repeat (4 * EIGHTH) begin
            @(posedge clk);
            if (audio_l != prev) toggles++;
            prev = audio_l;
            seen |= led;
        end
        check(toggles > 1000, $sformatf("audio_l toggled only %0d times", toggles));
        check(seen[3], $sformatf("LED meter only reached %b", seen));

        // 5. Up: next tune (Twinkle) from the top, with "t2" shown.
        press(UP);
        check(dut.tune == 2'd1, "up selects tune 1");
        check(dut.u_seq.idx == 6'd0 && dut.u_seq.mel == 6'd36, "tune 1 starts on C5");
        check(seg0 == 8'h78 && seg1 == 8'h5b, $sformatf("overlay shows %h %h, expected t2", seg0, seg1));

        // 6. Centre: note-name mode; after the overlay, the display names the note.
        press(SELECT);
        check(dut.mode == 2'd1, "centre selects display mode 1");
        while (dut.u_seg.overlay_ms != 10'd0) @(posedge clk);
        repeat (2) begin
            next_note(m, w);
            repeat (3) @(posedge clk);
            check(seg0 == letter(int'(m) % 12) && seg1 == digit(int'(m) / 12 + 2),
                  $sformatf("note %0d shown as %h %h", m, seg0, seg1));
        end

        // 7. Left: quieter, shown as "u5"; the meter follows the volume.
        check(dut.volume == 4'd6, "volume starts at 6");
        press(LEFT);
        check(dut.volume == 4'd5, "left lowers the volume");
        check(seg0 == 8'h1c && seg1 == digit(5), $sformatf("volume overlay %h %h, expected u5", seg0, seg1));
        // Down to 0 (and not below): silence (50 % duty), meter falls to 0.
        repeat (6) press(LEFT);
        check(dut.volume == 4'd0, "volume stops at 0");
        repeat (4 * SMP) @(posedge clk);
        ones = 0;
        w = 0;
        repeat (20000) begin
            @(posedge clk);
            if (audio_l) ones++;
            if (dut.u_seq.note_on) w++;
        end
        check(ones >= 9998 && ones <= 10002, $sformatf("volume 0 audio duty %0d/20000, expected half", ones));
        check(w > 0, "the tune keeps playing at volume 0");
        repeat (1000 * TICK) @(posedge clk);
        check(led == 8'h00, $sformatf("meter still %b at volume 0", led));

        // 8. Right: louder again (and not above 8); sound and meter return.
        repeat (10) press(RIGHT);
        check(dut.volume == 4'd8, "volume stops at 8");
        seen = 8'h00;
        repeat (4 * EIGHTH) begin
            @(posedge clk);
            seen |= led;
        end
        check(seen[3], $sformatf("meter only reached %b at full volume", seen));

        // 9. Down twice: tune 1 -> 0 -> 2 (wraps), Frere Jacques starts on C5.
        press(DOWN);
        check(dut.tune == 2'd0, "down selects tune 0");
        press(DOWN);
        check(dut.tune == 2'd2, "down wraps to tune 2");
        check(dut.u_seq.mel == 6'd36, "tune 2 starts on C5");

        // 10. Centre three times from mode 1: 2, 3, then wraps to 0 (snake).
        repeat (3) press(SELECT);
        check(dut.mode == 2'd0, "centre wraps the display mode");

        check(piezo == 2'b00, "piezo held low");
        if (errors == 0) $display("PASS: tb_music");
        else $display("FAIL: tb_music, %0d error(s)", errors);
        $finish;
    end

    initial begin
        #20_000_000;
        $display("FAIL: tb_music watchdog timeout");
        $finish;
    end
endmodule
