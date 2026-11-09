#!/bin/bash
# Live checks of the built Glyphspin.app on a macOS runner (or a Mac): launch, screenshot the desktop with the
# wallpaper drawing, read back Metal frames (SDR and the EDR float path), send effect commands, measure CPU and
# memory, then quit. Writes everything to OUT.   tools/ci/mac_live.sh dist/Glyphspin.app OUT
set -u
APP="$1"; OUT="$2"; mkdir -p "$OUT"
BIN="$APP/Contents/MacOS/glyphspin"
LOG="$HOME/Library/Application Support/Glyphspin/log.txt"
sw_vers > "$OUT/system.txt"; sysctl -n machdep.cpu.brand_string >> "$OUT/system.txt" 2>/dev/null
system_profiler SPDisplaysDataType >> "$OUT/system.txt" 2>/dev/null

start() { "$BIN" "$@" > "$OUT/stdout.txt" 2>&1 & PID=$!; sleep 6; }
quit() { "$BIN" --quit; sleep 1; if kill -0 "$PID" 2>/dev/null; then echo "STILL RUNNING"; kill "$PID"; else echo "process gone"; fi; }
png() { sips -s format png "$1" --out "${1%.bmp}.png" > /dev/null && rm -f "$1"; }

echo "== SDR run (no-pause, seed 7, torus)"
start --no-pause --seed 7 --shape torus
screencapture -x "$OUT/desktop_1.png" || echo "screencapture failed"
"$BIN" --dump "$OUT/frame_sdr.bmp"; png "$OUT/frame_sdr.bmp"
"$BIN" --send fx sweep; "$BIN" --send ripple 900 500; "$BIN" --send fx spin
sleep 1.2
"$BIN" --dump "$OUT/frame_fx.bmp"; png "$OUT/frame_fx.bmp"
screencapture -x "$OUT/desktop_2_fx.png" || true
"$BIN" --send reroll knot; sleep 4
"$BIN" --dump "$OUT/frame_reroll.bmp"; png "$OUT/frame_reroll.bmp"
"$BIN" --send stats "$OUT/stats_sdr.txt"
echo "== idle cost (12 fps drift, 15 s)"
"$BIN" --send resetstats; sleep 3
t0=$(ps -o cputime= -p "$PID"); s0=$(date +%s); sleep 15; t1=$(ps -o cputime= -p "$PID"); s1=$(date +%s)
python3 - "$t0" "$t1" "$s0" "$s1" > "$OUT/cpu.txt" <<'PY'
import sys
def sec(t):
    t = t.strip(); p = t.split(':'); v = 0.0
    for x in p: v = v * 60 + float(x)
    return v
c0, c1, s0, s1 = sec(sys.argv[1]), sec(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
print(f"cpu {c1 - c0:.2f} s over {s1 - s0:.0f} s wall = {100 * (c1 - c0) / (s1 - s0):.2f} % of one core")
PY
ps -o rss=,vsz= -p "$PID" | awk '{print "rss " $1/1024 " MB, vsz " $2/1024 " MB"}' >> "$OUT/cpu.txt"
footprint "$PID" 2>/dev/null | head -5 >> "$OUT/cpu.txt" || true
"$BIN" --send stats "$OUT/stats_idle.txt"
echo "== second launch rerolls instead of starting twice"
"$BIN" ; sleep 1; pgrep -fl "Contents/MacOS/glyphspin" > "$OUT/instances.txt"
quit | tee "$OUT/quit_1.txt"
screencapture -x "$OUT/desktop_3_after_quit.png" || true

echo "== EDR float path (pretend headroom 4x)"
start --no-pause --seed 7 --shape torus --sim-hdr 4
"$BIN" --send fx sweep; "$BIN" --send ripple 900 500; sleep 0.6
"$BIN" --dump "$OUT/frame_edr.bmp"; png "$OUT/frame_edr.bmp"
"$BIN" --send stats "$OUT/stats_edr.txt"
quit | tee "$OUT/quit_2.txt"

echo "== default run (pausing on): does it draw on the bare runner desktop?"
start --seed 3
"$BIN" --send stats "$OUT/stats_default.txt"
screencapture -x "$OUT/desktop_4_default.png" || true
quit | tee "$OUT/quit_3.txt"
cp "$LOG" "$OUT/log.txt" 2>/dev/null || true
cat "$OUT"/*.txt
