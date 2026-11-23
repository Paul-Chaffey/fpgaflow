# rc10-mp3

An MP3 decoder in the Celoxica RC10's FPGA, built in milestones
(`docs/mp3-decoder-plan.md`). The PC streams the MP3 file itself over USB;
the FPGA decodes it.

## Milestone 1: the front end

`mp3_frontend` turns the byte stream into MPEG-1 Layer III frames:

- Bytes go into a 4 KB ring buffer; FULL pauses the PC when it fills.
- A header counts only if the next header follows where its length says
  (or the stream ends there); otherwise the search moves on a byte. This
  resynchronises after damage and skips anything that isn't audio.
- The side information is parsed one bit a cycle; the main data is appended
  to the 4 KB bit reservoir, and each frame's main data start is worked out
  from `main_data_begin`.
- Each frame produces a 70-byte record (header, every side-info field,
  reservoir pointer; layout in `crates/mp3model/src/frontend.rs`) with a
  running CRC-32.

Checked against the golden model (`crates/mp3model`):

```sh
tools/mp3-test-streams.sh MUSIC.wav          # from the repository root
cargo build --release -p fpgaflow-mp3model
examples/rc10-mp3/tools/make-vectors.sh      # 18 streams, ~5000 frames
fpgaflow -C examples/rc10-mp3 sim            # every record must match
```

and on the board:

```sh
fpgaflow -C examples/rc10-mp3 build
fpgaflow-celoxica load examples/rc10-mp3/build/impl/mp3_top.bit
fpgaflow-celoxica mp3-check song.mp3        # frames, bytes and CRC vs the model
```

The display shows the frame count (low byte, hex); LED 0 is lit when the
stream is idle, LEDs 4-7 show the input buffer filling.
