# CLAUDE.md — Glyphspin

## What it is
Glyphspin is a Windows live wallpaper: one 3D object rendered as text glyphs, turning slowly behind the desktop
icons, with mouse interaction, HDR output, OLED care and music reactivity. Rust, single exe, no runtime.
User-facing docs are in README.md. Maintainers keep local, untracked notes in `dev-notes/` (gitignored).

## Confirmed decisions
- Look: one object on near-black (`#0b0e0f`), five depth bands of one accent (teal `#4fc7bd` default), amber
  `#e0a64a` only as a secondary highlight. No rainbow palettes, glitch/CRT/scanlines, matrix rain, neon, bloom
  beyond brief effects, or on-screen text.
- Motion is always eased and velocity-continuous (cascaded exponential smoothers, critically damped springs,
  smootherstep paths). Only clicks and beats may start instantly, with a short attack.
- Stack: Rust + `windows` 0.62, D3D11 + DXGI + DirectComposition (Progman is `WS_EX_NOREDIRECTIONBITMAP` on 24H2,
  so GDI can't draw there), GDI for the glyph atlas. All per-frame work happens on the CPU at character-cell
  resolution; one pixel shader turns cells into glyphs.
- The mouse hook only observes (never swallows input), runs on its own thread, and is installed only while the
  desktop is visible. Middle button for rotation because Explorer binds nothing to it on the desktop.
- Full pause (no frames, hook, or audio) when the desktop is covered, a fullscreen app runs, the session is
  locked or the display is off.
- OLED care is on by default; any new effect must move, vary or decay (no static light).

## Layout
`src/main.rs` controller + frame loop + CLI · `desktop.rs` wallpaper host (24H2 + legacy) · `gpu.rs` D3D11/DComp ·
`hdr.rs` · `glyphs.rs` atlas · `shapes.rs` / `shapes_x.rs` shapes · `raster.rs` cell rasteriser · `scene.rs`
motion, interaction, field, effects, composition · `fx.rs` render modes and effects · `oled.rs` OLED care ·
`audio.rs` SMTC + loopback + FFT · `input.rs` hook · `watch.rs` visibility · `tray.rs`, `settings.rs` ·
`offline.rs` headless pipeline (benchmarks, reference shading, burn-in heat maps) · `shaders/glyph.hlsl` (+
committed `.cso`) · `tools/brand.py` icon/logo.

## Key commands
```
cargo build --release                                   # target\release\glyphspin.exe
glyphspin.exe --quit                                    # close the running instance (restores the wallpaper)
glyphspin.exe --dump out.bmp                            # read back the next frame (+ stats .txt)
glyphspin.exe --send reroll | ripple X Y | set key=value | stats PATH | fx sweep|spin|morph|explode 0/1
glyphspin.exe --offline out.bmp --shape torus --frames 90         # headless render + timing
glyphspin.exe --offline heat.bmp --burn 7200 [--oled off] [--idle] # two-hour burn-in heat map
```
Test flags: `--no-pause`, `--shape S`, `--seed N`, `--sim-hdr <nits>`, `--sim-idle <s>`, `--audio-exe <exe|pid:N>`,
`--audio-ungated`. Shaders: `fxc -T ps_5_0 -E ps_main -O3 -Fo shaders\ps.cso shaders\glyph.hlsl` (and `vs_5_0` /
`vs_main` / `vs.cso`).

## Golden rules
1. Running it takes over the desktop background: end every test with `--quit` and check the process is gone.
2. Verify by running: `--dump` for real swapchain readbacks (HDR stats included), `--offline` without a GPU.
3. Keep idle cost honest: CPU over >= 10 s as % of one core, plus private working set.
4. Builds must stay warning-free. No real desktop captures in the repo.

## Current status
0.1.0: 18 shapes, render modes and effects, HDR, OLED care, music reactivity, tray, CI release workflow.
Tested on Windows 11 24H2/25H2 with an RTX GPU at 3840x2160. Not yet verified: the pre-24H2 WorkerW path, a real
Explorer restart, on-screen HDR by eye, multi-monitor beyond the primary display.
