# rc10-pcm-player

Plays music on the Celoxica RC10, streamed from the PC over the board's USB
port: step 1 towards an MP3 player (the PC decodes; a later step moves the
decoding into the FPGA).

```sh
fpgaflow -C examples/rc10-pcm-player build
fpgaflow-celoxica load examples/rc10-pcm-player/build/impl/pcm_player.bit
fpgaflow-celoxica play song.mp3        # anything ffmpeg can read
```

Plug headphones or powered speakers into the RC10's audio output.

| Joystick | |
|---|---|
| up | louder (16 steps of 3 dB; the display shows the step) |
| down | quieter (0 = mute) |
| left / right | DAC output coding: return-to-zero (LED 1, default) or NRZ (LED 0) |
| centre | pause (the dot lights; the PC waits) |

LEDs 4-7 show how full the 85 ms buffer is.

## How it works

- `play` runs ffmpeg to decode the file to 48 kHz 16-bit stereo and sends it
  on USB bulk OUT (alternate setting 2; see `docs/rc10/usb-protocol.md`).
- The FX2 hands each byte to the FPGA over its GPIF bus. `fx2_bus` takes
  them (only while GPIF2 selects the FPGA rather than the NAND flash), and
  four bytes make a sample in a 4096-sample block-RAM FIFO (`sample_fifo`).
- FULL (the FX2's RDY1) goes high with 512 bytes still free, so the FPGA
  paces the PC: USB just waits.
- Every 1000 clocks (48 kHz) a sample is played: scaled by the volume (one
  MULT18X18 per channel, top step 0.9 of full scale) and fed to a
  second-order sigma-delta DAC per channel (`sigma_delta2`) into the board's
  RC filter (150 R, 0.1 uF, then 220 uF to the jack). Modelled at ~100 dB in
  the audio band with ideal edges.
- Output coding. A pin's rising and falling edges never match exactly, and
  every transition adds that mismatch. With plain NRZ at 48 MHz the number
  of transitions follows the signal, so a mismatch of only 0.2 ns limits the
  audio band to about 70 dB: audible hiss. Return-to-zero runs the modulator
  at 12 MHz and sends each bit as four 48 MHz slots, `1 s s 0`: one rising
  and one falling edge per bit whatever the data, so the mismatch becomes a
  constant offset and the model stays at ~100 dB. RZ swings half as far, so
  NRZ is played 6 dB down to make the two directly comparable (left/right on
  the joystick). Noise on the 3.3 V I/O supply reaches the output either way.
- A USB read returns a 16-byte status (bytes received, underruns, dropped
  bytes, how many bytes arrived after FULL rose, fill, volume, flags);
  `play` shows it while playing and reports it at the end.
- A 22 ms gap ends a stream: a half-received sample is dropped, so the next
  stream always starts on a sample boundary.

| Module | |
|---|---|
| `pcm_player` | top: bytes to samples, FIFO control, playback, volume, status, display |
| `fx2_bus` | the FX2 GPIF bus: bytes in, status out, idle detection |
| `sample_fifo` | 4096 x 32-bit FIFO in block RAM |
| `sigma_delta2` | 2nd-order 1-bit DAC |
| `controls` | joystick: volume and pause |

The testbench drives the player through a model of the FX2's waveforms with
a small FIFO so FULL is exercised constantly, and checks every sample played,
the status block, stream realignment after stray bytes, and the DAC's output.
