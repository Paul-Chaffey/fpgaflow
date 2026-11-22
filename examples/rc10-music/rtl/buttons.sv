`timescale 1ns/1ps

// Joystick contacts: synchronise, debounce on the 1 kHz tick (4 stable
// samples), and give a one-cycle pulse when a contact is pressed.
module buttons (
    input  logic       clk,
    input  logic       rst,
    input  logic       tick,
    input  logic [4:0] raw,     // pressed = 1
    output logic [4:0] press
);
    genvar i;
    generate
        for (i = 0; i < 5; i = i + 1) begin : g_btn
            logic [1:0] sync;
            logic [3:0] hist;
            logic       state;
            logic       p;
            always_ff @(posedge clk) begin
                sync <= {sync[0], raw[i]};
                p <= 1'b0;
                if (rst) begin
                    hist  <= 4'h0;
                    state <= 1'b0;
                end else if (tick) begin
                    hist <= {hist[2:0], sync[1]};
                    if (hist == 4'hf && !state) begin
                        state <= 1'b1;
                        p <= 1'b1;
                    end else if (hist == 4'h0) begin
                        state <= 1'b0;
                    end
                end
            end
            assign press[i] = p;
        end
    endgenerate
endmodule
