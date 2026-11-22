#!/usr/bin/env bash
# Unpack Celoxica's PDK 4.1 installer into docs/celoxica/ (local reference,
# git-ignored): the complete PDK, the RC10 schematic (PDF + 300 dpi sheets),
# manuals, Celoxica's RC10 headers, the FPGA images embedded in RC.dll, text
# extracts and disassembly of the host library and USB driver.
#
# The PDK is Celoxica's copyrighted software, so it is not in this repository;
# fpgaflow's own notes on the RC10 are in docs/rc10/. Nothing from the
# installer is executed: it is unpacked with 7z and unshield only.
#
#   scripts/extract-celoxica-pdk.sh [--download] [--force] [--out DIR] [PDK.exe]
#
#   --download  fetch the installer (from the page below) if it is missing
#   --force     replace an existing output directory
#   --out DIR   output directory (default docs/celoxica)
#
# Source: Washington University ARL FPX tools page (2006),
#   https://www.arl.wustl.edu/projects/fpx/tools.html
# Needs: 7z, unshield, pdftoppm and pdftotext (poppler-utils), objdump;
# Debian/Ubuntu: sudo apt install p7zip-full unshield poppler-utils binutils
set -euo pipefail

URL="http://www.arl.wustl.edu/projects/fpx/files/Celoxica/CEL-ENGDNLPDKPDK_4.1_Download-01001.exe"
SHA256="1cfbed7b81a43aa9d0ef990a89d45501e3f61d3b7d9decab651d6d765cb39924"

repo="$(cd "$(dirname "$0")/.." && pwd)"
exe="$repo/Downloads/CEL-ENGDNLPDKPDK_4.1_Download-01001.exe"
out="$repo/docs/celoxica"
download=0
force=0
while [ $# -gt 0 ]; do
    case "$1" in
        --download) download=1 ;;
        --force) force=1 ;;
        --out) out="$(realpath -m "$2")"; shift ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        -*) echo "unknown option $1" >&2; exit 2 ;;
        *) exe="$(realpath -m "$1")" ;;
    esac
    shift
done

for t in 7z unshield pdftoppm pdftotext objdump sha256sum; do
    command -v "$t" >/dev/null || { echo "missing tool: $t (see the header of $0)" >&2; exit 1; }
done

if [ ! -f "$exe" ]; then
    if [ "$download" = 1 ]; then
        mkdir -p "$(dirname "$exe")"
        echo "downloading $URL"
        curl -fL --retry 2 -o "$exe.part" "$URL"
        mv "$exe.part" "$exe"
    else
        echo "no installer at $exe (pass its path, or --download)" >&2
        exit 1
    fi
fi
got="$(sha256sum "$exe" | cut -d' ' -f1)"
if [ "$got" != "$SHA256" ]; then
    echo "SHA-256 mismatch for $exe:" >&2
    echo "  expected $SHA256" >&2
    echo "  got      $got" >&2
    exit 1
fi
echo "installer verified (SHA-256 $SHA256)"

if [ -e "$out" ]; then
    if [ "$force" = 1 ]; then rm -rf "$out"; else
        echo "$out exists (use --force to replace it)" >&2; exit 1
    fi
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
7z x -y -o"$work/setup" "$exe" >/dev/null
mkdir -p "$out"
unshield -d "$out/pdk41" x "$work/setup/Disk1/data1.cab" >/dev/null
echo "unpacked $(find "$out/pdk41" -type f | wc -l) files into $out/pdk41"

p="$out/pdk41"
r="$out/rc10"
mkdir -p "$r/schematic-pages" "$out/rcdll-resources" "$out/analysis/text" "$out/analysis/disassembly"
cp "$p/RC10/Documentation/PSL/RC10/Schematic/RC10.pdf" "$r/RC10-schematic.pdf"
cp "$p/RC10/Documentation/PSL/RC10/RC10 Manual.pdf" "$r/RC10-manual.pdf"
cp "$p/RC_host_library_and_utilities/Documentation/PSL/RC_host_library_and_FTU3/RC host library and FTU3 manual.pdf" \
    "$r/RC-host-library-and-FTU3-manual.pdf"
cp "$p"/RC10/Hardware/Include/*.hch "$p/RC10/Software/Bin/edifmake_rc10.bat" \
    "$p/RC10/Examples/PSL/RC10/RC10.hw" "$p/RC_host_library_and_utilities/Software/Include/rc.h" "$r/"
pdftoppm -r 300 -png "$r/RC10-schematic.pdf" "$r/schematic-pages/sheet"

# FPGA images embedded in RC.dll as BINARY resources.
dll="$p/RC_host_library_and_utilities/Software/Bin/RC.dll"
7z x -y -o"$work/rsrc" "$dll" .rsrc >/dev/null
cp "$work/rsrc/.rsrc/BINARY/107" "$out/rcdll-resources/HostControl-RC10-3s1500lfg320.bit"
cp "$work/rsrc/.rsrc/BINARY/110" "$out/rcdll-resources/HostControl-RC300-2v6000ff1152.bit"
cp "$work/rsrc/.rsrc/BINARY/101" "$out/rcdll-resources/resource-101-RC250.bin"

pdftotext -layout "$r/RC10-manual.pdf" "$out/analysis/text/RC10-manual.txt"
pdftotext -layout "$r/RC-host-library-and-FTU3-manual.pdf" "$out/analysis/text/RC-host-library-and-FTU3-manual.txt"
pdftotext -layout "$r/RC10-schematic.pdf" "$out/analysis/text/RC10-schematic.txt"
objdump -d -M intel "$dll" > "$out/analysis/disassembly/RC.dll.asm" 2>/dev/null
objdump -d -M intel "$p/RCUSB/Software/Drivers/RCUSB/celoxica_rcusb.sys" \
    > "$out/analysis/disassembly/celoxica_rcusb.sys.asm" 2>/dev/null

cat > "$out/README.md" <<'EOF'
# Celoxica PDK 4.1 (local copy, not in git)

Made by `scripts/extract-celoxica-pdk.sh` from Celoxica's PDK 4.1 installer.
This is Celoxica's copyrighted software and documentation, kept out of the
public repository by `.gitignore`. fpgaflow's own RC10 notes (USB protocol,
pin cross-check, board facts) are in `docs/rc10/`.

| Path | Contents |
|---|---|
| `rc10/RC10-schematic.pdf`, `rc10/schematic-pages/` | RC10 schematic, rev A 2005; sheets 1 hierarchy, 2 Analogue IP, 3 FPGA, 4 IO, 5 LEDs/Joystick, 6 USB IF/PSU (300 dpi PNG) |
| `rc10/RC10-manual.pdf` | RC10 manual |
| `rc10/RC-host-library-and-FTU3-manual.pdf` | RC host library API and FTU3 |
| `rc10/*.hch`, `rc10/rc.h`, `rc10/edifmake_rc10.bat`, `rc10/RC10.hw` | Celoxica's RC10 headers, host library header, ISE build script |
| `rcdll-resources/` | FPGA images embedded in RC.dll (Celoxica's "HostControl" designs) |
| `analysis/text/` | Text extracts of the manuals and schematic |
| `analysis/disassembly/` | objdump of RC.dll and celoxica_rcusb.sys |
| `pdk41/` | The complete unpacked PDK, by installer component |
EOF
echo "done: $out ($(du -sh "$out" | cut -f1))"
