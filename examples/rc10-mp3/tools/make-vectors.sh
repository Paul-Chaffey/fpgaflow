#!/usr/bin/env bash
# Front-end test vectors for tb_frontend: each stream as the PC sends it and
# the records mp3model expects, in build/vectors (git-ignored). Needs the
# streams from tools/mp3-test-streams.sh (run from the repository root).
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
out=$here/build/vectors
model=$root/target/release/mp3model
mkdir -p "$out"
: > "$out/list.txt"
add() {  # NAME FILE
    "$model" frontend "$2" "$out/$1" \
        | sed -E 's/.*: ([0-9]+) frames, CRC ([0-9a-f]+),.*/\1 \2/' > "$out/$1.sum"
    echo "$1" >> "$out/list.txt"
}
for n in compl he_32khz he_44khz he_48khz he_mode hecommon si si_block si_huff sin1k0db; do
    add "iso-$n" "$root/build/iso-l3/$n.bit"
done
for n in music-128-joint music-320-stereo clicks-64-mono noise-96-32k tones-192-48k sweep-shine-128 music-vbr; do
    add "$n" "$root/build/mp3-streams/$n.mp3"
done
# Damaged: 1000 random bytes in the middle of a frame, and the end cut mid-frame.
src=$root/build/mp3-streams/music-64-mono.mp3
{ head -c 20000 "$src"; head -c 1000 /dev/urandom; tail -c +20001 "$src" | head -c 30123; } > "$out/damaged.src"
add damaged "$out/damaged.src"
wc -l < "$out/list.txt"
