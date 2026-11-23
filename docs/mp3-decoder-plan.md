# MP3 decoder in the RC10's FPGA: plan

Step 2 of the RC10 music player. Step 1 (`examples/rc10-pcm-player`) plays
PCM decoded on the PC. Here the PC streams the MP3 file itself (16-40 KB/s)
and the XC3S1500 decodes it. The PCM player's USB front end, FIFO pacing and
return-to-zero DACs are kept.

## Scope

- **First:** MPEG-1 Layer III. That means 32, 44.1 and 48 kHz; mono, stereo,
  joint stereo (mid/side and intensity); long, short and mixed blocks; the
  bit reservoir; and CBR and VBR. This covers nearly all music MP3s.
- **Later, if wanted:** MPEG-2/2.5 low sample rates (16-24 kHz and 8-12 kHz).
  These differ in side info and scale factor coding.
- **Not planned:** free-format bitrates, which are rare.

## Decoding pipeline

A frame holds 1152 samples per channel, as 2 granules of 576. At 44.1 kHz
that is 76.6 granules per second per channel.

| # | Stage | Work per granule and channel |
|---|---|---|
| 1 | Frame sync, header, CRC skip | find `FFF`, parse 32 bits, check consistency |
| 2 | Side info (17/32 bytes) | main_data_begin, part2_3_length, big_values, global_gain, block type, table selects, region counts … |
| 3 | Bit reservoir | main data can start up to 511 bytes back, in earlier frames |
| 4 | Scale factors | slen1/slen2 from scalefac_compress; scfsi reuse; short/mixed layouts |
| 5 | Huffman | 32 big-value tables (+ linbits) and 2 count1 tables gives 576 integers |
| 6 | Requantise | `sign·|i|^(4/3)·2^((global_gain-210)/4)·2^(-(sf·mult))`, short/subblock gains, pretab |
| 7 | Stereo | mid/side: (M±S)/√2; intensity: is_pos ratio table |
| 8 | Reorder, antialias | short-block reorder; 8 butterflies at each of 31 subband boundaries |
| 9 | IMDCT | 18 to 36 (long) or 3 × 6 to 12 (short), window by block type, overlap-add, frequency inversion |
| 10 | Polyphase synthesis | per 32 subband samples: 64×32 matrixing into the V FIFO, then a 512-tap window: 32 PCM samples |

## Budgets (XC3S1500: 13,312 slices, 32 RAMB16, 32 MULT18X18, 48 MHz)

**Arithmetic.** Direct forms, with no fast transforms, at 44.1 kHz stereo:

| Stage | MACs per second |
|---|---|
| IMDCT (648 MACs per subband, long blocks) | 3.2 M |
| Synthesis (2,560 MACs per 32 samples) | 7.1 M |
| Requantise, stereo, antialias | < 0.5 M |
| **Total** | **≈ 11 M** |

One MULT18X18 does 48 M a second. So one MAC unit per stage runs at under
20% load, and simple direct forms are fine. Huffman decoding at one bit per
clock needs at most 320 k clocks per second.

**Block RAM:**

| Use | RAMB16 |
|---|---|
| MP3 byte FIFO from USB (4 KB) | 2 |
| Main data / bit reservoir (4 KB) | 2 |
| Huffman decode trees | 2 |
| `x^(4/3)` table, 0..1023 (above that: scaled lookup + interpolation) | 1 |
| Spectrum, 576 × 2 ch × 24 bit, ping-pong between stages | 4 |
| IMDCT cosine × window coefficients | 2 |
| Overlap buffers, 576 × 2 ch | 2 |
| Synthesis V FIFOs, 1024 × 2 ch × 24 bit | 4 |
| Synthesis window D[512] and matrix cosines | 2 |
| PCM FIFO to the DACs | 2 |
| **Total** | **23 of 32** |

**Multipliers:** about 10 of 32, counting two per 24×18-bit MAC and the
volume control.

**Logic:** estimated 4,000-7,000 slices (30-50%). Huffman and side-info
control is the largest part. This is a guess until the first stages are
built.

## Word lengths and accuracy

ISO/IEC 11172-4 sets two levels of decoder accuracy, against an ideal
decoder on its test streams:

- **Full accuracy:** RMS error below 2^-15/√12 of full scale, and no sample
  off by more than 2^-14.
- **Limited accuracy:** RMS error below 2^-11/√12.

Aim for full. The fixed-point model (below) decides the widths. Start from
24-bit data and 18-bit coefficients, i.e. two MULT18X18 per product, and
check whether 18-bit data (one multiplier) is enough.

## Clocks

PCM leaves the decoder at the stream's rate. 32 and 48 kHz divide 48 MHz
exactly (1500 and 1000). 44.1 kHz does not:

- **A. Two cascaded DCMs.** 48 MHz × 21/20 × 7/8 = 44.1 MHz exactly, then
  ÷1000. The DACs and the PCM FIFO's read side run on it. The risk is DCM
  jitter on the 1-bit DAC.
- **B. One DCM, ×23/25.** This gives 44.16 kHz, 0.14% fast (a 2.4-cent pitch
  shift, inaudible). FULL pacing makes the PC follow. It has less jitter
  than A.
- **C. Stay on 48 MHz.** Resample 44.1 to 48 kHz in the FPGA. This costs
  more logic and adds no risk.

The choice is made by listening early, in milestone 0: the PCM player with
the DACs on a DCM clock, playing a 44.1 kHz stream unresampled.

**Decided 2026-11-23: A.** Both A and B played music and test tones with no
audible hiss (RZ output). A is exact, so it is used. The PCM player now runs
its DACs on it (`audio_clock.sv`, dual-clock FIFO from the USB side). 32 and
48 kHz streams will need either a clock switch (BUFGMUX between the FX2's 48
MHz and the DCMs) or resampling; that is decided in milestone 6.

## Verification

1. **Golden model.** A Rust crate, `crates/mp3model`, that decodes the way
   the hardware will:
   - the same stages, fixed-point at the chosen widths;
   - every intermediate available: side info, `is[576]`, `xr[576]` after
     each stage, IMDCT output, PCM;
   - its PCM checked against ffmpeg's float decoder (`mp3float`) for the
   ISO RMS and maximum error;
   - tables (Huffman, scale factor bands, pretab, antialias, IMDCT windows)
     taken from minimp3 (public domain, CC0), cross-checked against the
     model's agreement with ffmpeg.
2. **Test streams.** Generated here with LAME and shine (two independent
   encoders). They cover:
   - mono, stereo, joint stereo, and forced mid/side;
   - 32, 44.1 and 48 kHz; 32-320 kbit/s; VBR;
   - transients that force short and mixed blocks, and heavy reservoir use;
   - real music.

   LAME doesn't use intensity stereo, so that path needs a stream from
   elsewhere (ISO conformance streams, if obtainable). Until then it is
   checked by the model only.
3. **Stage by stage, bit-exact.** Each RTL stage is fed the model's input
   vectors (`$readmemh`). Its outputs must match the model bit for bit in
   `fpgaflow sim`, over frames from every test stream.
4. **On the board.**
   - The decoder keeps a CRC32 of each stage's output per granule.
   - A USB read returns the CRCs, as the PCM player's status block does.
   - The host runs the model on the same file and names the first frame and
     stage that differ. This finds hardware-only faults without a logic
     analyser.

## Milestones (each one simulated, built, committed; board tests with your OK)

| # | Deliverable | How it's checked |
|---|---|---|
| 0 | `mp3model` (float and fixed); word lengths chosen; test streams; DCM clock listening test | model vs ffmpeg within ISO full accuracy; your ears for the clock |
| 1 | FPGA front end: USB byte FIFO, sync, header, side info, reservoir; status shows frames, bitrate, mode | sim vs model; on the board, stream a real MP3 and compare frame counts and side-info CRCs |
| 2 | Scale factors and Huffman to `is[576]` | bit-exact sim; per-granule CRCs on the board |
| 3 | Requantise, stereo, reorder, antialias | bit-exact sim; board CRCs |
| 4 | IMDCT, windows, overlap-add, frequency inversion | bit-exact sim; board CRCs |
| 5 | Polyphase synthesis into the PCM FIFO and DACs: **first music decoded on the FPGA** | bit-exact sim; PCM CRCs; listening |
| 6 | Robustness and finish: resync after errors and ID3 tags; `fpgaflow-celoxica play` sends MP3s raw; timing closure; README; optionally MPEG-2 LSF | long real-music runs with zero CRC mismatches |

## Risks

- **Huffman and side-info control.** This is the most bug-prone part. It is
  mitigated by bit-exact vectors over many streams and by board CRCs.
- **Rarely exercised paths:** intensity stereo, mixed blocks, scfsi. Find
  streams that use them, or construct them.
- **The 44.1 kHz clock and DAC noise.** This is tested in milestone 0,
  before the decoder depends on it.
- **Timing at 48 MHz on a -4 part.** Pipeline the MAC units; the load is low.
