# Celoxica RC10 USB protocol (from PDK 4.1)

Read by static disassembly of Celoxica's host library `RC.dll` and USB driver
`celoxica_rcusb.sys` from the PDK 4.1 installer (nothing was run; see
[README.md](README.md) for getting the PDK and `scripts/extract-celoxica-pdk.sh`).
Addresses below refer to those files.
The FPGA-configuration part was verified on an RC10 on 2026-11-22 and is
implemented in `crates/celoxica` (`fpgaflow-celoxica`).

## Device

- USB `107a:9005` "CTC RC10" (Cambridge Technology Consultants), high speed,
  firmware `bcdDevice 0.11`, held in the board's EEPROM (no firmware file ships in the PDK).
- Other PIDs in `celoxica_rcusb.inf`: 9000 RCxxx unprogrammed, 9001 RCHID,
  9002 RC300, 9003 RC250, 9004 ET1D, 9005 RC10.
- One interface, three alternate settings, each with bulk EP `0x02` OUT and
  EP `0x86` IN (512 bytes): the Cypress FX2 (CY7C68013-56) slave-FIFO layout.
- The driver can also download FX2 firmware from
  `\SystemRoot\system32\drivers\celoxica_%04x.bin` with the FX2 `0xA0`
  request to `CPUCS` (`0xE600`). fpgaflow itself never sends `0xA0`; the
  firmware was read once for this analysis (see "FX2 firmware" below).

## Layers

`RC.dll` → `DeviceIoControl` / `ReadFile` / `WriteFile` → `celoxica_rcusb.sys` → USB.

- `WriteFile` → bulk OUT EP `0x02`, split into 256 KiB URBs (RC.dll writes up to 4 MiB per call).
- `ReadFile` → bulk IN EP `0x86`.
- Command IOCTLs → USB vendor requests, queued through the driver's
  second StartIo (`URB_FUNCTION_VENDOR_DEVICE`, `wIndex 0`).

## IOCTLs (driver dispatch at 0x114b8)

| IOCTL | RC.dll use | USB transfer |
|---|---|---|
| `0x226200` | DONE status (polled 10 x 10 ms) | vendor IN `bRequest 0x40`, 1 byte; bit 7 = DONE |
| `0x22a004` | set mode (4-byte input 0..2) | `SET_INTERFACE` 0, alt = {0→0, 1→2, 2→1}; no-op if unchanged |
| `0x22a00b` | assert nPROG | vendor OUT `bRequest 0x41`, no data; only allowed in mode 0 |
| `0x226214` | | returns a 16-bit driver field (firmware version) |
| `0x226218` | | returns VID, PID |
| `0x22a010`, `0x22a014` | | vendor OUT `0x43`, 10-byte packet, command `0x0c` / `0x0b` |
| `0x22a020` | | vendor OUT `0x43`, command `0x03` with 8-byte input |
| `0x22a024` | | vendor OUT `0x43`, command `0x02` |
| `0x22602c` etc. | flash, clocks, RAM | vendor OUT `0x43`, commands `0x01`, `0x04`, `0x05`, `0x23` (20-byte) … |
| (internal) | reset | vendor OUT `0x42`, no data |

`0x43` command packets are 10 or 20 bytes: byte 0 = command, then big-endian
fields copied from the IOCTL input. These drive the NAND flash file system,
the clocks and on-board RAM. They are only partly decoded and fpgaflow never
sends them, so the board's flash (Celoxica's demos, file 1 = boot image) is
never touched.

## Configuring the FPGA (`RCDeviceConfigureFromImage`, RC.dll 0x10005f00)

1. Set mode 0 → `SET_INTERFACE` 0 alt 0.
2. Assert nPROG → vendor OUT `0x41`.
3. Write the image → bulk OUT EP `0x02`. `RCImageLoad` reads the whole `.bit`
   file unchanged (header included); the FPGA ignores bytes before the sync
   word `AA 99 55 66`.
4. Check DONE → vendor IN `0x40`, bit 7, polled.

Status byte seen on the board: `0xE8` with Celoxica's demo loaded, `0x68`
after nPROG, `0xA8` after loading blinky.

The FX2 drives SelectMAP (M2 M1 M0 = 1 1 0) with CCLK from the 48 MHz clock,
so bitstreams must use `StartupClk:Cclk`. Celoxica's bitgen options (RC10
manual 2.9): LCK_cycle 5, Match_cycle Auto, DONE_cycle 6, GTS_cycle 4,
GWE_cycle Done, DonePin PullUp, M0Pin PullDown, M1Pin/M2Pin PullUp,
CclkPin/ProgPin PullNone, DriveDone Yes. fpgaflow uses these but keeps
LCK_cycle NoWait, since 5 hangs start-up in designs without a DCM.

## Streaming to and from the FPGA design (`RCUSBWrite` / `RCUSBRead`)

RC.dll's "mode 1" is alternate setting 2.

- `RCUSBWrite` → `WriteFile` → bulk OUT EP `0x02`.
- `RCUSBRead` (RC.dll 0x100087f0) → for each chunk of at most `0xFE00` bytes:
  IOCTL `0x22600c` with the length, which the driver sends as vendor OUT `0x43`
  `[0A, len_hi, len_lo, 0 ×7]`; then `ReadFile` → bulk IN EP `0x86`.

`fpgaflow-celoxica usb-write FILE` and `usb-read N [OUT]` do the same.

## FX2 firmware

Read on 2026-11-23 from a running RC10: `0xA0` write `CPUCS = 01` (8051 held
in reset), `0xA0` read of the 16 KB RAM (twice, identical), `CPUCS = 00`. The
board re-enumerated and worked normally. `0xA0` reads with the CPU running
return garbage (they collide with instruction fetches). The image is Keil C,
saved as `docs/celoxica/rc10/fx2-firmware/fx2-ram.bin` (git-ignored: it is
Celoxica's).

- Vendor requests: `0x40` status, `0x41` nPROG, `0x42` re-initialise, `0x43`
  command (10 bytes copied to internal RAM `0x39..0x42`). Commands `0x00..0x0C`
  dispatch through a Keil switch table at `0x01BA`; `0x0A` (read from the FPGA)
  is handled in the main loop.
- Endpoints (`0x1308`): EP2 bulk OUT 512 ×2, EP6 bulk IN 512 ×2 (`EP6FIFOCFG`
  ZEROLENIN), manual (not auto) FIFO commits.
- `GpifInit` (`0x0CAD`): `IFCONFIG = 0xEA` (internal 48 MHz, **IFCLK driven
  out**: that is the FPGA's 48 MHz clock on P10; GPIF master; async),
  `GPIFREADYCFG = 0xE0`, `GPIFCTLCFG = 0` (CMOS), `GPIFIDLECTL = 0x07` (CTL0-2
  idle high), `GPIFWFSELECT = 0xB1` (0 = FIFO write, 1 = FIFO read, 2 =
  single write, 3 = single read), waveforms copied from code `0x0C2D`.
- Main loop (`0x0570`): alternate setting 0 → FIFO write with INTRDY clear
  (configuration); setting 2 → FIFO write with INTRDY set (FPGA data) when
  RDY1 is low and EP2 holds a packet; setting 1 → flash commands only.

### Bus signals

| FX2 | net | FPGA | role |
|---|---|---|---|
| FD[7:0] | D[7:0] | U13 T11 P12 R11 T8 V8 R8 R6 | data (also the NAND flash bus) |
| CTL2 | GPIF0 | T14 | nCS: FPGA data strobe |
| CTL1 | P3 | V4 | nWR: low = FX2 writes; also the flash strobe decoder (74LV139 A1) |
| CTL0 | P2 | (SelectMAP CS_B) | configuration chip select |
| RDY0 | FF0 | P14 | "EMPTY": FPGA has nothing to send |
| RDY1 | FF1 | R14 | "FULL": FPGA can't take more |
| IFCLK | CLK48 | P10 | 48 MHz, driven by the FX2 |

### Waveforms (CTL2 CTL1 CTL0; branch byte bits 5:3 = state if true, 2:0 = if false)

FIFO write, data path (INTRDY set): S3 `101` stop if RDY1; S4 `001` byte
on D, stop if FIFO flag or RDY1; S5 `001` one byte per IFCLK (DATA + NEXT,
re-executed) until the EP2 flag or RDY1; S6 `111` NEXT; S7 idle. With INTRDY
clear the same waveform takes S1/S2 with `100`: CS_B and RDWR_B low, i.e.
SelectMAP configuration at 48 MHz.

FIFO read: S0 done if RDY0; S1 `011` for 3 IFCLK; S2 `011` decision; S3
`011` DATA (one sample per IFCLK, re-executed) until RDY0 or the EP6 flag
(the programmable flag is set to the requested count); S6 `111` DATA + NEXT.

Single write / single read (`101`: CTL1 low only) strobe the NAND flash
through the 74LV139; nCS stays high, so the FPGA is not involved.

Measured with `examples/rc10-usb-probe` on 2026-11-23:

- Writes: every IFCLK rising edge with nCS and nWR low (as registered in the
  FPGA) carries exactly one byte. 1000 bytes arrived as one burst of 1000
  consecutive cycles across two USB packets; 16 MB arrived with count, sum and
  XOR correct, at 36.4 MB/s.
- Reads: the FX2 samples one byte per IFCLK, the first one being the byte the
  FPGA registers on the second edge after it sees nCS low (counting from that
  edge as 0: bytes 0, 1, 2 … with no gaps); one burst per 512-byte packet.
  8 MB read at 22.2 MB/s.
- Not yet measured: how many bytes still arrive after the FPGA raises FULL,
  and how a burst ends when it raises EMPTY.

## Embedded resources in RC.dll (`docs/celoxica/rcdll-resources/` after extraction)

- `107`: `HostControl.ncd`, `3s1500lfg320`, 2005/06/30, Celoxica's "host
  control" FPGA design for the RC10 (`RCDeviceConfigureHostControl`).
- `110`: the same for the RC300 (`2v6000ff1152`).
- `101`: 3.2 MB, starts with `FF` padding; presumably the RC250 (Altera) image.
