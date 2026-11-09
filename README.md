<p align="center"><img src="assets/logo.png" width="96" alt="Glyphspin"></p>

<h1 align="center">Glyphspin</h1>

<p align="center">A 3D object drawn in text, turning slowly as your live wallpaper, on Windows and macOS.</p>

<p align="center"><img src="docs/hero.webp" width="960" alt="Glyphspin running as a wallpaper"></p>
<p align="center"><sub>Frames read back from the app's own swapchain at 3840x2160 (cropped).</sub></p>
<p align="center"><a href="https://github.com/dexterlabs1/glyphspin/releases/download/v0.1.0/glyphspin-demo.mp4"><b>Watch the one-minute demo</b></a> (a screen recording of a real desktop: drag, ripples, explode, slicing plane, morph, 4D turn)</p>

## Features
- **18 mathematical objects**, rendered as a luminance ramp, braille or dot glyphs (Cascadia Mono on Windows, SF Mono on macOS): torus, knots,
  geodesic sphere, Hopf fibration, a tesseract / 24-cell rotating in 4D, gyroid shell, Lorenz and Aizawa
  attractors that draw themselves, morphing supershapes, a Klein bottle, armillary rings, metaballs, a nebula with
  constellation lines, a Calabi-Yau slice and more.
- **Lives behind your desktop icons.** Icons, selection, double-click and the right-click menu all work as before.
- **Hands on.** Middle-drag to spin it with inertia, scroll to zoom, click for a ripple, click and hold to explode
  it, shift + middle-drag to turn 4D shapes through the fourth dimension.
- **Always fresh.** Every reroll picks a shape, a render mode (solid / wire / points), an ambient effect (a slicing
  plane, depth through glyph density) and a transition (dissolve or morph).
- **HDR aware.** FP16 scRGB on Windows HDR displays, EDR on Macs with XDR / HDR screens; glyphs stay at or below
  SDR white, highlights only in brief effects.
- **Themes.** Eight muted palettes (teal, amber, ice, sage, paper, ember, mono, rose) that crossfade when they
  change, an **OLED black** background that leaves unlit pixels fully off, and optional automatic rotation.
- **OLED care**, on by default: nothing lights the same pixels for hours (see below).
- **Music reactive** with Apple Music (or any audio), capturing only that app's sound.
- **Light.** About 2-3 % of one CPU core while drifting, nothing at all when the desktop is covered.

<p align="center"><img src="docs/shapes.png" width="960" alt="The 18 shapes"></p>

## Requirements
- Windows 10 2004+ or Windows 11 (tested on Windows 11 24H2 / 25H2, where the desktop's window layout changed;
  older layouts are handled by the classic WorkerW path).
- A GPU with Direct3D 11.
- HDR is optional. Apple Music (Microsoft Store version) is optional.

**macOS** (new in 0.2.0)
- macOS 13 Ventura or later, Apple silicon or Intel (one universal app).
- Music reactivity needs macOS 14.2 or later (Core Audio process taps).

## Install
1. Download `glyphspin-<version>-windows-x64.zip` from [Releases](../../releases) and unzip it anywhere.
2. Run `glyphspin.exe`. A small dotted ring appears in the system tray; the wallpaper takes over the desktop
   background.
3. To stop it, choose **Quit** in the tray menu (or run `glyphspin.exe --quit`). Your normal wallpaper comes back.

Nothing is installed; settings live in `%APPDATA%\Glyphspin\settings.json`. "Start with Windows" in the tray menu
adds or removes a `HKCU\...\Run` entry; it is off unless you turn it on.

### macOS
1. Download `glyphspin-<version>-macos-universal.zip` from [Releases](../../releases), unzip it and move
   `Glyphspin.app` to Applications.
2. The app is not notarized (no Apple Developer ID), so Gatekeeper stops the first launch:
   - macOS 13 / 14: right-click (Control-click) `Glyphspin.app`, choose **Open**, then **Open** again.
   - macOS 15 and later: open it once, then go to **System Settings > Privacy & Security** and click
     **Open Anyway** next to the Glyphspin message.
3. A dotted ring appears in the menu bar and the wallpaper fills the desktop behind your icons. **Quit** in that
   menu closes it. Your normal wallpaper is never changed: Glyphspin draws in its own window at desktop level.
4. With music on, macOS asks once for **System Audio Recording** permission (Privacy & Security > Screen &
   System Audio Recording). Without it the wallpaper still works; it just doesn't react to music.

Settings live in `~/Library/Application Support/Glyphspin/settings.json`. "Open at Login" in the menu registers it
as a login item (System Settings > General > Login Items); it is off unless you turn it on.

## Controls
| On the bare desktop | Does |
|---|---|
| Middle-drag | rotate; let go and it coasts, then eases back into its drift |
| Shift + middle-drag | turn a 4D shape through the xw / yw planes |
| Scroll | zoom (Ctrl + scroll stays Explorer's icon size) |
| Click | a ripple through the faint background glyphs |
| Click and hold (still) | exploded view, closes when you let go |
| Middle double-click | reroll to a new combination |

The app only observes the mouse; it never blocks or swallows input.

On macOS the same gestures work, plus **Option + drag** to rotate (and Option + double-click to reroll) for
trackpads and mice without a middle button; Control + scroll stays the system's screen zoom. Glyphspin doesn't ask
for Accessibility access, so it can't tell a click on a Finder icon from a click on bare desktop: icon clicks
ripple too.

## Tray menu and settings
Reroll · Shape (random or any of the 18) · Look (style, glyph size) · Theme (eight palettes, OLED black
background, rotate themes: off / with each new shape / every 15, 30 or 60 min) ·
Effects (off / subtle / full) · Music (Apple Music / all audio / off, intensity) · Display (HDR auto / on / off,
highlight 0-100 %) · OLED care · Frame rate (15 / 30 / 60 max) · Pause · Start with Windows (Open at Login on
macOS) · Quit.

In `settings.json` these are `theme`, `black_bg` and `theme_rotate` (`off`, `shape`, `15`, `30`, `60`); a theme
change, the black toggle and each rotation crossfade over about four seconds. `settings.json` also holds the
reroll weights: `w_solid`, `w_wire`, `w_points` (render modes), `w_plain`,
`w_sweep`, `w_density` (ambient effect) and `w_dissolve`, `w_morph` (transition).

## OLED care
On by default, because a wallpaper is the most static thing a screen shows.
- The object wanders slowly over most of the screen and gently changes size (periods of 5-15 minutes, eased).
- The glyph grid orbits by up to 3/4 of a cell in whole pixels, so glyph edges never sit on the same pixels.
- Background dots fade in and out every few minutes, in new places.
- At rest the brightest glyph is about half your SDR white (about 120 nits at a 240-nit SDR white). Only brief
  effects reach HDR highlights, and they fade within 1-3 s. The average picture level is capped.
- After 10 minutes without input it dims to 30 %; after 30 more minutes it fades to black and stops drawing. Any
  input brings it back.
- A new shape every 25 minutes.
- Optional: **OLED black background** (Theme menu) makes the background true black instead of near-black, so
  every pixel without a glyph is switched off.

A two-hour simulated burn-in (per-pixel light summed over the run, same scale) shows the difference: the hottest
pixel drops from 33 % of SDR white on average to 3 %, and its ratio to the screen mean from 37x to 5x.

<p align="center"><img src="docs/oled-burn-in.png" width="960" alt="Two-hour burn-in heat maps, without and with OLED care"></p>

## Music
With the Apple Music app playing, Glyphspin captures that app's audio only (Windows per-process loopback), so game
sounds and notification pings don't move it; "all audio" listens to the default output instead. Bass breathes the
object's size, beats send soft flashes or ripples, mids and highs speed its slow morph, loudness speeds the spin,
and eight spectrum bands ripple the surface. Capture runs only while something is playing (via the Windows media
session) and the wallpaper is visible. It is read-only and doesn't affect playback.

On macOS a Core Audio process tap listens to the Music app only ("all audio" taps the system output). Capture
runs while Music is playing (from its own player notifications) or any of its processes is sending audio.

## Performance
Measured on a 3840x2160 desktop (Ryzen 9 9950X3D, RTX 5090), release build:

| State | CPU (one core) |
|---|---|
| Desktop covered, locked or display off | 0.1 % (fully paused, no frames) |
| Idle drift, 12 fps, depending on shape | 1.8 - 3.3 % |
| During effects (spin, explode, sweep, reroll) | about 6 % |
| Music reacting, 30 fps | about 9 % |

Memory: about 25 MB private working set. GPU load is a single fullscreen pass per frame.

## Build from source
```
git clone https://github.com/dexterlabs1/glyphspin
cd glyphspin
cargo build --release
target\release\glyphspin.exe
```
Needs Rust (stable, MSVC toolchain) and the Windows SDK (for `rc.exe`, which embeds the icon). The compiled
shaders (`shaders/*.cso`) are committed, so `fxc` is only needed if you edit `shaders/glyph.hlsl`.
`python tools/brand.py` regenerates the icon and logo.

On macOS (Rust stable with the `aarch64-apple-darwin` and `x86_64-apple-darwin` targets, Xcode command line tools,
python3):
```
packaging/macos/bundle.sh        # dist/Glyphspin.app (universal, ad-hoc signed) and its zip
open dist/Glyphspin.app
```
The Metal shader (`shaders/glyph.metal`) is compiled at launch, so no Xcode project is involved.

## How it works
- On Windows 11 24H2+ the desktop is a `Progman` window whose children are the icon layer (`SHELLDLL_DefView`) and
  a `WorkerW` painting the static wallpaper. Glyphspin parents its own window between the two and draws into it
  with Direct3D 11 through DirectComposition.
- Each frame, the CPU rasterises the shape onto a grid of character cells (about 25,000 at 4K) and composes the
  effects there; one small texture is uploaded and a single pixel shader turns cells into glyphs from a Cascadia
  Mono atlas, in scRGB for HDR or sRGB otherwise.
- It sleeps between frames on a high-resolution timer, drops to 12 fps when nothing is moving, and stops
  completely (including its mouse hook and audio capture) whenever the desktop isn't visible.

- On macOS each screen gets a borderless window at the desktop level, just above the system wallpaper and below
  Finder's icons, drawn with Metal (8-bit sRGB, or 16-bit float extended sRGB when the screen has EDR headroom).
  The same cell pipeline runs in a shared, platform-free core, so both platforms render the same cells.

## License
MIT. See [LICENSE](LICENSE).
