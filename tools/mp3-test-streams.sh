#!/usr/bin/env bash
# Make the MP3 decoder's test streams (docs/mp3-decoder-plan.md) in
# build/mp3-streams/ (git-ignored: generated, and the music excerpt is not ours
# to publish). Usage: tools/mp3-test-streams.sh [MUSIC.wav]
set -euo pipefail
out=build/mp3-streams
mkdir -p "$out/src"
ff() { ffmpeg -hide_banner -loglevel error -y "$@"; }

# Sources (44.1 kHz stereo unless noted).
ff -f lavfi -i "sine=frequency=1000:duration=6" -f lavfi -i "sine=frequency=1500:duration=6" \
   -filter_complex "[0][1]amerge=inputs=2,volume=0.7" -ac 2 "$out/src/tones.wav"
ff -f lavfi -i "aevalsrc=0.6*sin(2*PI*(50+2000*t)*t)|0.6*sin(2*PI*(80+1500*t)*t):d=8:s=44100" "$out/src/sweep.wav"
# Clicks and silence: transients that force short and mixed blocks.
ff -f lavfi -i "aevalsrc=if(lt(mod(t\,0.25)\,0.002)\,0.9*sin(2*PI*3000*t)\,0.01*sin(2*PI*220*t))|if(lt(mod(t+0.1\,0.3)\,0.002)\,0.9\,0):d=6:s=44100" "$out/src/clicks.wav"
ff -f lavfi -i "anoisesrc=d=6:c=pink:a=0.3:r=44100" -ac 2 "$out/src/noise.wav"
if [ "${1:-}" ]; then
    ff -i "$1" -t 20 -ac 2 -ar 44100 "$out/src/music.wav"
fi

enc() {  # NAME SOURCE LAME-OPTIONS...
    local name=$1 src=$2; shift 2
    lame --quiet "$@" "$out/src/$src.wav" "$out/$name.mp3"
}
for s in tones sweep clicks noise $( [ -f "$out/src/music.wav" ] && echo music ); do
    enc "$s-128-joint"   $s -b 128 -m j
    enc "$s-320-stereo"  $s -b 320 -m s
    enc "$s-64-mono"     $s -b 64 -m m
    enc "$s-vbr"         $s -V 2
    enc "$s-160-forcems" $s -b 160 -m f
    enc "$s-96-32k"      $s -b 96 --resample 32
    enc "$s-192-48k"     $s -b 192 --resample 48
    enc "$s-128-nores"   $s -b 128 --nores
    ff -i "$out/src/$s.wav" -c:a libshine -b:a 128k "$out/$s-shine-128.mp3"
done
ls "$out"/*.mp3 | wc -l

# The ISO/IEC 11172-4 Layer III conformance streams and their reference
# decoder outputs, as mirrored in minimp3's repository (ISO's material: kept
# out of git). Free format (he_free) is outside the decoder's scope.
iso=build/iso-l3
mkdir -p "$iso"
for n in compl he_32khz he_44khz he_48khz he_mode hecommon si si_block si_huff sin1k0db; do
    for e in bit pcm; do
        [ -s "$iso/$n.$e" ] || curl -sSfL -o "$iso/$n.$e" \
            "https://raw.githubusercontent.com/lieff/minimp3/master/vectors/l3-$n.$e"
    done
done
ls "$iso"/*.bit | wc -l
