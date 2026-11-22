# fpgaflow

A complete FPGA design flow with a GUI, written in Rust, for Linux.

```
Verilog / SystemVerilog ─▶ Icarus | Verilator ─▶ GTKWave
                        └▶ Yosys ─▶ VPR | nextpnr | vendor P&R ─▶ bitstream ─▶ openFPGALoader
```

- **Front end:** Verilog and SystemVerilog only.
- **Simulation:** Icarus Verilog or Verilator, selectable per project. Waveforms
  open in GTKWave.
- **Synthesis:** Yosys.
- **Place & route:**
  - VTR/VPR where it can make real bitstreams (Xilinx 7-series via F4PGA,
    QuickLogic), and for generic VTR architectures.
  - nextpnr for Lattice iCE40, ECP5 and Nexus, Gowin, Cyclone V and Xilinx
    7-series (openXC7).
  - Vendor tools (ISE, Vivado, Quartus, ...) as optional adapters for parts with
    no open bitstream flow, e.g. the Spartan-3AN through ISE 14.7.
- **Programming:** openFPGALoader. The GUI detects dev boards as you plug them in.
- **Editor:** your choice: Kate (default), KWrite, VS Code, gVim, Emacs, or a
  custom command. The same editor opens HDL sources, `fpgaflow.toml`,
  constraint files and the user config.
- **Claude integration:** generate RTL and self-checking testbenches, fix
  errors, and write constraints. It works through the Anthropic API or the
  Claude Code CLI.

## Status

Early development. Working today, from the CLI:

| Command | What it does |
|---|---|
| `fpgaflow doctor` | find every tool, show versions, check USB/udev and lz4 setup |
| `fpgaflow init <dir>` | new project skeleton |
| `fpgaflow lint` | Verilator `--lint-only -Wall` (Icarus fallback) |
| `fpgaflow sim [--tool icarus\|verilator] [--waves]` | compile and run the self-checking testbench, PASS/FAIL verdict, optional GTKWave |
| `fpgaflow waves` | open the last waveform in GTKWave with a saved or generated layout |
| `fpgaflow hier` | Yosys elaboration: module hierarchy and port lists |
| `fpgaflow synth [--family F] [--backend B]` | Yosys synthesis for the target; utilisation by resource type; netlist in the backend's format |
| `fpgaflow families` | the 24 supported families and their P&R backends |
| `fpgaflow build` | synthesis → place & route → timing → bitstream (ISE backend today) |
| `fpgaflow detect` | read-only JTAG scan of the project's board |
| `fpgaflow program` | load the bitstream into the FPGA (SRAM, volatile) |
| `fpgaflow boards` | known dev boards |
| `fpgaflow edit [file] [--line N --col N]` | open in the configured editor (Kate by default) |

Compiler errors from Icarus, Verilator and Yosys are reported as
`file:line:col` diagnostics; ISE constraint errors point at the line in your
own UCF. All 24 families synthesise the blinky example with Yosys 0.71, and the
Spartan-3AN goes all the way to a bitstream through ISE 14.7 (timing met at
222.8 MHz for blinky).

`fpgaflow build` refuses to write a bitstream if any top-level port bit has no
`LOC` constraint, and again if the pad report shows a pin the tools placed
themselves: an unconstrained pin can drive a net wired to something else on
the board. `examples/blinky/constraints/s3an-starter.ucf` and the master
`data/boards/s3an-starter.ucf` use the pin constraints printed in AMD's UG334.

**Verified on hardware:** blinky on a Spartan-3AN Starter Kit, built with Yosys
and ISE, loaded with openFPGALoader over the board's embedded Xilinx USB
cable: LED0 blinks and BTN_SOUTH resets it.

nextpnr/VPR backends, live board detection, the GUI and the Claude integration
are next.

## Programming the Spartan-3AN Starter Kit

1. **Power:** use a regulated 5 V supply (UG334 ch. 17). A weak adapter makes the
   board brown out, and the embedded USB cable (on a 1.8 V LDO from the 5 V
   rail) drops off USB every few seconds.
2. **USB access:** install the udev rule, then replug the USB cable:
   `sudo cp data/udev/60-fpgaflow-xilinx.rules /etc/udev/rules.d/ && sudo udevadm control --reload-rules`.
   openFPGALoader's own rules have no entry for Xilinx cables (USB vendor `03fd`).
3. **openFPGALoader with the XC3S700AN entry:** `scripts/build-openfpgaloader.sh`
   (stock v1.2.0 reads the IDCODE but has no part entry for it; submitted
   upstream as [openFPGALoader#771](https://github.com/trabucayre/openFPGALoader/pull/771)). openFPGALoader
   loads the cable's FX2 firmware (`xusb_emb.hex` from ISE) itself; the cable
   then re-enumerates from `03fd:000d` to `03fd:0008`.
4. `fpgaflow build && fpgaflow program`. The JTAG clock is 750 kHz: at 6 MHz
   this cable's IDCODE reads overflowed.

## Quick start

```sh
cargo build --release
./target/release/fpgaflow doctor                  # what is installed?
./target/release/fpgaflow init myproj --top top   # new project
./target/release/fpgaflow -C myproj edit          # opens fpgaflow.toml in Kate
./target/release/fpgaflow -C examples/blinky info
```

### External tools

The easiest way to get the open-source tools is the
[OSS CAD Suite](https://github.com/YosysHQ/oss-cad-suite-build). It includes
Yosys, Icarus, Verilator, GTKWave, nextpnr and openFPGALoader. fpgaflow looks in
`~/tools/oss-cad-suite/bin` and `/opt/Xilinx/14.7/ISE_DS/ISE/bin/lin64`, then
`$PATH`. Run `fpgaflow config` to change the search paths or pin individual
tools.

## Project file

```toml
[project]
name = "blinky"
top  = "blinky"

[sources]
rtl = ["rtl/**/*.sv"]
tb  = ["tb/**/*.sv"]

[sim]
tool   = "verilator"   # or "icarus"
tb_top = "tb_blinky"
wave   = "fst"

[synth]                # all optional
frontend = "verilog"   # or "slang" for full SystemVerilog (yosys-slang)
# script = "synth/custom.ys"   # replaces the family's synth command
# extra  = ["opt -full"]       # run after synthesis

[target]
family  = "spartan3a"
device  = "xc3s700an"
package = "fgg484"
backend = "ise"

[program]
mode = "sram"          # or "flash"
```

## Roadmap

1. Skeleton: project model, Doctor, job runner, editor launcher, CLI *(done)*
2. Simulation: Icarus and Verilator, GTKWave, clickable diagnostics *(done)*
3. Synthesis: Yosys for every supported family *(done)*
4. P&R and bitstream:
   - ISE 14.7 (Spartan-3AN) first *(done)*
   - then nextpnr (iCE40, ECP5)
   - then VPR (generic arch, F4PGA xc7)
   - then Gowin, Nexus, Mistral and openXC7
   - then the other vendor adapters
5. Programming *(done for the Spartan-3AN kit)* and live board detection (udev)
6. Claude integration (API and Claude Code CLI)
7. egui GUI over all of the above, plus packaging

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
