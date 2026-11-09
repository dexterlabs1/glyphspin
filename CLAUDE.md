# CLAUDE.md — Glyphspin

## What it is
Glyphspin is a live wallpaper for Windows and macOS: one 3D object rendered as text glyphs, turning slowly behind
the desktop icons, with mouse interaction, HDR output, OLED care and music reactivity. Rust, single binary, no
runtime.
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
- macOS: objc2 0.6 / objc2-* 0.3 (metal-rs and cocoa are deprecated); a borderless window per screen at the
  desktop level, Metal (BGRA8 sRGB, or RGBA16F extended linear sRGB when the screen has EDR headroom; EDR white =
  1.0); a global NSEvent monitor that only observes (Option + drag rotates on Macs without a middle button; no
  Accessibility API, so icon clicks ripple too); Core Audio process taps looked up at runtime (14.2+, the app still
  starts on 13); now playing from Music's `com.apple.Music.playerInfo` notification (MediaRemote is closed since
  15.4); IPC over a Unix socket in `~/Library/Application Support/Glyphspin`; ad-hoc signed, not notarized.
- OLED care is on by default; any new effect must move, vary or decay (no static light).
- Motion: `bounce` (default) = constant speed 0.03 screen heights/s in a straight line; each edge is a soft
  spring (a 1.5 s half-sine turn whose deepest point is the edge), speed restored between hits, so velocity
  stays continuous (DVD logo, eased). Direction comes from the per-launch phases, not the RNG, so seeded
  offline renders are unchanged. `wander` = the OLED-care sums of sines. Switching blends over ~2 s.
- Themes (`core/src/theme.rs`): muted accents on near-black only (no neon, no rainbow); bands are mixed from
  the accent toward the theme background. OLED black swaps only the background for true black. Every theme
  change crossfades in linear light with smootherstep over 4 s; rotation runs on scene time (stands still while
  paused) and each screen keeps its own theme state.

## Layout
Cargo workspace. `core/` (crate `glyphspin-core`, no dependencies, no platform code): `shapes.rs` / `shapes_x.rs`
shapes · `raster.rs` cell rasteriser · `scene.rs` motion, interaction, field, effects, composition · `fx.rs` render
modes and effects · `oled.rs` OLED care · `audio.rs` FFT analysis + shared features · `engine.rs` per-screen frame
controller · `atlas.rs` atlas layout · `menu.rs` menu model · `settings.rs` · `theme.rs` themes, OLED black, rotation + crossfade · `offline.rs` headless pipeline
(benchmarks, reference shading, burn-in heat maps) · `dump.rs` readback decode + stats.
`src/main.rs` CLI · `src/platform/mod.rs` the `Platform` / `Surface` traits ·
`src/platform/windows/` `desktop.rs` wallpaper host (24H2 + legacy), `gpu.rs` D3D11/DComp, `hdr.rs`, `glyphs.rs`
GDI atlas, `audio.rs` SMTC + loopback, `input.rs` hook, `watch.rs` visibility, `tray.rs` ·
`src/platform/macos/` `screen.rs`, `metal.rs`, `glyphs.rs` CoreText atlas, `input.rs`, `watch.rs`, `menubar.rs`,
`audio.rs` process taps, `ipc.rs` · `shaders/glyph.hlsl` (+ committed `.cso`) and `shaders/glyph.metal` (compiled
at launch) · `packaging/macos/` Info.plist + `bundle.sh` · `tools/brand.py` icon/logo/icns · `tools/ci/` offline
cases, macOS live checks, Windows-vs-macOS compare.

## Key commands
```
cargo build --release                                   # target\release\glyphspin.exe
glyphspin.exe --quit                                    # close the running instance (restores the wallpaper)
glyphspin.exe --dump out.bmp                            # read back the next frame (+ stats .txt)
glyphspin.exe --send reroll | ripple X Y | set key=value | stats PATH | fx sweep|spin|morph|explode 0/1
glyphspin.exe --offline out.bmp --shape torus --frames 90         # headless render + timing
glyphspin.exe --offline heat.bmp --burn 7200 [--oled off] [--idle] # two-hour burn-in heat map
```
macOS: `packaging/macos/bundle.sh` (universal `dist/Glyphspin.app` + zip); the same CLI runs from
`Glyphspin.app/Contents/MacOS/glyphspin`. CI (`.github/workflows/build.yml`) builds both, runs the offline cases
on both, live-checks the Mac app on a runner (screenshots, readbacks, CPU) and compares the cell grids.
Offline cross-platform flags: `--atlas procedural`, `--cells PATH` (FNV-1a hash of the cell grid); theme
previews: `--theme NAME`, `--black`.
Test flags: `--no-pause`, `--shape S`, `--seed N`, `--sim-hdr <nits>`, `--sim-idle <s>`, `--audio-exe <exe|pid:N>`,
`--audio-ungated`. Shaders: `fxc -T ps_5_0 -E ps_main -O3 -Fo shaders\ps.cso shaders\glyph.hlsl` (and `vs_5_0` /
`vs_main` / `vs.cso`).

## Golden rules
1. Running it takes over the desktop background: end every test with `--quit` and check the process is gone.
2. Verify by running: `--dump` for real swapchain readbacks (HDR stats included), `--offline` without a GPU.
3. Keep idle cost honest: CPU over >= 10 s as % of one core, plus private working set.
4. Builds must stay warning-free. No real desktop captures in the repo.

## Current status
0.4.1: Apple Music capture follows AMPLibraryAgent.exe. 0.4.0: bounce motion (default). 0.3.0: themes, OLED black, theme rotation. 0.2.0: adds macOS. 0.1.0: 18 shapes, render modes and effects, HDR, OLED care, music reactivity, tray, CI release
workflow. Windows tested on Windows 11 24H2/25H2 with an RTX GPU at 3840x2160. macOS verified only on a CI runner
(macOS 26 VM, 1024x768, no audio device): drawn behind Finder's icons, SDR and EDR readbacks, effects, quit, ~1.9 %
of one core, cell grids identical to Windows. Not yet verified: a real Mac (Retina, multi-screen, music taps and
the permission prompt, Option-drag), the pre-24H2 WorkerW path, a real Explorer restart, on-screen HDR by eye,
multi-monitor beyond the primary display on Windows.
