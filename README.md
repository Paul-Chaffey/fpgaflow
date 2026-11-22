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
| `fpgaflow edit [file] [--line N --col N]` | open in the configured editor (Kate by default) |

Compiler errors from Icarus, Verilator and Yosys are reported as
`file:line:col` diagnostics. All 24 families synthesise the blinky example
with Yosys 0.71. P&R, programming, the GUI and the Claude integration are
next.

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
   - ISE 14.7 (Spartan-3AN) first
   - then nextpnr (iCE40, ECP5)
   - then VPR (generic arch, F4PGA xc7)
   - then Gowin, Nexus, Mistral and openXC7
   - then the other vendor adapters
5. Programming and live board detection (udev)
6. Claude integration (API and Claude Code CLI)
7. egui GUI over all of the above, plus packaging

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.
