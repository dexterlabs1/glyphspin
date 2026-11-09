//! Everything that moves: the object's motion (ported from the original Python prototype's idle spin), mouse interaction, the background glyph
//! field and its effects (ripples, drag wake, spin push, glow), the reroll dissolve, and audio reactivity. All of it
//! runs on the CPU at cell resolution and ends in one packed cell buffer for the GPU.
//!
//! Motion rule: every movement is eased and velocity-continuous. Velocities go through cascaded
//! exponential smoothers or critically damped springs; nothing snaps. Only on-beat hits and clicks may start
//! instantly, and even those have a short attack ramp.

use crate::fx::{self, Fx, Mode, Transition};
use crate::raster::{self, Extras, ObjCell, Scratch, Style, View};
use crate::shapes::Kind;
use crate::shapes::{Geom, Rng, SHAPES, Shape, V3, add, cross, len, norm, scale, sstep, sub};
use crate::util::f32_to_f16;

type Q = [f64; 4];

fn q_mul(a: Q, b: Q) -> Q {
    let [w1, x1, y1, z1] = a;
    let [w2, x2, y2, z2] = b;
    [w1 * w2 - x1 * x2 - y1 * y2 - z1 * z2,
     w1 * x2 + x1 * w2 + y1 * z2 - z1 * y2,
     w1 * y2 - x1 * z2 + y1 * w2 + z1 * x2,
     w1 * z2 + x1 * y2 - y1 * x2 + z1 * w2]
}

fn q_from_rotvec(v: V3) -> Q {
    let (x, y, z) = (v[0] as f64, v[1] as f64, v[2] as f64);
    let a = (x * x + y * y + z * z).sqrt();
    if a < 1e-12 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    let s = (a / 2.0).sin() / a;
    [(a / 2.0).cos(), x * s, y * s, z * s]
}

fn q_axis(axis: V3, ang: f32) -> Q {
    q_from_rotvec(scale(norm(axis), ang))
}

fn q_norm(q: Q) -> Q {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

fn q_to_mat(q: Q) -> [[f32; 3]; 3] {
    let [w, x, y, z] = q;
    let m = [[1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - w * z), 2.0 * (x * z + w * y)],
             [2.0 * (x * y + w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - w * x)],
             [2.0 * (x * z - w * y), 2.0 * (y * z + w * x), 1.0 - 2.0 * (x * x + y * y)]];
    m.map(|r| r.map(|v| v as f32))
}

fn rest_quat(shape: &Shape) -> Q {
    let mut q = [1.0, 0.0, 0.0, 0.0];
    for (axis, ang) in &shape.rest {
        if *ang != 0.0 {
            q = q_mul(q_axis(*axis, *ang), q);
        }
    }
    q
}

fn smootherstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

fn hash2(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77).rotate_left(13);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

#[derive(Clone, Copy, Default, Debug)]
pub struct AudioFrame {
    pub active: bool,
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
    pub loud: f32,
    pub onset_count: u64,
    pub onset_strength: f32,
    /// QPC time (seconds) when the samples behind the latest onset were captured
    pub onset_capture_s: f64,
    /// eight log-spaced spectrum bands, 0..1 (60 Hz .. 12 kHz)
    pub spec: [f32; 8],
}

#[derive(Clone, Copy)]
pub struct Params {
    pub style: Style,
    pub effects: f32,   // 0 off, 0.5 subtle, 1 full
    pub audio: f32,     // 0 off, 0.5 subtle, 1 full
    pub oled: bool,     // OLED care: wander, rotating field, no at-rest HDR pop
    pub weights: fx::Weights,
}

struct Ripple {
    x: f32,
    y: f32,
    age: f32,
    amp: f32,
    r0: f32,
}

struct Particle {
    p0: (f32, f32),
    c: (f32, f32),
    p1: (f32, f32),
    g0: u16,
    g1: u16,
    l0: f32,
    l1: f32,
    delay: f32,
    fade_out: bool,
}

const REROLL_DUR: f32 = 2.3;
const FIELD_DENSITY: f32 = 0.035;
const FIELD_REST_LEVEL: f32 = 0.38;

pub struct Scene {
    rng: Rng,
    pub shape: Shape,
    pub shape_lock: Option<&'static str>,
    geom: Geom,
    geom_age: f32,
    geom_stale: bool,
    built_ppu: f32,
    q: Q,
    w: V3,
    w_mid: V3,
    w_target: V3,
    axis0: V3,
    axis_perp: V3,
    axis_wob: f32,
    axis_rate: f32,
    speed: f32,
    light: V3,
    drift_t: f32,
    drift_rate: f32,
    fade: f32,
    fade_target: f32,
    pending: Option<&'static str>,
    // grid
    pub cols: usize,
    pub rows: usize,
    pub cw: f32,
    pub ch: f32,
    pub screen_h: f32,
    base_s: f32,
    cx: f32,
    cy: f32,
    // interaction
    zoom: f32,
    zoom_v: f32,
    zoom_target: f32,
    dragging: bool,
    drag_last: (f32, f32, f64),
    drag_idle: f32,
    w_drag: V3,
    // effects
    ripples: Vec<Ripple>,
    wake: Vec<f32>,
    wake_max: f32,
    spin_phase: f32,
    spin_e: f32,
    flash: f32,
    particles: Vec<Particle>,
    reroll_t: f32,
    frozen: bool,
    // audio
    a_bass: f32,
    a_mid: f32,
    a_high: f32,
    a_loud: f32,
    last_onset: u64,
    last_hit: f32,
    pub last_onset_capture_s: f64,
    // buffers
    obj: Vec<ObjCell>,
    scratch: Scratch,
    h1: Vec<f32>,
    h2: Vec<f32>,
    glyph: Vec<u16>,
    level: Vec<f32>,
    tint: Vec<f32>,
    emis: Vec<f32>,
    glow: Vec<f32>,
    tmp: Vec<f32>,
    pub cells: Vec<u16>,
    prev: Vec<u16>,
    pub time: f32,
    // OLED care
    home: (f32, f32),
    wander_amp: (f32, f32),
    wander_ph: [f32; 5],
    wander_s: f32,
    /// 0..1, eased toward 1 while OLED care is on (so toggling never jumps)
    pub oled_k: f32,
    reroll_c0: (f32, f32),
    reroll_boost: f32,
    reroll_dur: f32,
    pub fx: Fx,
    obj2: Vec<ObjCell>,
    last_p: Option<Params>,
}

impl Scene {
    pub fn new(seed: u64, shape: Option<&'static str>) -> Scene {
        let mut rng = Rng::new(seed);
        let name = shape.unwrap_or_else(|| rng.pick(&SHAPES));
        let sh = Shape::new(name, &mut rng);
        let mut s = Scene {
            rng,
            shape: sh,
            shape_lock: shape,
            geom: Geom::new(),
            geom_age: 1e9,
            geom_stale: true,
            built_ppu: 0.0,
            q: [1.0, 0.0, 0.0, 0.0],
            w: [0.0; 3],
            w_mid: [0.0; 3],
            w_target: [0.0; 3],
            axis0: [1.0, 0.0, 0.0],
            axis_perp: [0.0, 1.0, 0.0],
            axis_wob: 0.2,
            axis_rate: 0.07,
            speed: 0.3,
            light: [0.0, 0.6, 0.8],
            drift_t: 0.0,
            drift_rate: 1.0,
            fade: 0.0,
            fade_target: 1.0,
            pending: None,
            cols: 0,
            rows: 0,
            cw: 1.0,
            ch: 2.0,
            screen_h: 1.0,
            base_s: 1.0,
            cx: 0.0,
            cy: 0.0,
            zoom: 1.0,
            zoom_v: 0.0,
            zoom_target: 1.0,
            dragging: false,
            drag_last: (0.0, 0.0, 0.0),
            drag_idle: 0.0,
            w_drag: [0.0; 3],
            ripples: Vec::new(),
            wake: Vec::new(),
            wake_max: 0.0,
            spin_phase: 0.0,
            spin_e: 0.0,
            flash: 0.0,
            particles: Vec::new(),
            reroll_t: 0.0,
            frozen: false,
            a_bass: 0.0,
            a_mid: 0.0,
            a_high: 0.0,
            a_loud: 0.0,
            last_onset: 0,
            last_hit: -10.0,
            last_onset_capture_s: 0.0,
            obj: Vec::new(),
            scratch: Scratch::default(),
            h1: Vec::new(),
            h2: Vec::new(),
            glyph: Vec::new(),
            level: Vec::new(),
            tint: Vec::new(),
            emis: Vec::new(),
            glow: Vec::new(),
            tmp: Vec::new(),
            cells: Vec::new(),
            prev: Vec::new(),
            time: 0.0,
            home: (0.0, 0.0),
            wander_amp: (0.0, 0.0),
            wander_ph: [0.0; 5],
            wander_s: 1.0,
            oled_k: 1.0,
            reroll_c0: (0.0, 0.0),
            reroll_boost: 1.0,
            reroll_dur: REROLL_DUR,
            fx: Fx::new(),
            obj2: Vec::new(),
            last_p: None,
        };
        for k in 0..5 {
            s.wander_ph[k] = s.rng.uniform(0.0, 2.0 * std::f32::consts::PI);
        }
        s.init_motion(true);
        s
    }

    fn init_motion(&mut self, moving: bool) {
        let rng = &mut self.rng;
        let a = loop {
            let a = norm([rng.gauss(0.0, 1.0), rng.gauss(0.0, 1.0), rng.gauss(0.0, 0.5)]);
            if a[2].abs() < 0.7 {
                break a;
            }
        };
        self.axis0 = a;
        self.axis_perp = norm(cross(a, [0.0, 0.0, 1.0]));
        self.axis_wob = rng.uniform(0.15, 0.3);
        self.axis_rate = rng.uniform(0.05, 0.09);
        self.speed = rng.uniform(0.26, 0.42) * rng.sign();
        let (lx, ly) = (rng.uniform(-0.8, 0.8), rng.uniform(0.35, 0.9));
        self.light = norm([lx, ly, 0.9]);
        let turn = rng.uniform(0.0, 2.0 * std::f32::consts::PI);
        self.q = q_mul(q_axis(a, turn), rest_quat(&self.shape));
        if moving {
            self.w = scale(a, self.speed);
        } else {
            self.w = [0.0; 3];
        }
        self.w_mid = self.w;
        self.drift_t = rng.uniform(0.0, 100.0);
        self.geom_stale = true;
        self.built_ppu = 0.0;
    }

    /// Lay out the grid: `cols` x `rows` cells of `cw` x `ch` pixels on a screen `screen_h` pixels tall.
    pub fn set_grid(&mut self, cols: usize, rows: usize, cw: f32, ch: f32, screen_h: f32) {
        self.cols = cols;
        self.rows = rows;
        self.cw = cw;
        self.ch = ch;
        self.screen_h = screen_h;
        self.home = (cols as f32 * cw / 2.0, rows as f32 * ch / 2.0);
        // the object's silhouette spans roughly half the screen height at zoom 1
        self.base_s = 0.27 * screen_h;
        // OLED care wander range: as wide as the screen allows with the object fully on screen
        let w = (cols as f32 - 1.0) * cw;
        self.wander_amp = ((w / 2.0 - 0.36 * screen_h).max(0.0), 0.16 * screen_h);
        self.update_wander();
        let n = cols * rows;
        self.h1 = (0..n).map(|i| (hash2(i as u32, 17) & 0xffff) as f32 / 65535.0).collect();
        self.h2 = (0..n).map(|i| (hash2(i as u32, 91) & 0xffff) as f32 / 65535.0).collect();
        for v in [&mut self.level, &mut self.tint, &mut self.emis, &mut self.glow, &mut self.tmp, &mut self.wake] {
            v.clear();
            v.resize(n, 0.0);
        }
        self.glyph.clear();
        self.glyph.resize(n, 0);
        self.fx.resize(n);
        self.cells.clear();
        self.cells.resize(n * 4, 0);
        self.prev.clear();
        self.wake_max = 0.0;
        self.particles.clear();
        self.frozen = false;
        self.built_ppu = 0.0;
        self.geom_stale = true;
    }

    fn view(&self) -> View {
        let breath = 1.0 + 0.07 * self.a_bass;
        View { cols: self.cols, rows: self.rows, cw: self.cw, ch: self.ch, cx: self.cx, cy: self.cy,
               s: self.base_s * self.zoom * breath * self.wander_s }
    }

    /// OLED care: the object's centre and size wander slowly over most of the screen (sums of sines with
    /// incommensurate periods of 5-15 minutes: smooth, velocity-continuous, never repeating exactly).
    fn update_wander(&mut self) {
        let t = self.time;
        let tau = 2.0 * std::f32::consts::PI;
        let ph = self.wander_ph;
        let x = 0.62 * (tau * t / 431.0 + ph[0]).sin() + 0.38 * (tau * t / 677.0 + ph[1]).sin();
        let y = 0.6 * (tau * t / 337.0 + ph[2]).sin() + 0.4 * (tau * t / 893.0 + ph[3]).sin();
        let s = 0.91 + 0.09 * (tau * t / 563.0 + ph[4]).sin();
        let k = self.oled_k;
        self.cx = self.home.0 + k * self.wander_amp.0 * x;
        self.cy = self.home.1 + k * self.wander_amp.1 * y;
        self.wander_s = 1.0 + k * (s - 1.0);
    }

    /// OLED care: the background field's dots live ~4 minutes each (smooth fade in/out) and then reappear
    /// elsewhere, so no field pixel stays lit. Returns (selection hash, envelope 0..1, glyph hash).
    fn field_cell(&self, i: usize, oled: bool) -> (f32, f32, f32) {
        if !oled {
            return (self.h1[i], 1.0, self.h2[i]);
        }
        let period = 240.0;
        let x = (self.time + self.h2[i] * period) / period;
        let ep = x.floor();
        let u = x - ep;
        let h = hash2(i as u32 ^ 0x5bd1_e995, ep as u32);
        let env = (std::f32::consts::PI * u).sin();
        ((h & 0xffff) as f32 / 65535.0, env * env, (h >> 16) as f32 / 65535.0)
    }

    /// The glyph and level layers of the last composed frame (for the brightness limiter).
    pub fn layers(&self) -> (&[u16], &[f32]) {
        (&self.glyph, &self.level)
    }

    pub fn is_busy_rerolling(&self) -> bool {
        !self.particles.is_empty() || self.fade_target == 0.0 || self.dragging
    }

    // ------------------------------------------------------------------ input

    pub fn drag_start(&mut self, x: f32, y: f32, t: f64, four_d: bool) {
        self.fx.drag4d = four_d && self.shape.is_4d;
        self.dragging = true;
        self.drag_last = (x, y, t);
        self.drag_idle = 0.0;
        self.w_drag = self.w;
    }

    pub fn drag_move(&mut self, x: f32, y: f32, t: f64, effects: f32) {
        if !self.dragging {
            return;
        }
        let (lx, ly, lt) = self.drag_last;
        let (dx, dy) = (x - lx, y - ly);
        let d = (dx * dx + dy * dy).sqrt();
        if d < 0.5 {
            return;
        }
        let dt = ((t - lt) as f32).max(0.004);
        if self.fx.drag4d {
            // shift + middle-drag on a 4D shape turns it through the xw / yw planes instead
            let (ax, ay) = (dx / (0.25 * self.screen_h), dy / (0.25 * self.screen_h));
            self.shape.user4.0 += ax;
            self.shape.user4.1 += ay;
            let k = 1.0 - (-dt / 0.05).exp();
            self.fx.w4.0 += ((ax / dt).clamp(-8.0, 8.0) - self.fx.w4.0) * k;
            self.fx.w4.1 += ((ay / dt).clamp(-8.0, 8.0) - self.fx.w4.1) * k;
            self.drag_last = (x, y, t);
            return;
        }
        let ang = d / (0.25 * self.screen_h);
        let axis = norm([dy, dx, 0.0]);
        if !self.frozen {
            self.q = q_norm(q_mul(q_axis(axis, ang), self.q));
        }
        let w_inst = scale(axis, (ang / dt).min(14.0));
        let k = 1.0 - (-dt / 0.05).exp();
        self.w_drag = add(self.w_drag, scale(sub(w_inst, self.w_drag), k));
        self.w = self.w_drag;
        self.w_mid = self.w_drag;
        self.drag_idle = 0.0;
        // the wake: splat along the segment, stronger for faster strokes
        if effects > 0.0 {
            let v = d / dt;
            let strength = effects * (v / (1.2 * self.screen_h)).clamp(0.0, 1.0) * 0.9;
            if strength > 0.01 {
                let steps = (d / self.cw).ceil().max(1.0) as usize;
                for i in 0..steps {
                    let f = (i as f32 + 0.5) / steps as f32;
                    self.splat(lx + dx * f, ly + dy * f, strength / steps as f32 * 2.2);
                }
            }
        }
        self.drag_last = (x, y, t);
    }

    pub fn drag_end(&mut self) {
        if !self.dragging {
            return;
        }
        self.dragging = false;
        self.fx.drag4d = false;
        // inertia: carry the measured velocity, then the smoothers ease it back into the drift
        self.w = self.w_drag;
        self.w_mid = self.w_drag;
    }

    /// Set the spin velocity directly (tests: trails).
    pub fn kick(&mut self, w: V3) {
        self.w = w;
        self.w_mid = w;
    }

    pub fn wheel(&mut self, clicks: f32) {
        self.zoom_target = (self.zoom_target * 1.12f32.powf(clicks)).clamp(0.45, 2.2);
    }

    pub fn ripple(&mut self, x: f32, y: f32, amp: f32) {
        if self.ripples.len() >= 8 {
            self.ripples.remove(0);
        }
        self.ripples.push(Ripple { x, y, age: 0.0, amp, r0: 0.0 });
    }

    fn splat(&mut self, x: f32, y: f32, amount: f32) {
        let sigma = 0.022 * self.screen_h;
        let rc = ((3.0 * sigma) / self.cw).ceil() as i32;
        let rr = ((3.0 * sigma) / self.ch).ceil() as i32;
        let (gx, gy) = ((x / self.cw) as i32, (y / self.ch) as i32);
        let inv = 1.0 / (2.0 * sigma * sigma);
        for cy in (gy - rr).max(0)..(gy + rr + 1).min(self.rows as i32) {
            for cx in (gx - rc).max(0)..(gx + rc + 1).min(self.cols as i32) {
                let px = (cx as f32 + 0.5) * self.cw - x;
                let py = (cy as f32 + 0.5) * self.ch - y;
                let v = amount * (-(px * px + py * py) * inv).exp();
                let i = cy as usize * self.cols + cx as usize;
                self.wake[i] = (self.wake[i] + v).min(1.4);
                self.wake_max = self.wake_max.max(self.wake[i]);
            }
        }
    }

    pub fn shape_name(&self) -> &'static str {
        self.shape.name
    }

    /// Dissolve into glyphs that scatter and re-form as a new shape (or a quick fade when effects are off).
    /// `tr` None picks the transition by the weights (a quick fade when effects are off).
    pub fn reroll(&mut self, name: Option<&'static str>, effects: f32, tr: Option<Transition>) {
        let name = name.unwrap_or_else(|| {
            let others: Vec<&'static str> = SHAPES.iter().copied().filter(|s| *s != self.shape.name).collect();
            self.rng.pick(&others)
        });
        let w = self.last_p.map(|p| p.weights).unwrap_or_default();
        let tr = tr.unwrap_or_else(|| if effects > 0.0 { fx::pick_transition(&mut self.rng, &w) } else { Transition::Fade });
        let t = self.time;
        self.fx.reroll(&mut self.rng, &w, t);
        let morph = tr == Transition::Morph;
        if tr == Transition::Fade || (effects <= 0.0 && !morph) || self.obj.is_empty() || self.cols == 0 {
            self.pending = Some(name);
            self.fade_target = 0.0;
            return;
        }
        // a morph glides straight from cell to cell without a flash; a dissolve scatters with an HDR flash
        self.reroll_boost = if morph { 0.0 } else { 1.0 };
        self.reroll_dur = if morph { 2.0 } else { REROLL_DUR };
        let cols = self.cols;
        let (cw, ch) = (self.cw, self.ch);
        let pos = |idx: u32| -> (f32, f32) {
            (((idx as usize % cols) as f32 + 0.5) * cw, ((idx as usize / cols) as f32 + 0.5) * ch)
        };
        // sources: wherever particles currently are (mid-reroll) or the object's cells
        let mut src: Vec<((f32, f32), u16, f32)> = if !self.particles.is_empty() {
            let t = self.reroll_t;
            self.particles.iter().filter_map(|p| {
                let (x, y, g, l) =
                    particle_at(p, t, 0.0, (self.cx - self.reroll_c0.0, self.cy - self.reroll_c0.1), self.reroll_dur);
                if g == 0 { None } else { Some(((x, y), g, l.min(5.0))) }
            }).collect()
        } else {
            self.obj.iter().map(|c| (pos(c.idx), c.glyph, c.level)).collect()
        };
        if src.is_empty() {
            src.push(((self.cx, self.cy), 2, 3.0));
        }
        // the new shape, frozen at its start pose until the glyphs arrive
        self.shape = Shape::new(name, &mut self.rng);
        self.init_motion(false);
        self.frozen = true;
        self.fade = 1.0;
        self.fade_target = 1.0;
        let p = self.last_p;
        self.render_object(1.0, 0.0, p.as_ref());
        let dst: Vec<((f32, f32), u16, f32)> = self.obj.iter().map(|c| (pos(c.idx), c.glyph, c.level)).collect();
        let (cx, cy) = (self.cx, self.cy);
        let ang = |p: &(f32, f32)| (p.1 - cy).atan2(p.0 - cx);
        src.sort_by(|a, b| ang(&a.0).total_cmp(&ang(&b.0)));
        let mut dst_sorted = dst.clone();
        dst_sorted.sort_by(|a, b| ang(&a.0).total_cmp(&ang(&b.0)));
        self.particles.clear();
        let mut used = vec![false; src.len()];
        let h = self.screen_h;
        for (i, d) in dst_sorted.iter().enumerate() {
            let si = i * src.len() / dst_sorted.len().max(1);
            used[si] = true;
            let s = src[si];
            let c = if morph { self.morph_point(s.0, d.0, h) } else { self.scatter_point(s.0, h) };
            let delay = self.rng.uniform(0.0, if morph { 0.3 } else { 0.18 });
            self.particles.push(Particle { p0: s.0, c, p1: d.0, g0: s.1, g1: d.1, l0: s.2, l1: d.2, delay,
                                           fade_out: false });
        }
        for (i, s) in src.iter().enumerate() {
            if !used[i] {
                let (c, end) = if morph {
                    let end = (s.0.0 + (self.cx - s.0.0) * 0.3, s.0.1 + (self.cy - s.0.1) * 0.3);
                    (((s.0.0 + end.0) / 2.0, (s.0.1 + end.1) / 2.0), end)
                } else {
                    let c = self.scatter_point(s.0, h);
                    (c, (s.0.0 + (c.0 - s.0.0) * 1.4, s.0.1 + (c.1 - s.0.1) * 1.4))
                };
                let delay = self.rng.uniform(0.0, 0.18);
                self.particles.push(Particle { p0: s.0, c, p1: end, g0: s.1, g1: s.1, l0: s.2, l1: 0.0, delay,
                                               fade_out: true });
            }
        }
        self.reroll_t = 0.0;
        self.reroll_c0 = (self.cx, self.cy);
        if !morph {
            self.ripple(self.cx, self.cy, 0.7 * effects);
        }
        self.obj.clear();
    }

    fn morph_point(&mut self, a: (f32, f32), b: (f32, f32), h: f32) -> (f32, f32) {
        // a gentle bow off the straight line, so the glyphs swirl a little on their way
        let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = (dx * dx + dy * dy).sqrt().max(1.0);
        let k = self.rng.gauss(0.0, 0.05) * h / l;
        (mx - dy * k, my + dx * k)
    }

    fn scatter_point(&mut self, p: (f32, f32), h: f32) -> (f32, f32) {
        let d = norm([p.0 - self.cx + self.rng.gauss(0.0, 5.0), p.1 - self.cy + self.rng.gauss(0.0, 5.0), 0.0]);
        let a = self.rng.uniform(-0.6, 0.6);
        let (s, c) = a.sin_cos();
        let dir = (d[0] * c - d[1] * s, d[0] * s + d[1] * c);
        let dist = self.rng.uniform(0.3, 0.85) * h;
        (p.0 + dir.0 * dist, p.1 + dir.1 * dist)
    }

    // ------------------------------------------------------------------ step

    /// Advance by `dt`. Returns true while anything is moving faster than the idle drift (drives the frame rate).
    pub fn step(&mut self, dt: f32, p: &Params, audio: &AudioFrame) -> bool {
        self.time += dt;
        let e = |rate: f32| 1.0 - (-dt * rate).exp();
        self.oled_k += ((if p.oled { 1.0 } else { 0.0 }) - self.oled_k) * e(0.5);
        self.update_wander();
        self.last_p = Some(*p);
        let auto_modes = p.style == Style::Auto;
        let fx_busy = self.fx.step(dt, self.time, &mut self.rng, &p.weights, auto_modes, p.effects);
        self.shape.user4.0 += self.fx.w4.0 * dt;
        self.shape.user4.1 += self.fx.w4.1 * dt;
        let ag0 = if audio.active { p.audio } else { 0.0 };
        for i in 0..8 {
            self.fx.spec[i] += (audio.spec[i] * ag0 - self.fx.spec[i]) * e(10.0);
            if self.fx.spec[i] < 1e-3 {
                self.fx.spec[i] = 0.0;
            }
        }

        // audio envelopes (already smoothed in the analyser; a second gentle smoother keeps motion velocity-smooth)
        let ag = if audio.active { p.audio } else { 0.0 };
        self.a_bass += (audio.bass * ag - self.a_bass) * e(14.0);
        self.a_mid += (audio.mid * ag - self.a_mid) * e(4.0);
        self.a_high += (audio.high * ag - self.a_high) * e(6.0);
        self.a_loud += (audio.loud * ag - self.a_loud) * e(1.5);
        if audio.onset_count != self.last_onset {
            self.last_onset = audio.onset_count;
            let s = audio.onset_strength;
            if ag > 0.0 && s > 0.25 && self.time - self.last_hit > 0.33 {
                self.last_hit = self.time;
                self.last_onset_capture_s = audio.onset_capture_s;
                if s > 0.65 && p.effects > 0.0 {
                    let r0 = self.base_s * self.zoom * 0.9;
                    if self.ripples.len() >= 8 {
                        self.ripples.remove(0);
                    }
                    self.ripples.push(Ripple { x: self.cx, y: self.cy, age: 0.0, amp: 0.55 * ag * s.min(1.0), r0 });
                }
                self.flash = self.flash.max(s.min(1.0) * ag);
            }
        }
        self.flash *= (-dt / 0.28).exp();
        if self.flash < 0.005 {
            self.flash = 0.0;
        }

        // first appearance / simple reroll fade
        if self.fade_target == 0.0 {
            self.fade = (self.fade - dt / 0.32).max(0.0);
            if self.fade <= 0.0 {
                if let Some(name) = self.pending.take() {
                    self.shape = Shape::new(name, &mut self.rng);
                    self.init_motion(true);
                    self.fade_target = 1.0;
                }
            }
        } else {
            self.fade = (self.fade + dt / 0.6).min(1.0);
        }

        // reroll particles
        if !self.particles.is_empty() {
            self.reroll_t += dt;
            if self.reroll_t >= self.reroll_dur {
                self.particles.clear();
                self.frozen = false;
            }
        }

        // drift
        let target_drift = if self.frozen { 0.0 } else { 1.0 + 2.2 * (0.6 * self.a_mid + 0.4 * self.a_high) };
        self.drift_rate += (target_drift - self.drift_rate) * e(1.2);
        self.drift_t += dt * self.drift_rate;

        // spin: two cascaded exponential smoothers, so velocity and acceleration stay continuous
        let ang = self.drift_t * self.axis_rate;
        let axis = norm(add(add(self.axis0, scale(self.axis_perp, self.axis_wob * ang.sin())),
                            scale(cross(self.axis0, self.axis_perp), 0.5 * self.axis_wob * (1.3 * ang).cos())));
        let mult = if self.frozen { 0.0 } else { 1.0 + 1.4 * self.a_loud };
        self.w_target = scale(axis, self.speed * mult);
        if self.dragging {
            self.drag_idle += dt;
            if self.drag_idle > 0.06 {
                // holding still mid-drag: the measured velocity bleeds off
                self.w_drag = scale(self.w_drag, (-dt / 0.06).exp());
                self.w = self.w_drag;
                self.w_mid = self.w_drag;
            }
        } else {
            let r = if self.frozen { 3.0 } else { 1.6 };
            self.w_mid = add(self.w_mid, scale(sub(self.w_target, self.w_mid), e(r)));
            self.w = add(self.w, scale(sub(self.w_mid, self.w), e(r)));
            if !self.frozen {
                self.q = q_norm(q_mul(q_from_rotvec(scale(self.w, dt)), self.q));
            }
        }

        // zoom: critically damped spring
        let wn = 7.0;
        let acc = wn * wn * (self.zoom_target - self.zoom) - 2.0 * wn * self.zoom_v;
        self.zoom_v += acc * dt;
        self.zoom += self.zoom_v * dt;

        // effects decay
        for r in self.ripples.iter_mut() {
            r.age += dt;
        }
        self.ripples.retain(|r| r.amp * (-r.age / 1.1).exp() > 0.01 && r.age < 3.5);
        if self.wake_max > 0.0 {
            let k = (-dt / 0.6).exp();
            let mut mx = 0f32;
            for v in self.wake.iter_mut() {
                *v *= k;
                mx = mx.max(*v);
            }
            self.wake_max = mx;
            if mx < 0.004 {
                self.wake.iter_mut().for_each(|v| *v = 0.0);
                self.wake_max = 0.0;
            }
        }
        let wl = len(self.w);
        let tl = len(self.w_target);
        let excess = (wl - tl * 1.15 - 0.15).max(0.0);
        self.spin_e = (excess / 3.0).clamp(0.0, 1.0) * p.effects;
        if self.spin_e < 0.003 {
            self.spin_e = 0.0;
        }
        self.spin_phase += dt * wl * 1.5 * if self.w[2] >= 0.0 { 1.0 } else { -1.0 };

        let spin_busy = len(sub(self.w, self.w_target)) > 0.02;
        let zoom_busy = (self.zoom - self.zoom_target).abs() > 1e-3 || self.zoom_v.abs() > 1e-3;
        fx_busy || self.dragging || spin_busy || zoom_busy || !self.ripples.is_empty() || self.wake_max > 0.0
            || !self.particles.is_empty() || self.flash > 0.0 || self.fade < 1.0 || self.spin_e > 0.0 || ag > 0.0
    }

    /// Rasterise the object into `self.obj` (and `self.obj2` while two render modes dissolve into each other).
    fn render_object(&mut self, fade: f32, shimmer: f32, p: Option<&Params>) {
        let v = self.view();
        // sampling density follows the on-screen size; rebuild when it moved by more than ~12%
        let ppu = raster::ppu(&v);
        if self.built_ppu <= 0.0 || (ppu / self.built_ppu - 1.0).abs() > 0.12 {
            self.shape.build(ppu);
            self.built_ppu = ppu;
            self.geom_stale = true;
        }
        if self.geom_stale {
            self.shape.geom(self.drift_t, &mut self.geom);
            self.geom_stale = false;
            self.geom_age = 0.0;
        }
        let rot = q_to_mat(self.q);
        let style = p.map(|p| p.style).unwrap_or(Style::Auto);
        let audio_on = p.map(|p| p.audio > 0.0).unwrap_or(false);
        let displaced = self.fx.displace(&self.geom, audio_on);
        let kind = self.shape.kind;
        // render mode: forced by the style setting, else the combination's mode (dissolving when it changes)
        let (to, from, f) = match style {
            Style::Ramp => (Mode::Solid, Mode::Solid, 1.0),
            Style::Braille => (Mode::Wire, Mode::Wire, 1.0),
            Style::Auto => (self.fx.mode, self.fx.from, sstep(self.fx.blend)),
        };
        let slice = self.fx.slice();
        let mut obj = std::mem::take(&mut self.obj);
        let mut obj2 = std::mem::take(&mut self.obj2);
        obj2.clear();
        let passes: Vec<(Mode, Option<(f32, bool)>)> =
            if f >= 1.0 || to == from { vec![(to, None)] } else { vec![(from, Some((f, false))), (to, Some((f, true)))] };
        for (pi, (mode, cut)) in passes.iter().enumerate() {
            let src = if displaced { &self.fx.work } else { &self.geom };
            let needs_subset = cut.is_some() || matches!((kind, mode), (Kind::Surface, Mode::Wire | Mode::Points)
                                                         | (Kind::Lines, Mode::Points));
            let (g, rkind) = if needs_subset {
                let mut sub = std::mem::replace(&mut self.fx.sub[pi], Geom::new());
                let rk = fx::mode_subset(src, kind, *mode, *cut, &mut sub);
                self.fx.sub[pi] = sub;
                (&self.fx.sub[pi], rk)
            } else {
                (src, kind)
            };
            let pass_style = match (rkind, mode) {
                (Kind::Lines, Mode::Solid) | (Kind::Glyphs, Mode::Solid) => Style::Ramp,
                (Kind::Glyphs, Mode::Wire) => Style::Braille,
                _ => Style::Auto,
            };
            let ex = Extras { kind: (rkind != kind).then_some(rkind), slice };
            let out = if pi == 0 { &mut obj } else { &mut obj2 };
            raster::frame(&self.shape, g, &rot, self.light, fade, &v, pass_style, shimmer, self.time, &ex,
                          &mut self.scratch, out);
        }
        self.obj = obj;
        self.obj2 = obj2;
    }

    /// Build this frame's cell buffer. Returns true when it differs from the last one.
    pub fn compose(&mut self, dt: f32, p: &Params) -> bool {
        let n = self.cols * self.rows;
        if n == 0 {
            return false;
        }
        // geometry follows the slow drift: rebuild at ~8 Hz
        self.geom_age += dt;
        if self.geom_age >= 0.12 {
            self.geom_stale = true;
        }

        self.glyph.iter_mut().for_each(|g| *g = 0);
        self.level.iter_mut().for_each(|l| *l = 0.0);
        self.tint.iter_mut().for_each(|t| *t = 0.0);
        let mut any_emis = false;

        // background field
        let eg = p.effects;
        if eg > 0.0 {
            let active = !self.ripples.is_empty() || self.wake_max > 0.0 || self.spin_e > 0.0
                || self.a_bass > 0.01 || self.a_high > 0.01;
            if !active {
                for i in 0..n {
                    let (hv, env, h2) = self.field_cell(i, p.oled);
                    if hv < FIELD_DENSITY && env > 0.04 {
                        self.glyph[i] = if h2 < 0.6 { 1 } else { 2 };
                        self.level[i] = FIELD_REST_LEVEL * env;
                    }
                }
            } else {
                any_emis |= self.field_energy(eg, p.oled);
            }
        }

        // the object (hidden while its glyphs are in flight)
        if self.particles.is_empty() {
            let fade = sstep(self.fade);
            self.render_object(fade, self.a_high * 0.9, Some(p));
            // the front band sits just above level 5 (the shader scales it by the highlight setting);
            // OLED care drops this at-rest highlight entirely
            let pop = if p.oled { 0.0 } else { 0.6 };
            let density = self.fx.ambient == fx::Ambient::Density && p.style == Style::Auto;
            let obj2 = std::mem::take(&mut self.obj2);
            let n_first = self.obj.len();
            for (ci, c) in self.obj.iter().chain(obj2.iter()).enumerate() {
                let i = c.idx as usize;
                if density {
                    // depth cue through glyph density: dimmer (farther) cells thin out
                    let keep = 0.35 + 0.65 * ((c.level - 1.0) / 4.0);
                    if (hash2(i as u32, 7) & 0xffff) as f32 / 65535.0 > keep {
                        continue;
                    }
                }
                if ci >= n_first && self.glyph[i] != 0 && self.level[i] > c.level && c.level < 5.0 {
                    continue; // the second render-mode pass doesn't overwrite a brighter first-pass cell
                }
                let mut l = c.level;
                if l >= 5.0 {
                    l += pop + self.flash * 1.6;
                } else if l >= 4.0 {
                    l += self.flash * 0.7;
                }
                self.glyph[i] = c.glyph;
                self.level[i] = l * fade.max(0.0);
                self.tint[i] = 0.0;
                if c.hot {
                    // the slicing plane's cross-section: amber rim
                    self.level[i] = self.level[i].max(4.4);
                    self.tint[i] = 0.55;
                }
                if self.flash > 0.05 && c.level >= 5.0 {
                    self.emis[i] = self.emis[i].max(self.flash * 0.5);
                    any_emis = true;
                }
            }
            self.obj2 = obj2;
            self.trails(dt, p.effects);
        } else {
            let t = self.reroll_t;
            let boost = 1.7 * eg * self.reroll_boost;
            // targets follow the wandering centre so the glyphs land exactly on the live object
            let shift = (self.cx - self.reroll_c0.0, self.cy - self.reroll_c0.1);
            for k in 0..self.particles.len() {
                let (x, y, g, l) = particle_at(&self.particles[k], t, boost, shift, self.reroll_dur);
                if g == 0 || l <= 0.02 {
                    continue;
                }
                let (cx, cy) = ((x / self.cw).floor() as i32, (y / self.ch).floor() as i32);
                if cx < 0 || cy < 0 || cx >= self.cols as i32 || cy >= self.rows as i32 {
                    continue;
                }
                let i = cy as usize * self.cols + cx as usize;
                if l >= self.level[i] {
                    self.glyph[i] = g;
                    self.level[i] = l;
                    if l > 5.0 {
                        self.emis[i] = self.emis[i].max((l - 5.0) * 0.4);
                        any_emis = true;
                    }
                }
            }
        }

        // glow: separable blur of the emissive layer at cell resolution (cells are 2:1, so wider horizontally)
        let glow_on = any_emis || self.glow.iter().any(|g| *g > 0.0);
        if any_emis {
            blur(&self.emis, &mut self.tmp, &mut self.glow, self.cols, self.rows, 2.2, 1.1);
            self.emis.iter_mut().for_each(|v| *v = 0.0);
        } else if glow_on {
            self.glow.iter_mut().for_each(|v| *v = 0.0);
        }

        // pack
        for i in 0..n {
            let o = i * 4;
            self.cells[o] = f32_to_f16(self.glyph[i] as f32);
            self.cells[o + 1] = f32_to_f16(self.level[i]);
            self.cells[o + 2] = f32_to_f16(self.tint[i]);
            self.cells[o + 3] = f32_to_f16(self.glow[i]);
        }
        let changed = self.prev != self.cells;
        if changed {
            self.prev.clone_from(&self.cells);
        }
        changed
    }

    /// Field energy: ripples, wake, spin push and the audio pulse. Returns true when anything emits glow.
    fn field_energy(&mut self, eg: f32, oled: bool) -> bool {
        let h = self.screen_h;
        let (cw, ch) = (self.cw, self.ch);
        let obj_r = self.base_s * self.zoom;
        let ring_r = obj_r * 1.2;
        let ring_w = obj_r * 0.2;
        let tick = (self.time * 10.0) as u32;
        let mut any = false;
        let rip: Vec<(f32, f32, f32, f32, f32)> = self
            .ripples
            .iter()
            .map(|r| {
                let front = r.r0 + 0.9 * h * r.age;
                let sigma = 0.03 * h + 0.02 * h * r.age;
                let a = r.amp * sstep(r.age / 0.06) * (-r.age / 1.1).exp();
                (r.x, r.y, front, sigma, a)
            })
            .collect();
        for cy in 0..self.rows {
            let py = (cy as f32 + 0.5) * ch;
            for cx in 0..self.cols {
                let i = cy * self.cols + cx;
                let px = (cx as f32 + 0.5) * cw;
                let mut e = self.wake[i];
                for &(rx, ry, front, sigma, a) in &rip {
                    let d = ((px - rx).powi(2) + (py - ry).powi(2)).sqrt();
                    let x = (d - front) / sigma;
                    if x.abs() < 3.0 {
                        e += a * (-x * x).exp() / (1.0 + d / h);
                    }
                }
                let (ox, oy) = (px - self.cx, py - self.cy);
                let r = (ox * ox + oy * oy).sqrt();
                if self.spin_e > 0.0 {
                    let x = (r - ring_r) / ring_w;
                    if x.abs() < 3.0 {
                        let th = oy.atan2(ox);
                        e += self.spin_e * 1.1 * (-x * x).exp() * (0.55 + 0.45 * (5.0 * th - self.spin_phase).cos());
                    }
                }
                if self.a_bass > 0.01 {
                    e += self.a_bass * 0.3 * (-(r / (0.6 * h)).powi(2)).exp();
                }
                if self.a_high > 0.01 {
                    e += self.a_high * 0.12 * ((hash2(i as u32, tick) & 0xff) as f32 / 255.0);
                }
                let e = e * eg;
                let (h1, env, h2) = self.field_cell(i, oled);
                if h1 < FIELD_DENSITY * env.sqrt() + 0.6 * e.min(1.0) {
                    let gi = if e < 0.02 {
                        if h2 < 0.6 { 1 } else { 2 }
                    } else {
                        1 + ((e * (0.65 + 0.35 * h2)).min(1.0) * 8.0) as u16
                    };
                    let l = FIELD_REST_LEVEL * env + e.min(1.8) * 4.4;
                    self.glyph[i] = gi.clamp(1, 9);
                    self.level[i] = l;
                    self.tint[i] = ((e - 1.1) * 0.8).clamp(0.0, 0.6);
                    if l > 4.8 {
                        self.emis[i] = (l - 4.8) * 0.45;
                        any = true;
                    }
                }
            }
        }
        any
    }

    /// Afterimage trails on fast spins: cells the object just left keep a fading copy of their glyph.
    fn trails(&mut self, dt: f32, effects: f32) {
        let speed = len(self.w);
        let k = if effects > 0.0 { ((speed - 1.2) / 4.0).clamp(0.0, 1.0) } else { 0.0 };
        if k == 0.0 && self.fx.trail_max == 0.0 {
            return;
        }
        let decay = (-dt / 0.12).exp();
        let mut mx = 0f32;
        for v in self.fx.trail_lv.iter_mut() {
            *v *= decay;
            mx = mx.max(*v);
        }
        if k > 0.0 {
            for c in &self.obj {
                let i = c.idx as usize;
                let v = c.level * 0.4 * k;
                if v > self.fx.trail_lv[i] {
                    self.fx.trail_lv[i] = v;
                    self.fx.trail_g[i] = c.glyph;
                    mx = mx.max(v);
                }
            }
        }
        for i in 0..self.glyph.len() {
            let v = self.fx.trail_lv[i];
            if v > 0.15 && (self.glyph[i] == 0 || self.level[i] < v) {
                self.glyph[i] = self.fx.trail_g[i];
                self.level[i] = v;
            }
        }
        self.fx.trail_max = if mx < 0.05 { 0.0 } else { mx };
        if self.fx.trail_max == 0.0 {
            self.fx.trail_lv.iter_mut().for_each(|v| *v = 0.0);
        }
    }

    pub fn object_cells(&self) -> usize {
        self.obj.len()
    }
}

/// Position, glyph and level of a reroll particle at time `t`.
fn particle_at(p: &Particle, t: f32, boost: f32, shift: (f32, f32), total: f32) -> (f32, f32, u16, f32) {
    let dur = total - p.delay;
    let u = ((t - p.delay) / dur).clamp(0.0, 1.0);
    let e = smootherstep(u);
    let a = (1.0 - e) * (1.0 - e);
    let b = 2.0 * (1.0 - e) * e;
    let c = e * e;
    let x = a * p.p0.0 + b * p.c.0 + c * (p.p1.0 + shift.0);
    let y = a * p.p0.1 + b * p.c.1 + c * (p.p1.1 + shift.1);
    let flight = (std::f32::consts::PI * e).sin().powf(1.5);
    if p.fade_out {
        let l = p.l0 * (1.0 - e) + boost * flight * (1.0 - e);
        return (x, y, p.g0, l);
    }
    let g = if e < 0.55 { p.g0 } else { p.g1 };
    let l = p.l0 + (p.l1 - p.l0) * e + boost * flight;
    (x, y, g, l)
}

fn blur(src: &[f32], tmp: &mut [f32], dst: &mut [f32], cols: usize, rows: usize, sx: f32, sy: f32) {
    let kern = |s: f32| -> Vec<f32> {
        let r = (s * 3.0).ceil() as i32;
        let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * s * s)).exp()).collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        k
    };
    let kx = kern(sx);
    let ky = kern(sy);
    let rx = (kx.len() / 2) as i32;
    let ry = (ky.len() / 2) as i32;
    for y in 0..rows {
        let row = &src[y * cols..(y + 1) * cols];
        if row.iter().all(|v| *v == 0.0) {
            tmp[y * cols..(y + 1) * cols].iter_mut().for_each(|v| *v = 0.0);
            continue;
        }
        for x in 0..cols {
            let mut acc = 0.0;
            for (j, k) in kx.iter().enumerate() {
                let xx = x as i32 + j as i32 - rx;
                if xx >= 0 && (xx as usize) < cols {
                    acc += row[xx as usize] * k;
                }
            }
            tmp[y * cols + x] = acc;
        }
    }
    for y in 0..rows {
        for x in 0..cols {
            let mut acc = 0.0;
            for (j, k) in ky.iter().enumerate() {
                let yy = y as i32 + j as i32 - ry;
                if yy >= 0 && (yy as usize) < rows {
                    acc += tmp[yy as usize * cols + x] * k;
                }
            }
            dst[y * cols + x] = if acc < 1e-3 { 0.0 } else { acc };
        }
    }
}
