//! OLED care: keep any pixel from being lit the same way for hours.
//!
//! What lives here (the wander of the object and the rotating background field are in scene.rs):
//! - pixel orbit: the whole glyph grid drifts by up to 3/4 of a cell each way on a slow Lissajous path, in whole
//!   pixels (glyphs stay crisp); one-pixel steps every few seconds are invisible;
//! - at-rest brightness: glyphs that aren't part of an effect are scaled by REST_SCALE;
//! - average picture level cap: if the frame's mean luminance passes APL_CAP of SDR white, everything eases down;
//! - idle dimming: after N minutes without input ease down to DIM_LEVEL, after M more minutes to black (and stop
//!   drawing); any input eases back;
//! - auto reroll on a slow, jittered timer.

/// At rest the brightest glyph sits at ~0.66 x 0.75 = ~0.5 of SDR white (about 120 nits at a 240-nit SDR white).
pub const REST_SCALE: f32 = 0.75;
/// Mean frame luminance cap, as a fraction of SDR white. A quiet frame is ~1%; big zooms and effects can pass it.
pub const APL_CAP: f32 = 0.03;
/// Idle dim level (fraction of normal brightness).
pub const DIM_LEVEL: f32 = 0.3;

/// Grid offset in whole pixels for the pixel orbit at time `t`; `k` (0..1) eases it in and out.
pub fn orbit(t: f32, k: f32, cw: u32, ch: u32, ph: (f32, f32)) -> (i32, i32) {
    let tau = 2.0 * std::f32::consts::PI;
    let x = 0.75 * cw as f32 * (tau * t / 211.0 + ph.0).sin();
    let y = 0.75 * ch as f32 * (tau * t / 293.0 + ph.1).sin();
    ((k * x).round() as i32, (k * y).round() as i32)
}

/// Brightness that eases through two cascaded exponential smoothers (velocity-continuous, never snaps).
pub struct Dimmer {
    pub v: f32,
    mid: f32,
}

impl Dimmer {
    pub fn new() -> Dimmer {
        Dimmer { v: 1.0, mid: 1.0 }
    }

    /// Target brightness after `idle_s` seconds without input. Minutes of 0 mean never.
    pub fn target(idle_s: f32, care: bool, dim_min: u32, black_min: u32) -> f32 {
        if !care || dim_min == 0 {
            return 1.0;
        }
        let dim_at = dim_min as f32 * 60.0;
        if black_min > 0 && idle_s >= dim_at + black_min as f32 * 60.0 {
            0.0
        } else if idle_s >= dim_at {
            DIM_LEVEL
        } else {
            1.0
        }
    }

    pub fn step(&mut self, target: f32, dt: f32) {
        // down over ~10 s, back up in ~1 s
        let rate = if target < self.v { 0.45 } else { 5.0 };
        let k = 1.0 - (-dt * rate).exp();
        self.mid += (target - self.mid) * k;
        self.v += (self.mid - self.v) * k;
        if (self.v - target).abs() < 0.002 && (self.mid - target).abs() < 0.002 {
            self.v = target;
            self.mid = target;
        }
    }
}

/// Mean luminance of a composed frame relative to SDR white: sum of band luminance x glyph coverage per cell,
/// plus the background.
pub fn apl(glyph: &[u16], level: &[f32], coverage: &[f32], band_luma: &[f32; 6]) -> f32 {
    let mut sum = 0.0f32;
    for (g, l) in glyph.iter().zip(level) {
        if *g == 0 {
            continue;
        }
        let c = coverage.get(*g as usize).copied().unwrap_or(0.0);
        let l = l.clamp(0.0, 5.0);
        let i = (l.floor() as usize).min(4);
        let f = l - i as f32;
        let y = band_luma[i] + (band_luma[i + 1] - band_luma[i]) * f;
        sum += (y - band_luma[0]) * c;
    }
    band_luma[0] + sum / glyph.len().max(1) as f32
}

/// Eased gain that holds the frame's mean luminance under APL_CAP.
pub struct AplLimiter {
    pub k: f32,
}

impl AplLimiter {
    pub fn step(&mut self, apl: f32, dt: f32) {
        let target = if apl > APL_CAP { APL_CAP / apl } else { 1.0 };
        self.k += (target - self.k) * (1.0 - (-dt * 1.5).exp());
    }
}

/// Auto reroll every `minutes` (jittered +-15%) of displayed time.
pub struct AutoReroll {
    next: f32,
    seed: u32,
}

impl AutoReroll {
    pub fn new(seed: u32) -> AutoReroll {
        AutoReroll { next: -1.0, seed: seed | 1 }
    }

    fn jitter(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        0.85 + 0.3 * (self.seed % 1000) as f32 / 1000.0
    }

    /// True when a reroll is due at displayed time `t`.
    pub fn due(&mut self, t: f32, minutes: u32) -> bool {
        if minutes == 0 {
            self.next = -1.0;
            return false;
        }
        if self.next < 0.0 {
            self.next = t + minutes as f32 * 60.0 * self.jitter();
            return false;
        }
        if t >= self.next {
            self.next = t + minutes as f32 * 60.0 * self.jitter();
            return true;
        }
        false
    }
}

/// Per-tile mean coverage of a glyph atlas (for the APL estimate).
pub fn tile_coverage(px: &[u8], w: u32, cw: u32, ch: u32, cols: u32, tiles: u32) -> Vec<f32> {
    (0..tiles)
        .map(|t| {
            let (ox, oy) = ((t % cols) * cw, (t / cols) * ch);
            let mut s = 0u32;
            for y in 0..ch {
                for x in 0..cw {
                    s += px[((oy + y) * w + ox + x) as usize] as u32;
                }
            }
            s as f32 / (255.0 * (cw * ch) as f32)
        })
        .collect()
}
