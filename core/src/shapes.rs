//! The first eight shapes, ported from the original Python prototype. Every shape fits inside the unit sphere.
//! `geom(t)` fills points (and normals when the shape has them) in object space; `t` is the drift clock.

use std::f32::consts::PI;

pub type V3 = [f32; 3];

pub const SHAPES: [&str; 18] = ["torus", "torus_wire", "knot", "geodesic", "lattice", "mobius", "ring", "hopf",
                                "tesseract", "gyroid", "attractor", "supershape", "klein", "armillary", "metaballs",
                                "nebula", "calabi_yau", "mobius_stack"];

// ------------------------------------------------------------------ small math

#[inline]
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
pub fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
#[inline]
pub fn norm(a: V3) -> V3 {
    let n = dot(a, a).sqrt().max(1e-9);
    scale(a, 1.0 / n)
}
#[inline]
pub fn len(a: V3) -> f32 {
    dot(a, a).sqrt()
}

pub fn sstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn linspace(a: f32, b: f32, n: usize, endpoint: bool) -> Vec<f32> {
    let d = if endpoint { (n.max(2) - 1) as f32 } else { n as f32 };
    (0..n).map(|i| a + (b - a) * i as f32 / d).collect()
}

// ------------------------------------------------------------------ rng

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn f(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn uniform(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    pub fn gauss(&mut self, mu: f32, sigma: f32) -> f32 {
        let u1 = self.f().max(1e-7);
        let u2 = self.f();
        mu + sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[(self.next_u64() % xs.len() as u64) as usize]
    }
    pub fn sign(&mut self) -> f32 {
        if self.next_u64() & 1 == 0 { -1.0 } else { 1.0 }
    }
}

// ------------------------------------------------------------------ shape

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Surface,
    Lines,
    Glyphs,
}

pub struct Geom {
    pub pts: Vec<V3>,
    pub nrm: Vec<V3>, // empty when the shape has no normals
    pub occ: Vec<V3>, // occluder points (lines shapes only)
}

impl Geom {
    pub fn new() -> Geom {
        Geom { pts: Vec::new(), nrm: Vec::new(), occ: Vec::new() }
    }
    fn clear(&mut self) {
        self.pts.clear();
        self.nrm.clear();
        self.occ.clear();
    }
}

pub struct Shape {
    pub name: &'static str,
    pub kind: Kind,
    pub two_sided: bool,
    pub rest: Vec<(V3, f32)>,
    pub cull: bool,
    pub has_occluder: bool,
    pub occ_bias: f32,
    /// shapes that live in 4D (rotated by shift + middle-drag)
    pub is_4d: bool,
    /// user 4D rotation (xw, yw), radians
    pub user4: (f32, f32),
    ppu: f32,
    p: Params,
}

enum Params {
    Torus { phase: f32, rate: f32, wire: bool, u: Vec<f32>, v: Vec<f32>, ou: Vec<f32>, ov: Vec<f32> },
    Knot { pq: ((f32, f32), (f32, f32)), phase: f32, period: f32, t: Vec<f32> },
    Geodesic { phase: f32, rate: f32, freq: u32, ea: Vec<V3>, eb: Vec<V3>, s: Vec<f32> },
    Lattice { phase: f32, rate: f32, p0: Vec<V3> },
    Mobius { phase: f32, rate: f32, h: f32, u: Vec<f32>, v: Vec<f32> },
    Ring { count: usize, twists: f32, rate: f32, s: Vec<f32>, th_rail: Vec<f32> },
    Hopf { k: usize, phase: f32, rate: f32, slide: f32, t: Vec<f32> },
    X(Box<crate::shapes_x::X>),
}

impl Shape {
    pub fn new(name: &str, rng: &mut Rng) -> Shape {
        if crate::shapes_x::NAMES.contains(&name) {
            let x = crate::shapes_x::X::new(name, rng);
            return Shape {
                name: x.name,
                kind: x.kind,
                two_sided: x.two_sided,
                rest: x.rest.clone(),
                cull: false,
                has_occluder: false,
                occ_bias: 0.06,
                is_4d: x.is_4d,
                user4: (0.0, 0.0),
                ppu: 20.0,
                p: Params::X(Box::new(x)),
            };
        }
        let base = |name, kind, rest: Vec<(V3, f32)>, p| Shape {
            name,
            kind,
            two_sided: false,
            rest,
            cull: false,
            has_occluder: false,
            occ_bias: 0.06,
            is_4d: false,
            user4: (0.0, 0.0),
            ppu: 20.0,
            p,
        };
        match name {
            "torus" | "torus_wire" => {
                let wire = name == "torus_wire";
                let mut s = base(if wire { "torus_wire" } else { "torus" },
                                 if wire { Kind::Lines } else { Kind::Surface },
                                 vec![([1.0, 0.0, 0.0], -0.85)],
                                 Params::Torus { phase: rng.uniform(0.0, 2.0 * PI), rate: rng.uniform(0.18, 0.3), wire,
                                                 u: vec![], v: vec![], ou: vec![], ov: vec![] });
                s.has_occluder = wire;
                s
            }
            "knot" => {
                let pairs = [(2.0, 3.0), (3.0, 2.0), (2.0, 5.0), (3.0, 4.0), (3.0, 5.0), (5.0, 2.0)];
                let a = rng.pick(&pairs);
                let others: Vec<_> = pairs.iter().copied().filter(|p| *p != a).collect();
                let b = rng.pick(&others);
                let mut s = base("knot", Kind::Lines, vec![([1.0, 0.0, 0.0], -0.32)],
                                 Params::Knot { pq: (a, b), phase: rng.uniform(0.0, 1.0),
                                                period: rng.uniform(70.0, 110.0), t: vec![] });
                s.has_occluder = true;
                s.occ_bias = 0.15;
                s
            }
            "geodesic" => {
                let phase = rng.uniform(0.0, 2.0 * PI);
                let rate = rng.uniform(0.06, 0.1);
                let freq = rng.pick(&[1u32, 2, 2]);
                let (ea, eb) = geodesic_edges(freq);
                let mut s = base("geodesic", Kind::Lines, vec![([1.0, 0.0, 0.0], -0.35), ([0.0, 1.0, 0.0], 0.3)],
                                 Params::Geodesic { phase, rate, freq, ea, eb, s: vec![] });
                s.cull = true;
                s
            }
            "lattice" => {
                let k = rng.pick(&[4usize, 5, 5, 6]);
                let phase = rng.uniform(0.0, 2.0 * PI);
                let rate = rng.uniform(0.08, 0.14);
                let g = linspace(-1.0, 1.0, k, true);
                let mut p0 = Vec::new();
                // numpy meshgrid(g, g, g) ravel order: y varies slowest in x,y,z = meshgrid -> irrelevant for points
                for &y in &g {
                    for &x in &g {
                        for &z in &g {
                            p0.push([x * 0.56, y * 0.56, z * 0.56]);
                        }
                    }
                }
                base("lattice", Kind::Glyphs, vec![([0.0, 1.0, 0.0], PI / 4.0), ([1.0, 0.0, 0.0], 0.6155)],
                     Params::Lattice { phase, rate, p0 })
            }
            "mobius" => {
                let phase = rng.uniform(0.0, 2.0 * PI);
                let rate = rng.uniform(0.15, 0.25);
                let h = rng.pick(&[1.0f32, 1.0, 3.0]);
                let mut s = base("mobius", Kind::Surface, vec![([1.0, 0.0, 0.0], -0.95), ([0.0, 0.0, 1.0], 0.4)],
                                 Params::Mobius { phase, rate, h, u: vec![], v: vec![] });
                s.two_sided = true;
                s
            }
            "ring" => {
                let count = rng.pick(&[18usize, 22, 26]);
                let twists = rng.pick(&[1.0f32, 2.0, 3.0]);
                let rate = rng.uniform(0.25, 0.4) * rng.sign();
                base("ring", Kind::Lines, vec![([1.0, 0.0, 0.0], -1.0)],
                     Params::Ring { count, twists, rate, s: vec![], th_rail: vec![] })
            }
            _ => {
                let k = rng.pick(&[5usize, 6, 7]);
                let phase = rng.uniform(0.0, 2.0 * PI);
                let rate = rng.uniform(0.07, 0.11);
                let slide = rng.uniform(0.04, 0.07) * rng.sign();
                let mut s = base("hopf", Kind::Lines, vec![([1.0, 0.0, 0.0], -0.75)],
                                 Params::Hopf { k, phase, rate, slide, t: vec![] });
                s.has_occluder = true;
                s.occ_bias = 0.09;
                s
            }
        }
    }

    fn n(&self, length: f32, k: f32) -> usize {
        ((length * self.ppu * k) as usize).max(8)
    }

    /// ppu = sub-pixels per object unit at the current size; shapes size their sampling from it.
    pub fn build(&mut self, ppu: f32) {
        self.ppu = ppu;
        if let Params::X(x) = &mut self.p {
            x.build(ppu);
            return;
        }
        let tau = 2.0 * PI;
        match self.p {
            Params::Torus { wire, .. } => {
                let (mut u, mut v, mut ou, mut ov) = (vec![], vec![], vec![], vec![]);
                if !wire {
                    let nu = self.n(tau * 0.7, 1.15);
                    let nv = self.n(tau * 0.3, 1.15);
                    for &vv in &linspace(0.0, tau, nv, false) {
                        for &uu in &linspace(0.0, tau, nu, false) {
                            u.push(uu);
                            v.push(vv);
                        }
                    }
                } else {
                    let nu = self.n(tau * 0.7, 1.6);
                    let nv = self.n(tau * 0.3, 1.6);
                    for &uu in &linspace(0.0, tau, 16, false) {
                        for &vv in &linspace(0.0, tau, nv, false) {
                            u.push(uu);
                            v.push(vv);
                        }
                    }
                    for &vv in &linspace(0.0, tau, 6, false) {
                        for &uu in &linspace(0.0, tau, nu, false) {
                            u.push(uu);
                            v.push(vv);
                        }
                    }
                    let nou = self.n(tau, 1.2);
                    let nov = self.n(tau * 0.3, 1.2);
                    for &a in &linspace(0.0, tau, nou, false) {
                        for &b in &linspace(0.0, tau, nov, false) {
                            ou.push(a);
                            ov.push(b);
                        }
                    }
                }
                if let Params::Torus { u: pu, v: pv, ou: pou, ov: pov, .. } = &mut self.p {
                    *pu = u;
                    *pv = v;
                    *pou = ou;
                    *pov = ov;
                }
            }
            Params::Knot { .. } => {
                let n = self.n(16.0, 1.7);
                if let Params::Knot { t, .. } = &mut self.p {
                    *t = linspace(0.0, tau, n, false);
                }
            }
            Params::Geodesic { freq, .. } => {
                let k = ((1.15 / freq as f32 * ppu * 1.7) as usize).max(4);
                if let Params::Geodesic { s, .. } = &mut self.p {
                    *s = linspace(0.0, 1.0, k, true);
                }
            }
            Params::Lattice { .. } | Params::X(_) => {}
            Params::Mobius { .. } => {
                let nu = self.n(tau * 0.72, 1.2);
                let nv = self.n(0.6, 1.4);
                if let Params::Mobius { u, v, .. } = &mut self.p {
                    u.clear();
                    v.clear();
                    for &vv in &linspace(-1.0, 1.0, nv, true) {
                        for &uu in &linspace(0.0, tau, nu, false) {
                            u.push(uu);
                            v.push(vv);
                        }
                    }
                }
            }
            Params::Ring { .. } => {
                let ns = ((0.6 * ppu * 1.8) as usize).max(6);
                let nr = self.n(tau * 0.8, 1.7);
                if let Params::Ring { s, th_rail, .. } = &mut self.p {
                    *s = linspace(-1.0, 1.0, ns, true);
                    *th_rail = linspace(0.0, tau, nr, false);
                }
            }
            Params::Hopf { .. } => {
                let n = self.n(4.4, 1.7);
                if let Params::Hopf { t, .. } = &mut self.p {
                    *t = linspace(0.0, tau, n, false);
                }
            }
        }
    }

    pub fn geom(&self, t: f32, g: &mut Geom) {
        g.clear();
        match &self.p {
            Params::X(x) => x.geom(t, g, self.user4),
            Params::Torus { phase, rate, wire, u, v, ou, ov } => {
                let r = 0.29 + 0.035 * (phase + t * rate).sin();
                let big = 1.0 - r;
                for (uu, vv) in u.iter().zip(v) {
                    let (su, cu) = uu.sin_cos();
                    let (sv, cv) = vv.sin_cos();
                    let n = [cv * cu, cv * su, sv];
                    g.pts.push([big * cu + r * n[0], big * su + r * n[1], r * n[2]]);
                    g.nrm.push(n);
                }
                if *wire {
                    for (a, b) in ou.iter().zip(ov) {
                        let rr = big + r * 0.97 * b.cos();
                        g.occ.push([rr * a.cos(), rr * a.sin(), r * 0.97 * b.sin()]);
                    }
                }
            }
            Params::Knot { pq, phase, period, t: tt } => {
                let x = (phase + t / period).rem_euclid(1.0);
                let tri = 1.0 - (2.0 * x - 1.0).abs();
                let m = sstep((tri - 0.35) / 0.3);
                let ((p1, q1), (p2, q2)) = *pq;
                let knot = |p: f32, q: f32, s: f32| -> V3 {
                    let r = 2.0 + (q * s).cos();
                    [r * (p * s).cos() / 3.1, r * (p * s).sin() / 3.1, (q * s).sin() / 3.1]
                };
                for &s in tt {
                    let a = knot(p1, q1, s);
                    if m < 1e-4 {
                        g.pts.push(a);
                    } else {
                        let b = knot(p2, q2, s);
                        g.pts.push(add(scale(a, 1.0 - m), scale(b, m)));
                    }
                }
                curve_occluder(&g.pts.clone(), tt.len(), 3, 0.11, &mut g.occ);
            }
            Params::Geodesic { phase, rate, ea, eb, s, .. } => {
                let sph = sstep(0.5 + 0.5 * (phase + t * rate).sin());
                for (a, b) in ea.iter().zip(eb) {
                    let d = sub(*b, *a);
                    for &ss in s {
                        let p = add(*a, scale(d, ss));
                        let n = norm(p);
                        let p = add(p, scale(sub(n, p), sph));
                        g.pts.push(scale(p, 0.97));
                        g.nrm.push(n);
                    }
                }
            }
            Params::Lattice { phase, rate, p0 } => {
                let tw = 0.55 * (phase + t * rate).sin();
                for p in p0 {
                    let (s, c) = (p[2] * tw).sin_cos();
                    g.pts.push([c * p[0] - s * p[1], s * p[0] + c * p[1], p[2]]);
                }
            }
            Params::Mobius { phase, rate, h, u, v } => {
                let w = 0.3 + 0.05 * (phase + t * rate).sin();
                let f = |u: f32, v: f32| -> V3 {
                    let r = 0.7 + w * v * (h * u / 2.0).cos();
                    [r * u.cos(), r * u.sin(), w * v * (h * u / 2.0).sin()]
                };
                let e = 1e-3;
                for (&uu, &vv) in u.iter().zip(v) {
                    let p = f(uu, vv);
                    let pu = sub(f(uu + e, vv), f(uu - e, vv));
                    let pv = sub(f(uu, vv + e), f(uu, vv - e));
                    g.pts.push(p);
                    g.nrm.push(norm(cross(pu, pv)));
                }
            }
            Params::Ring { count, twists, rate, s, th_rail } => {
                let up = [0.0, 0.0, 1.0];
                for i in 0..*count {
                    let th = 2.0 * PI * i as f32 / *count as f32;
                    let tw = twists * th / 2.0 + t * rate;
                    let radial = [th.cos(), th.sin(), 0.0];
                    let tang = [-th.sin(), th.cos(), 0.0];
                    let d = add(scale(radial, tw.cos()), scale(up, tw.sin()));
                    let nrm = cross(d, tang);
                    for &ss in s {
                        g.pts.push(add(scale(radial, 0.7), scale(d, 0.29 * ss)));
                        g.nrm.push(nrm);
                    }
                }
                let mut second = Vec::with_capacity(th_rail.len());
                for &th in th_rail {
                    let tw = twists * th / 2.0 + t * rate;
                    let rad = [th.cos(), th.sin(), 0.0];
                    let tan = [-th.sin(), th.cos(), 0.0];
                    let d = add(scale(rad, tw.cos()), scale(up, tw.sin()));
                    let n2 = cross(d, tan);
                    g.pts.push(add(scale(rad, 0.7), scale(d, 0.29)));
                    g.nrm.push(n2);
                    second.push((sub(scale(rad, 0.7), scale(d, 0.29)), n2));
                }
                for (p, n) in second {
                    g.pts.push(p);
                    g.nrm.push(n);
                }
            }
            Params::Hopf { k, phase, rate, slide, t: tt } => {
                let c = -0.4 + 0.25 * (phase + t * rate).sin();
                let r = (1.0 - c * c).max(0.0).sqrt();
                let kk = 1.0 / (2.0 * (1.0 + c)).sqrt();
                let m = tt.len();
                for i in 0..*k {
                    let az = 2.0 * PI * i as f32 / *k as f32 + t * slide;
                    let (a, b) = (r * az.cos(), r * az.sin());
                    for &s in tt {
                        let (st, ct) = s.sin_cos();
                        let x0 = kk * (1.0 + c) * ct;
                        let x1 = kk * (a * st - b * ct);
                        let x2 = kk * (a * ct + b * st);
                        let x3 = kk * (1.0 + c) * st;
                        let d = 1.0 - x1;
                        g.pts.push([x0 / d, x3 / d, x2 / d]);
                    }
                }
                let n = g.pts.len() as f32;
                let mean = g.pts.iter().fold([0.0; 3], |acc, p| add(acc, *p));
                let mean = scale(mean, 1.0 / n);
                let mut mx = 0f32;
                for p in g.pts.iter_mut() {
                    *p = sub(*p, mean);
                    mx = mx.max(len(*p));
                }
                let sc = 0.95 / mx.max(1e-6);
                for p in g.pts.iter_mut() {
                    *p = scale(*p, sc);
                }
                curve_occluder(&g.pts.clone(), m, 2, 0.06, &mut g.occ);
            }
        }
    }
}

/// Tube of points around closed curves of `m` points each, for hiding strands that pass behind others.
fn curve_occluder(pts: &[V3], m: usize, step: usize, tube: f32, out: &mut Vec<V3>) {
    let curves = pts.len() / m.max(1);
    let phis: Vec<(f32, f32)> = (0..10).map(|i| (2.0 * PI * i as f32 / 10.0).sin_cos()).collect();
    for k in 0..curves {
        let c = &pts[k * m..(k + 1) * m];
        let idx: Vec<usize> = (0..m).step_by(step).collect();
        let ns = idx.len();
        for j in 0..ns {
            let p = c[idx[j]];
            let nx = c[idx[(j + 1) % ns]];
            let pv = c[idx[(j + ns - 1) % ns]];
            let tan = norm(sub(nx, pv));
            let a = [tan[0].abs(), tan[1].abs(), tan[2].abs()];
            let r = if a[0] <= a[1] && a[0] <= a[2] {
                [1.0, 0.0, 0.0]
            } else if a[1] <= a[2] {
                [0.0, 1.0, 0.0]
            } else {
                [0.0, 0.0, 1.0]
            };
            let n1 = norm(cross(tan, r));
            let n2 = cross(tan, n1);
            for (s, co) in &phis {
                out.push(add(p, scale(add(scale(n1, *co), scale(n2, *s)), tube)));
            }
        }
    }
}

fn geodesic_edges(f: u32) -> (Vec<V3>, Vec<V3>) {
    let g = (1.0 + 5f32.sqrt()) / 2.0;
    let mut v: Vec<V3> = vec![
        [-1.0, g, 0.0], [1.0, g, 0.0], [-1.0, -g, 0.0], [1.0, -g, 0.0], [0.0, -1.0, g], [0.0, 1.0, g],
        [0.0, -1.0, -g], [0.0, 1.0, -g], [g, 0.0, -1.0], [g, 0.0, 1.0], [-g, 0.0, -1.0], [-g, 0.0, 1.0],
    ];
    let l = len(v[0]);
    for p in v.iter_mut() {
        *p = scale(*p, 1.0 / l);
    }
    let faces = [(0, 11, 5), (0, 5, 1), (0, 1, 7), (0, 7, 10), (0, 10, 11), (1, 5, 9), (5, 11, 4), (11, 10, 2),
                 (10, 7, 6), (7, 1, 8), (3, 9, 4), (3, 4, 2), (3, 2, 6), (3, 6, 8), (3, 8, 9), (4, 9, 5),
                 (2, 4, 11), (6, 2, 10), (8, 6, 7), (9, 8, 1)];
    let key = |p: V3| -> (i64, i64, i64) {
        ((p[0] * 1e5).round() as i64, (p[1] * 1e5).round() as i64, (p[2] * 1e5).round() as i64)
    };
    let mut edges = std::collections::BTreeSet::new();
    let mut pts = std::collections::HashMap::new();
    let ff = f as f32;
    for (a, b, c) in faces {
        let (pa, pb, pc) = (v[a], v[b], v[c]);
        let mut grid = std::collections::HashMap::new();
        for i in 0..=f {
            for j in 0..=(f - i) {
                let p = add(add(pa, scale(sub(pb, pa), i as f32 / ff)), scale(sub(pc, pa), j as f32 / ff));
                grid.insert((i as i32, j as i32), key(p));
                pts.insert(key(p), p);
            }
        }
        for i in 0..=f as i32 {
            for j in 0..=(f as i32 - i) {
                for (di, dj) in [(1, 0), (0, 1), (-1, 1)] {
                    if let Some(k2) = grid.get(&(i + di, j + dj)) {
                        let k1 = grid[&(i, j)];
                        let e = if k1 < *k2 { (k1, *k2) } else { (*k2, k1) };
                        edges.insert(e);
                    }
                }
            }
        }
    }
    let ea = edges.iter().map(|(a, _)| pts[a]).collect();
    let eb = edges.iter().map(|(_, b)| pts[b]).collect();
    (ea, eb)
}
