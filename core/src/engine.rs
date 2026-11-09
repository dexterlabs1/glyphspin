//! The per-screen frame controller shared by both platforms: the scene, its cell grid, and OLED care (idle dimmer,
//! APL limiter, auto reroll, pixel orbit). Each frame the platform calls `pre_step` + `step`, uploads `scene.cells`
//! and draws with `consts`; `next_interval` says when the next frame is due. The platform owns the window, the GPU
//! surface, input, visibility and timing.

use crate::atlas::{ATLAS_COLS, Atlas, TILE_COUNT};
use crate::fx;
use crate::oled;
use crate::raster::Style;
use crate::scene::{AudioFrame, Params, Scene};
use crate::settings::{self, Settings};
use crate::util;

/// Frame rate while nothing is moving.
pub const IDLE_FPS: f32 = 12.0;

/// The shader's constant buffer (shaders/glyph.hlsl `Consts`, shaders/glyph.metal `Consts`; same layout).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Consts {
    pub grid_origin: [f32; 2],
    pub cell: [f32; 2],
    pub grid_dim: [u32; 2],
    pub atlas_cols: u32,
    pub hdr: u32,
    pub bands: [[f32; 4]; 6],
    pub amber: [f32; 4],
    pub glow_col: [f32; 4],
    pub white: f32,
    pub peak: f32,
    pub highlight: f32,
    pub glow_gain: f32,
    pub bright: f32,
    pub fx_bright: f32,
    pub bg_dim: f32,
    pub pad0: f32,
}

pub fn params(s: &Settings) -> Params {
    let lv = |v: &str| match v {
        "off" => 0.0,
        "subtle" => 0.5,
        _ => 1.0,
    };
    Params {
        style: match s.style.as_str() {
            "ramp" => Style::Ramp,
            "braille" => Style::Braille,
            _ => Style::Auto,
        },
        effects: lv(&s.effects),
        audio: if s.audio_source == "off" { 0.0 } else { lv(&s.audio_level) },
        oled: s.oled_care,
        weights: s.weights,
    }
}

pub struct Step {
    /// something is animating: draw at the frame-rate cap rather than the idle rate
    pub busy: bool,
    /// the frame differs from the last one drawn
    pub changed: bool,
}

pub struct Engine {
    pub scene: Scene,
    // grid
    pub cols: usize,
    pub rows: usize,
    pub cw: u32,
    pub ch: u32,
    pub origin: (i32, i32),
    pub atlas_cw: u32,
    // OLED care
    pub base_origin: (i32, i32),
    pub orbit_ph: (f32, f32),
    pub dimmer: oled::Dimmer,
    pub dim_target: f32,
    pub apl: oled::AplLimiter,
    pub auto_reroll: oled::AutoReroll,
    pub coverage: Vec<f32>,
    pub band_luma: [f32; 6],
    pub last_drawn: ((i32, i32), u32),
}

impl Engine {
    pub fn new(seed: u64, shape: Option<&'static str>) -> Engine {
        Engine {
            scene: Scene::new(seed, shape),
            cols: 0,
            rows: 0,
            cw: 0,
            ch: 0,
            origin: (0, 0),
            atlas_cw: 0,
            base_origin: (0, 0),
            orbit_ph: (((seed >> 8) % 6283) as f32 / 1000.0, ((seed >> 20) % 6283) as f32 / 1000.0),
            dimmer: oled::Dimmer::new(),
            dim_target: 1.0,
            apl: oled::AplLimiter { k: 1.0 },
            auto_reroll: oled::AutoReroll::new(seed as u32),
            coverage: Vec::new(),
            band_luma: [0.0; 6],
            last_drawn: ((i32::MIN, 0), 0),
        }
    }

    /// Target cell width in physical pixels for a screen `h` pixels tall at the glyph-size setting.
    pub fn cw_target(s: &Settings, h: u32) -> u32 {
        let rows_target = match s.glyph_size.as_str() {
            "fine" => 120.0,
            "coarse" => 60.0,
            _ => 84.0,
        };
        ((h as f32 / rows_target) / 2.0).round().max(4.0) as u32
    }

    /// Lay out the cell grid for a `w`x`h` pixel screen and the atlas built for `cw_target`.
    pub fn set_grid(&mut self, w: u32, h: u32, atlas: &Atlas, cw_target: u32) {
        self.cw = atlas.cw;
        self.ch = atlas.ch;
        // a spare cell each way so the pixel orbit (up to 3/4 of a cell) never uncovers an edge
        self.cols = (w.div_ceil(self.cw) + 2) as usize;
        self.rows = (h.div_ceil(self.ch) + 2) as usize;
        self.base_origin = ((w as i32 - (self.cols as u32 * self.cw) as i32) / 2,
                            (h as i32 - (self.rows as u32 * self.ch) as i32) / 2);
        self.origin = self.base_origin;
        self.coverage = oled::tile_coverage(&atlas.pixels, atlas.w, atlas.cw, atlas.ch, ATLAS_COLS, TILE_COUNT);
        self.last_drawn = ((i32::MIN, 0), 0);
        self.atlas_cw = cw_target;
        self.scene.set_grid(self.cols, self.rows, self.cw as f32, self.ch as f32, h as f32);
    }

    /// OLED care before the scene step: idle dimming and the auto reroll (a quiet fade through dark: effects are for
    /// interaction, not for an unattended wallpaper). True when it rerolled.
    pub fn pre_step(&mut self, s: &Settings, dt: f32, idle_s: f32) -> bool {
        self.dim_target = oled::Dimmer::target(idle_s, s.oled_care, s.idle_dim_min, s.idle_black_min);
        self.dimmer.step(self.dim_target, dt);
        let auto_min = if s.oled_care { s.auto_reroll_min } else { 0 };
        if self.auto_reroll.due(self.scene.time, auto_min) && !self.scene.is_busy_rerolling() {
            let lock = self.scene.shape_lock;
            self.scene.reroll(lock.filter(|n| *n != self.scene.shape_name()), 0.0, Some(fx::Transition::Morph));
            return true;
        }
        false
    }

    /// Step and compose the scene, then the APL limiter and the pixel orbit.
    pub fn step(&mut self, p: &Params, dt: f32, af: &AudioFrame) -> Step {
        let busy = self.scene.step(dt, p, af);
        let changed = self.scene.compose(dt, p);
        {
            let (g, l) = self.scene.layers();
            let apl = oled::apl(g, l, &self.coverage, &self.band_luma);
            self.apl.step(apl, dt);
        }
        let orbit = oled::orbit(self.scene.time, self.scene.oled_k, self.cw, self.ch, self.orbit_ph);
        self.origin = (self.base_origin.0 + orbit.0, self.base_origin.1 + orbit.1);
        let bright_key = (self.dimmer.v * 1000.0) as u32 ^ ((self.apl.k * 1000.0) as u32) << 12;
        let changed = changed || self.last_drawn != (self.origin, bright_key);
        self.last_drawn = (self.origin, bright_key);
        Step { busy, changed }
    }

    /// Shader constants. `hdr`: the target is a float (scRGB / EDR) surface; `white`: SDR white in the target's units
    /// (nits / 80 on Windows scRGB, 1.0 for macOS EDR and SDR); `peak`: the highlight cap in the same units.
    pub fn consts(&mut self, s: &Settings, hdr: bool, white: f32, peak: f32) -> Consts {
        let acc = util::hex_rgb(settings::accent_hex(&s.accent));
        let bg = util::hex_rgb("#0b0e0f");
        let wh = [1.0, 1.0, 1.0];
        let bands = [bg, util::mix(acc, bg, 0.80), util::mix(acc, bg, 0.62), util::mix(acc, bg, 0.38), acc,
                     util::mix(acc, wh, 0.45)];
        let mut c = Consts {
            grid_origin: [self.origin.0 as f32, self.origin.1 as f32],
            cell: [self.cw as f32, self.ch as f32],
            grid_dim: [self.cols as u32, self.rows as u32],
            atlas_cols: ATLAS_COLS,
            ..Default::default()
        };
        for (i, b) in bands.iter().enumerate() {
            c.bands[i] = util::lin(*b);
            self.band_luma[i] = 0.2126 * c.bands[i][0] + 0.7152 * c.bands[i][1] + 0.0722 * c.bands[i][2];
        }
        c.amber = util::lin(util::hex_rgb("#e0a64a"));
        c.glow_col = util::lin(util::mix(acc, wh, 0.2));
        c.glow_gain = 0.35;
        c.hdr = hdr as u32;
        c.white = white;
        c.peak = peak;
        c.highlight = s.highlight as f32 / 100.0;
        let rest = 1.0 + self.scene.oled_k * (oled::REST_SCALE - 1.0);
        c.fx_bright = self.dimmer.v * self.apl.k;
        c.bright = rest * c.fx_bright;
        c.bg_dim = self.dimmer.v;
        c
    }

    /// Faded to black by the idle dimmer: stop drawing until there is input again.
    pub fn asleep(&self) -> bool {
        self.dim_target == 0.0 && self.dimmer.v == 0.0
    }

    /// Seconds until the next frame: the frame-rate cap while busy (or recording), the idle rate otherwise.
    pub fn interval(s: &Settings, busy: bool, want_dump: bool) -> f32 {
        let cap = s.fps_cap as f32;
        let fps = if busy || want_dump { cap } else { IDLE_FPS.min(cap) };
        1.0 / fps
    }

    /// Test hooks for the effects (the same code paths the gestures use) and rerolls/ripples sent as text commands.
    /// `x`/`y` of a ripple are pixels from the screen's top-left. False if `parts` isn't one of these.
    pub fn fx_command(&mut self, s: &Settings, parts: &[&str]) -> bool {
        let effects = params(s).effects;
        match parts {
            ["ripple", x, y] => {
                let (x, y) = (x.parse::<f32>().unwrap_or(0.0), y.parse::<f32>().unwrap_or(0.0));
                self.scene.ripple(x - self.origin.0 as f32, y - self.origin.1 as f32, 1.8 * effects);
            }
            ["fx", "sweep"] => self.scene.fx.sweep_t = 0.0,
            ["fx", "explode", v] => self.scene.fx.explode_target = if *v == "1" { 1.0 } else { 0.0 },
            ["fx", "spin"] => self.scene.kick([0.0, 7.0, 1.0]),
            ["fx", "morph"] => self.scene.reroll(None, effects, Some(fx::Transition::Morph)),
            _ => return false,
        }
        true
    }
}
