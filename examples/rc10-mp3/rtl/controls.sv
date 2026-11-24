`timescale 1ns/1ps

// (from examples/rc10-pcm-player/rtl/pcm_player.sv)
// Joystick, sampled every MS clocks; a press counts once when it has been
// seen on two samples running.
module controls #(
    parameter int MS = 48000,
    parameter logic [3:0] VOL0 = 4'd14     // volume at configuration
) (
    input  logic       clk,
    input  logic [4:0] joy,            // {select, down, up, right, left}
    output logic [3:0] vol,
    output logic       paused,
    output logic       rz              // DAC coding: 1 = return to zero
);
    logic [$clog2(MS)-1:0] div = '0;
    logic [4:0] s1 = '0, s2 = '0, held = '0;
    logic [3:0] v = VOL0;
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
