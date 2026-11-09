//! Render modes and the second set of effects: wire / solid / points modes that dissolve into each other, a slicing
//! plane that sweeps through the object, afterimage trails on fast spins, an exploded view on click-hold, audio
//! displacement of the surface, a depth cue through glyph density, and 4D rotation for 4D shapes. Each reroll picks
//! a combination (shape x mode x ambient effect x transition) with weights from the settings.
//!
//! OLED care: nothing here is static. Modes change every few minutes, the sweep is a moving plane every minute or
//! two, trails and the explosion decay within a second or two, and the density dither moves with the object.

use crate::shapes::{Geom, Kind, Rng, V3, add, dot, norm, scale};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Solid,
    Wire,
    Points,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ambient {
    Plain,
    Sweep,
    Density,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Transition {
    Dissolve,
    Morph,
    Fade,
}

/// Relative weights for the reroll's random combination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    pub solid: f32,
    pub wire: f32,
    pub points: f32,
    pub plain: f32,
    pub sweep: f32,
    pub density: f32,
    pub dissolve: f32,
    pub morph: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Weights { solid: 3.0, wire: 2.0, points: 1.0, plain: 2.0, sweep: 2.0, density: 1.0, dissolve: 1.0, morph: 1.0 }
    }
}

pub fn pick<T: Copy>(rng: &mut Rng, items: &[(T, f32)]) -> T {
    let total: f32 = items.iter().map(|(_, w)| w.max(0.0)).sum();
    if total <= 0.0 {
        return items[0].0;
    }
    let mut r = rng.f() * total;
    for (v, w) in items {
        r -= w.max(0.0);
        if r <= 0.0 {
            return *v;
        }
    }
    items[items.len() - 1].0
}

pub fn pick_mode(rng: &mut Rng, w: &Weights) -> Mode {
    pick(rng, &[(Mode::Solid, w.solid), (Mode::Wire, w.wire), (Mode::Points, w.points)])
}

pub fn pick_ambient(rng: &mut Rng, w: &Weights) -> Ambient {
    pick(rng, &[(Ambient::Plain, w.plain), (Ambient::Sweep, w.sweep), (Ambient::Density, w.density)])
}

pub fn pick_transition(rng: &mut Rng, w: &Weights) -> Transition {
    pick(rng, &[(Transition::Dissolve, w.dissolve), (Transition::Morph, w.morph)])
}

pub const SWEEP_DUR: f32 = 9.0;
const MODE_BLEND: f32 = 2.5;

fn smootherstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

/// Plane offset over a sweep (u = 0..1): in from beyond the object, cut a bit past the middle, back out. Zero
/// velocity at both ends and at the turn.
pub fn sweep_offset(u: f32) -> f32 {
    let s = if u < 0.5 { smootherstep(u * 2.0) } else { smootherstep((1.0 - u) * 2.0) };
    1.1 - 1.25 * s
}

#[inline]
fn hash01(i: u32, salt: u32) -> f32 {
    let mut h = i.wrapping_mul(0x9E37_79B1) ^ salt.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 65535.0
}

pub struct Fx {
    pub mode: Mode,
    pub from: Mode,
    pub blend: f32,
    next_mode_t: f32,
    pub ambient: Ambient,
    pub sweep_t: f32,
    pub sweep_n: V3,
    next_sweep_t: f32,
    pub explode: f32,
    explode_v: f32,
    pub explode_target: f32,
    pub w4: (f32, f32),
    pub drag4d: bool,
    pub spec: [f32; 8],
    pub trail_lv: Vec<f32>,
    pub trail_g: Vec<u16>,
    pub trail_max: f32,
    pub work: Geom,
    pub sub: [Geom; 2],
    ico: Vec<V3>,
}

impl Fx {
    pub fn new() -> Fx {
        let g = (1.0 + 5f32.sqrt()) / 2.0;
        let ico = [[-1.0, g, 0.0], [1.0, g, 0.0], [-1.0, -g, 0.0], [1.0, -g, 0.0], [0.0, -1.0, g], [0.0, 1.0, g],
                   [0.0, -1.0, -g], [0.0, 1.0, -g], [g, 0.0, -1.0], [g, 0.0, 1.0], [-g, 0.0, -1.0], [-g, 0.0, 1.0]]
            .iter()
            .map(|v| norm(*v))
            .collect();
        Fx {
            mode: Mode::Solid,
            from: Mode::Solid,
            blend: 1.0,
            next_mode_t: 150.0,
            ambient: Ambient::Plain,
            sweep_t: -1.0,
            sweep_n: [0.0, 1.0, 0.0],
            next_sweep_t: 30.0,
            explode: 0.0,
            explode_v: 0.0,
            explode_target: 0.0,
            w4: (0.0, 0.0),
            drag4d: false,
            spec: [0.0; 8],
            trail_lv: Vec::new(),
            trail_g: Vec::new(),
            trail_max: 0.0,
            work: Geom::new(),
            sub: [Geom::new(), Geom::new()],
            ico,
        }
    }

    pub fn resize(&mut self, n: usize) {
        self.trail_lv.clear();
        self.trail_lv.resize(n, 0.0);
        self.trail_g.clear();
        self.trail_g.resize(n, 0);
        self.trail_max = 0.0;
    }

    /// A new combination for a new shape (no blend: the shape itself is changing).
    pub fn reroll(&mut self, rng: &mut Rng, w: &Weights, time: f32) {
        self.mode = pick_mode(rng, w);
        self.from = self.mode;
        self.blend = 1.0;
        self.next_mode_t = time + rng.uniform(120.0, 240.0);
        self.ambient = pick_ambient(rng, w);
        self.sweep_t = -1.0;
        self.next_sweep_t = time + rng.uniform(8.0, 25.0);
    }

    /// Advance. Returns true while anything here is animating faster than the idle drift.
    pub fn step(&mut self, dt: f32, time: f32, rng: &mut Rng, w: &Weights, auto_modes: bool, effects: f32) -> bool {
        // mode changes mid-life, dissolving from one into the other
        if self.blend < 1.0 {
            self.blend = (self.blend + dt / MODE_BLEND).min(1.0);
        } else if auto_modes && time >= self.next_mode_t {
            let m = pick_mode(rng, w);
            if m != self.mode {
                self.from = self.mode;
                self.mode = m;
                self.blend = 0.0;
            }
            self.next_mode_t = time + rng.uniform(120.0, 240.0);
        }
        // the slicing plane
        if self.sweep_t >= 0.0 {
            self.sweep_t += dt;
            if self.sweep_t >= SWEEP_DUR {
                self.sweep_t = -1.0;
            }
        } else if self.ambient == Ambient::Sweep && effects > 0.0 && time >= self.next_sweep_t {
            self.sweep_t = 0.0;
            let n = norm([rng.gauss(0.0, 1.0), rng.gauss(0.0, 0.6), rng.gauss(0.0, 0.5)]);
            self.sweep_n = n;
            self.next_sweep_t = time + SWEEP_DUR + rng.uniform(45.0, 90.0);
        }
        // exploded view: critically damped spring
        let wn = 6.0;
        let acc = wn * wn * (self.explode_target - self.explode) - 2.0 * wn * self.explode_v;
        self.explode_v += acc * dt;
        self.explode += self.explode_v * dt;
        if self.explode_target == 0.0 && self.explode.abs() < 1e-3 && self.explode_v.abs() < 1e-3 {
            self.explode = 0.0;
            self.explode_v = 0.0;
        }
        // 4D inertia
        if !self.drag4d {
            let k = (-dt / 0.9).exp();
            self.w4 = (self.w4.0 * k, self.w4.1 * k);
            if self.w4.0.abs() + self.w4.1.abs() < 1e-3 {
                self.w4 = (0.0, 0.0);
            }
        }
        self.blend < 1.0 || self.sweep_t >= 0.0 || self.explode != 0.0 || self.w4 != (0.0, 0.0) || self.trail_max > 0.0
    }

    pub fn slice(&self) -> Option<(V3, f32, f32)> {
        (self.sweep_t >= 0.0).then(|| (self.sweep_n, sweep_offset(self.sweep_t / SWEEP_DUR), 0.05))
    }

    /// Fill `self.work` with the geometry after the exploded view and the audio displacement.
    /// Returns false when neither applies (use the source geometry as is).
    pub fn displace(&mut self, src: &Geom, audio_on: bool) -> bool {
        let spec_on = audio_on && self.spec.iter().any(|v| *v > 0.02);
        if self.explode == 0.0 && !spec_on {
            return false;
        }
        let w = &mut self.work;
        w.pts.clear();
        w.nrm.clone_from(&src.nrm);
        w.occ.clear();
        let ex = self.explode * 0.5;
        let sector = |p: V3, ico: &[V3]| -> V3 {
            let mut best = ico[0];
            let mut bd = -2.0;
            for d in ico {
                let v = dot(p, *d);
                if v > bd {
                    bd = v;
                    best = *d;
                }
            }
            best
        };
        let spec = self.spec;
        let band = |p: V3| -> f32 {
            // spectrum bands laid around the object's z axis, interpolated (circular)
            let a = (p[1].atan2(p[0]) / (2.0 * std::f32::consts::PI) + 0.5) * 8.0;
            let i = a.floor() as i32;
            let f = a - i as f32;
            let s0 = spec[(i.rem_euclid(8)) as usize];
            let s1 = spec[((i + 1).rem_euclid(8)) as usize];
            s0 + (s1 - s0) * f
        };
        let has_n = !src.nrm.is_empty();
        for (k, p) in src.pts.iter().enumerate() {
            let mut q = *p;
            if spec_on {
                let b = band(q) * 0.08;
                q = if has_n { add(q, scale(src.nrm[k], b)) } else { add(q, scale(norm(q), b * 0.8)) };
            }
            if ex != 0.0 {
                q = add(q, scale(sector(*p, &self.ico), ex));
            }
            w.pts.push(q);
        }
        for p in &src.occ {
            let q = if ex != 0.0 { add(*p, scale(sector(*p, &self.ico), ex)) } else { *p };
            w.occ.push(q);
        }
        true
    }
}

/// Copy the points a render mode keeps into `out` (and a dissolve between modes: keep the points whose hash is
/// on this pass's side of `cut`). Returns the kind to render them as.
pub fn mode_subset(src: &Geom, kind: Kind, mode: Mode, cut: Option<(f32, bool)>, out: &mut Geom) -> Kind {
    out.pts.clear();
    out.nrm.clear();
    out.occ.clone_from(&src.occ);
    let has_n = !src.nrm.is_empty();
    // points mode: a sparse cloud of a few hundred dots, whatever the sampling density
    let np = src.pts.len().max(1) as f32;
    let (ps, pl) = ((800.0 / np).min(1.0), (600.0 / np).min(1.0));
    let (rkind, keep): (Kind, Box<dyn Fn(usize, V3) -> bool>) = match (kind, mode) {
        (Kind::Surface, Mode::Wire) => (Kind::Lines, Box::new(|_, p: V3| {
            // contour lines in object space along two axes: a mesh that reads as wireframe
            let fz = (p[2] * 4.0 + 8.0).fract();
            let fx = (p[0] * 4.0 + 8.0).fract();
            fz < 0.07 || fx < 0.07
        })),
        (Kind::Surface, Mode::Points) => (Kind::Glyphs, Box::new(move |i, _| hash01(i as u32, 7) < ps)),
        (Kind::Lines, Mode::Points) => (Kind::Glyphs, Box::new(move |i, _| hash01(i as u32, 7) < pl)),
        _ => (kind, Box::new(|_, _| true)),
    };
    for (i, p) in src.pts.iter().enumerate() {
        if let Some((c, below)) = cut {
            if (hash01(i as u32, 99) < c) != below {
                continue;
            }
        }
        if keep(i, *p) {
            out.pts.push(*p);
            if has_n {
                out.nrm.push(src.nrm[i]);
            }
        }
    }
    rkind
}
