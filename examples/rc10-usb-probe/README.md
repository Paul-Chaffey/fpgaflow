# rc10-usb-probe

Measures the Celoxica RC10's USB data path: the FX2's GPIF bus between USB and
the FPGA (see `docs/rc10/usb-protocol.md`). It is the first step towards
streaming audio to the board.

- **Reads** (`fpgaflow-celoxica usb-read 512`): each byte is the number of
  48 MHz cycles since the FX2 pulled nCS low, so the dump shows exactly which
  cycles the FX2 samples. From count 128 on, the bytes cycle through a stats
  block: `A5`, bytes written (3, little-endian), sum, XOR, first, last, read
  bursts (2), write bursts (2), cycles in the last write burst (2), `5A C3`.
- **Writes** (`fpgaflow-celoxica usb-write FILE`): every cycle with nCS and
  nWR low counts as one byte. The display shows the count (low byte, hex), the
  LEDs count read bursts; the joystick centre clears both.

The design never holds FULL or EMPTY, and drives D only while the FX2 reads.

```sh
fpgaflow -C examples/rc10-usb-probe build
fpgaflow-celoxica load examples/rc10-usb-probe/build/impl/usb_probe.bit
fpgaflow-celoxica usb-read 512
head -c 1000 /dev/urandom > /tmp/x.bin && fpgaflow-celoxica usb-write /tmp/x.bin
fpgaflow-celoxica usb-read 512
```
