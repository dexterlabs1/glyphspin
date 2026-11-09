# Changelog

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
