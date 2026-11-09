//! Colour themes. A theme is a near-black background, one accent (the five depth bands are mixed from it toward the
//! background, the brightest toward white) and a secondary colour for the slicing-plane rim and warm tints.
//! "OLED black" replaces only the background with true black, so unlit pixels are off; the glyph bands keep the
//! theme's colours. Theme changes (picked, rotated or the black toggle) crossfade in linear light, eased.

use crate::util;

pub struct Theme {
    pub name: &'static str,
    pub label: &'static str,
    pub bg: &'static str,
    pub accent: &'static str,
    pub second: &'static str,
}

const fn t(name: &'static str, label: &'static str, bg: &'static str, accent: &'static str, second: &'static str)
    -> Theme {
    Theme { name, label, bg, accent, second }
}

/// Muted on near-black only: no neon, no rainbow. The first five keep the names of 0.1/0.2's accent colours.
pub const THEMES: [Theme; 8] = [
    t("teal", "Teal", "#0b0e0f", "#4fc7bd", "#e0a64a"),
    t("amber", "Amber", "#0f0c08", "#e0a64a", "#8fb8de"),
    t("ice", "Ice", "#090c10", "#8fb8de", "#e0a64a"),
    t("sage", "Sage", "#0b0f0b", "#9cc49a", "#d9b26a"),
    t("paper", "Paper", "#0e0d0b", "#d9d4c7", "#e0a64a"),
    t("ember", "Ember", "#100a09", "#d98a6a", "#e8c98a"),
    t("mono", "Mono", "#0c0c0c", "#bfc3c4", "#e0a64a"),
    t("rose", "Rose", "#0f0b0c", "#c99aa3", "#9cc49a"),
];

/// `theme_rotate` values: off, a new theme with each new shape, or every N minutes.
pub const ROTATE: [&str; 5] = ["off", "shape", "15", "30", "60"];

/// Seconds a theme change takes.
pub const FADE_S: f32 = 4.0;

pub fn index(name: &str) -> usize {
    THEMES.iter().position(|t| t.name == name).unwrap_or(0)
}

/// The theme after `name` in the list (rotation order).
pub fn next(name: &str) -> &'static str {
    THEMES[(index(name) + 1) % THEMES.len()].name
}

/// A theme's shader colours, linear.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// 0 = background, 1..5 = dim .. bright
    pub bands: [[f32; 4]; 6],
    pub second: [f32; 4],
    pub glow: [f32; 4],
}

pub fn palette(name: &str, black: bool) -> Palette {
    let th = &THEMES[index(name)];
    let acc = util::hex_rgb(th.accent);
    let bg = util::hex_rgb(th.bg);
    let wh = [1.0, 1.0, 1.0];
    let srgb = [bg, util::mix(acc, bg, 0.80), util::mix(acc, bg, 0.62), util::mix(acc, bg, 0.38), acc,
                util::mix(acc, wh, 0.45)];
    let mut bands = srgb.map(util::lin);
    if black {
        bands[0] = [0.0, 0.0, 0.0, 1.0];
    }
    Palette { bands, second: util::lin(util::hex_rgb(th.second)), glow: util::lin(util::mix(acc, wh, 0.2)) }
}

fn smootherstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// `a` -> `b` at progress `t` (0..1), eased.
pub fn blend(a: &Palette, b: &Palette, t: f32) -> Palette {
    if t >= 1.0 {
        return *b;
    }
    let k = smootherstep(t);
    let l = |x: [f32; 4], y: [f32; 4]| [x[0] + (y[0] - x[0]) * k, x[1] + (y[1] - x[1]) * k, x[2] + (y[2] - x[2]) * k, 1.0];
    let mut bands = a.bands;
    for i in 0..6 {
        bands[i] = l(a.bands[i], b.bands[i]);
    }
    Palette { bands, second: l(a.second, b.second), glow: l(a.glow, b.glow) }
}

/// Per-screen theme state: which theme is showing (rotation can move away from the chosen one), the crossfade, and
/// the rotation clock (scene time, so it stands still while the wallpaper is paused).
pub struct ThemeState {
    pub name: &'static str,
    seen: String,
    seen_rotate: String,
    from: Palette,
    to: Palette,
    t: f32,
    next_at: f32,
    last_shape: &'static str,
    init: bool,
}

impl ThemeState {
    pub fn new() -> ThemeState {
        let p = palette("teal", false);
        ThemeState { name: "teal", seen: String::new(), seen_rotate: String::new(), from: p, to: p, t: 1.0, next_at: 0.0, last_shape: "",
                     init: false }
    }

    fn period(rotate: &str) -> Option<f32> {
        rotate.parse::<f32>().ok().filter(|m| *m > 0.0).map(|m| m * 60.0)
    }

    /// Advance by `dt` at scene time `time` with the current shape. True while crossfading.
    pub fn step(&mut self, theme: &str, black: bool, rotate: &str, time: f32, shape: &'static str, dt: f32) -> bool {
        if !self.init || theme != self.seen {
            // first frame, or the user picked a theme: show it (rotation continues from there)
            self.seen = theme.to_string();
            self.name = THEMES[index(theme)].name;
            self.seen_rotate.clear();
            if !self.init {
                self.last_shape = shape;
            }
        }
        if rotate != self.seen_rotate {
            // rotation switched on or its period changed: the next change is one full period away
            self.seen_rotate = rotate.to_string();
            self.next_at = time + Self::period(rotate).unwrap_or(0.0);
        }
        match rotate {
            "shape" => {
                if shape != self.last_shape {
                    self.name = next(self.name);
                }
            }
            r => {
                if let Some(p) = Self::period(r) {
                    if time >= self.next_at {
                        self.name = next(self.name);
                        self.next_at = time + p;
                    }
                }
            }
        }
        self.last_shape = shape;
        let target = palette(self.name, black);
        if !self.init {
            self.from = target;
            self.to = target;
            self.t = 1.0;
            self.init = true;
        } else if target != self.to {
            self.from = self.current();
            self.to = target;
            self.t = 0.0;
        }
        if self.t < 1.0 {
            self.t = (self.t + dt / FADE_S).min(1.0);
        }
        self.fading()
    }

    pub fn fading(&self) -> bool {
        self.t < 1.0
    }

    pub fn current(&self) -> Palette {
        blend(&self.from, &self.to, self.t)
    }
}
