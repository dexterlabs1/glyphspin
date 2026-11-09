#!/bin/bash
# Render a fixed set of --offline cases with EXE into OUT (BMP + cells + stdout per case). CI runs it on Windows
# and macOS and compare.py diffs the two. "font_*" cases use the platform font (Cascadia Mono / SF Mono) and are
# expected to differ; "proc_*" cases use --atlas procedural, so any difference is in the core itself.
set -u
EXE="$1"; OUT="$2"; mkdir -p "$OUT"
run() { name=$1; shift; "$EXE" --offline "$OUT/$name.bmp" --cells "$OUT/$name.cells" "$@" > "$OUT/$name.txt" 2>&1 || echo "FAILED $name"; }
run font_torus  --shape torus --seed 7 --frames 60 --size 1920 1080
run font_knot   --shape knot --seed 11 --frames 45 --size 1920 1080 --style braille
run proc_torus  --shape torus --seed 7 --frames 60 --size 1920 1080 --atlas procedural
run proc_knot   --shape knot --seed 11 --frames 45 --size 1920 1080 --style braille --atlas procedural
run proc_fx     --shape supershape --seed 21 --frames 70 --size 1920 1080 --sweep-at 10 --explode-at 30 \
                --spin-at 40 --ripple-at 50 --audio-sim --atlas procedural
run proc_reroll --shape hopf --seed 4 --frames 60 --size 1920 1080 --reroll-at 20 --atlas procedural
run proc_tess   --shape tesseract --seed 5 --frames 40 --size 1920 1080 --ripple-at 20 --atlas procedural
run proc_burn   --shape torus --seed 7 --burn 300 --size 1920 1080 --atlas procedural
ls -la "$OUT"
