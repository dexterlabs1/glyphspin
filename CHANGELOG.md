# Changelog

## 0.4.0 - 2026-11-09
- Bounce motion (new default): the object travels in slow straight lines across the whole screen and turns
  softly at each edge, like the DVD logo at a fraction of the speed (a screen width in about a minute). The
  old drift stays as Look > Motion: wander. In a two-hour burn-in simulation bounce halves the hottest pixel.
  `--offline` takes `--bounce`.

## 0.3.0 - 2026-11-08
- Themes: teal, amber, ice, sage, paper, ember, mono and rose, each a background, an accent ramp and a
  secondary highlight. The old `accent` setting is read as the theme of the same name.
- OLED black background: true black under any theme, glyph colours unchanged.
- Rotate themes automatically: with each new shape, or every 15, 30 or 60 minutes.
- Theme changes crossfade (eased, about 4 s). `--offline` takes `--theme NAME` and `--black`; `stats` shows the
  current theme.

## 0.2.0 - 2026-11-08
- macOS support (13 Ventura+, universal app): a Metal window per screen at desktop level behind Finder's icons, EDR
  highlights on XDR / HDR screens, a menu bar extra with the same menu, Open at Login, Option + drag to rotate on
  trackpads, and music reactivity through Core Audio process taps (macOS 14.2+).
- The code is split into a platform-free core (shapes, scene, effects, OLED care, audio analysis, settings, menu
  model) and Windows / macOS backends; Windows output is byte-identical to 0.1.0 in the offline reference cases.
- `--dump` now reports the brightest pixel and the average for SDR frames too.

## 0.1.0 - 2026-11-08
First public release.
- Live wallpaper behind the desktop icons on Windows 10/11, including the Windows 11 24H2 desktop layout.
- 18 shapes rendered as ramp, braille or dot glyphs; render modes solid / wire / points.
- Interaction: middle-drag rotate with inertia, shift + middle-drag 4D rotation, wheel zoom, click ripple,
  click-and-hold exploded view, middle double-click reroll.
- Effects: background glyph field with ripples and drag wake, dissolve and morph rerolls, slicing-plane sweep,
  afterimage trails, depth cue by glyph density, brief HDR glow.
- HDR: FP16 scRGB output with SDR-white-relative levels and a highlight cap; 8-bit sRGB on SDR displays.
- OLED care: object wander, pixel orbit, rotating background field, reduced at-rest brightness, APL cap, idle dim
  and black, auto reroll.
- Music reactivity via per-process loopback (Apple Music) or the default output, gated on the Windows media
  session.
- Full pause when the desktop is covered, a fullscreen app runs, the session is locked or the display is off.
- Tray menu; settings in %APPDATA%\Glyphspin\settings.json.
