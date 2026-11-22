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
  request to `CPUCS` (`0xE600`). fpgaflow never sends `0xA0`.

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

## Embedded resources in RC.dll (`docs/celoxica/rcdll-resources/` after extraction)

- `107`: `HostControl.ncd`, `3s1500lfg320`, 2005/06/30, Celoxica's "host
  control" FPGA design for the RC10 (`RCDeviceConfigureHostControl`).
- `110`: the same for the RC300 (`2v6000ff1152`).
- `101`: 3.2 MB, starts with `FF` padding; presumably the RC250 (Altera) image.
