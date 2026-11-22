# Celoxica RC10

The RC10 was Celoxica's Spartan-3 teaching and prototyping board (c. 2005).
Celoxica no longer exists and its tools only ran on Windows 2000/XP. This page
collects what fpgaflow needs to use the board on Linux, worked out from the
board itself, Celoxica's PDK 4.1 documentation and a static look at its USB
driver and host library.

| | |
|---|---|
| FPGA | Xilinx Spartan-3 XC3S1500-4FG320 (the manual says XC3S1500L; same JTAG IDCODE `0x01434093`) |
| USB | Cypress FX2 (CY7C68013-56), `107a:9005` "CTC RC10", high speed |
| Clock | 48 MHz from the FX2 on P10 (GCLK0); two expansion clocks on E10, F10 |
| Flash | 16 MB NAND behind the FX2; file 1 boots at power-up (Celoxica's demo menu) |
| fpgaflow board id | `rc10`; master UCF `data/boards/rc10.ucf` |

## Using it with fpgaflow

1. Power: USB alone runs the board. The PC drive-style 4-pin connector CN18
   (+12 V, GND, GND, +5 V) is only needed for servos, an LCD backlight or the
   5 V/12 V pins of the ATA connector.
2. USB access: `sudo cp data/udev/60-fpgaflow-celoxica.rules /etc/udev/rules.d/ && sudo udevadm control --reload-rules`,
   then replug the board.
3. `board = "rc10"` in `fpgaflow.toml`, then `fpgaflow build && fpgaflow program`.

`fpgaflow program` loads the FPGA over the board's own USB port with
`fpgaflow-celoxica` (protocol: [usb-protocol.md](usb-protocol.md)). Loads are
volatile; the reset button next to the power connector reloads file 1 from
flash. fpgaflow never writes the board's flash or FX2 firmware.

## I/O banks

From the schematic, with the two board-dependent supplies measured on our board:

| Bank | VCCO | Standard | What's on it |
|---|---|---|---|
| 0 | VccADC, 1.2 V via L5 (measured at C33) | LVCMOS12 (2/4/6 mA) | VGA colour DAC |
| 1, 2 | ATAv, jumper JP2: 1-2 = 3.3 V, 2-3 = 2.5 V (measured 3.3 V) | LVCMOS33 | expansion header, expansion clocks, joystick, LCD |
| 3 | 2.5 V | LVCMOS25 | camera, ADC control |
| 4, 5 | 3.3 V | LVCMOS33 | 48 MHz clock, PS/2, FX2, RS-232, audio, VGA sync, CAN, servos |
| 6, 7 | 2.5 V | LVCMOS25 | ADC data, LEDs, 7-segment displays |

## Pins that must never be used

These I/O pins are wired straight to a supply; driving one low shorts the
rail. Celoxica's own library lists the same pins. `fpgaflow build` refuses
any constraint that uses them and tells ISE to `PROHIBIT` them.

| Pins | Wired to |
|---|---|
| A8 C8 D8 E8 F8 | bank 0 VCCO (VccADC, 1.2 V) |
| J13 | ATAv (3.3 V via JP2) |
| E4 F5 G1 J5 J6 K5 K6 M18 | +2.5 V |

## Configuration and JTAG

- The FX2 configures the FPGA in slave SelectMAP: M2 M1 M0 = 1 1 0 (pull-ups
  on M1/M2, M0 to ground), HSWAP_EN pulled down. CCLK (T15) comes from the
  48 MHz clock through 150 Ω; PROG_B (E5) and DONE (R15) go to FX2 port pins
  P0 and P4 through 150 Ω. The SelectMAP data pins D0–D7 (T12 R12 N11 P11 U9
  V9 R7 T7) also reach the FX2.
- Bitstreams therefore start up on CCLK (`StartupClk:Cclk`); the `rc10` board
  profile sets this and Celoxica's other bitgen options.
- JTAG connector CN3, 0.1" single row, FPGA alone on the chain, 2.5 V JTAG
  (VCCAUX):

  | Pin | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
  |---|---|---|---|---|---|---|---|---|---|
  | Signal | TMS | key (cut) | TDI | TDO | n/c | TCK | +3.3 V | GND | +2.5 V (cable VREF) |

## Demo

`examples/rc10-music` plays three tunes with drums on the audio outputs, a
level meter on the LEDs and patterns on the 7-segment displays. Verified on
the board 2026-11-22: tunes, note names and display orientation correct.

## Peripherals

All pins are in `data/boards/rc10.ucf`, checked against the schematic and
Celoxica's library ([pin-crosscheck.txt](pin-crosscheck.txt),
[library-pin-lists.txt](library-pin-lists.txt)):

- 8 LEDs, active high through 150 Ω (LED0 rightmost).
- Two 7-segment digits (common cathode, segments active high); SEG0 is the
  left digit, SEG1 the right (confirmed on the board).
- 5-way joystick: each switch to ATAv with an external 11 kΩ pull-down.
- VGA (7 bits per colour, resistor DAC), PS/2 mouse and keyboard, RS-232
  (ICL3222), stereo PWM audio and piezo buzzer, CAN (SN65HVD230), 4 servo PWM
  outputs, two ADC10065 10-bit ADCs, OV9650 camera connector, LCD connector.
- 50-pin expansion header: 34 I/O (also usable as ATA), +3.3/+5/+12 V at
  0.5 A max, two clock inputs. I/O is 3.3 V maximum.
- The FX2's GPIF data bus (U13 T11 P12 R11 T8 V8 R8 R6 plus control lines)
  carries FPGA↔host data after configuration.

## Celoxica's documentation and software

The schematic, manuals and PDK are Celoxica's copyrighted material and are not
in this repository. The PDK 4.1 installer is still downloadable from the
Washington University ARL FPX tools page
(<https://www.arl.wustl.edu/projects/fpx/tools.html>, 2006):

| File | SHA-256 |
|---|---|
| `CEL-ENGDNLPDKPDK_4.1_Download-01001.exe` (PDK 4.1, 59 MB) | `1cfbed7b81a43aa9d0ef990a89d45501e3f61d3b7d9decab651d6d765cb39924` |
| `CEL-ENGDNLDKDK_4.0_SP1_Download-01000.exe` (DK 4.0 SP1, Handel-C, 19 MB) | `ac90074c715bac38f7237528e1da6be37e90596bf7ed9714aa51652502921212` |

`scripts/extract-celoxica-pdk.sh --download` fetches the PDK, checks its
SHA-256 and unpacks it (without running it) into `docs/celoxica/`, which git
ignores: the complete PDK, the RC10 schematic as a PDF and 300 dpi sheets, the
RC10 and host library manuals, Celoxica's RC10 headers, the FPGA images inside
`RC.dll`, and text extracts and disassembly used for the notes above.
