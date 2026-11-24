#!/usr/bin/env bash
# Render the RC10 Player icon (data/icons/fpgaflow-player.svg) to the PNG
# sizes the desktop uses (data/icons/hicolor/<size>/apps). Needs Inkscape.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
for z in 16 22 24 32 48 64 128 256; do
  out="$repo/data/icons/hicolor/${z}x${z}/apps/fpgaflow-player.png"
  mkdir -p "$(dirname "$out")"
  inkscape "$repo/data/icons/fpgaflow-player.svg" --export-type=png \
    --export-filename="$out" -w "$z" -h "$z" >/dev/null 2>&1
done
echo "rendered data/icons/hicolor/*/apps/fpgaflow-player.png"
