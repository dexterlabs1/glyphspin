//! The second set of shapes ("abstract futuristic"): mathematical objects that read well as glyphs.
//! Same contract as shapes.rs: every shape fits inside the unit sphere; `geom(t)` fills points (and normals for
//! surfaces) in object space; `t` is the drift clock. Sampling density follows `ppu` (sub-pixels per unit).

use crate::shapes::{Geom, Kind, Rng, V3, add, cross, len, norm, scale, sstep, sub};
use std::f32::consts::PI;

pub const NAMES: [&str; 10] =
    ["tesseract", "gyroid", "attractor", "supershape", "klein", "armillary", "metaballs", "nebula", "calabi_yau",
     "mobius_stack"];

const TAU: f32 = 2.0 * PI;

fn grid(n: usize, a: f32, b: f32, endpoint: bool) -> impl Iterator<Item = f32> {
    let d = if endpoint { (n.max(2) - 1) as f32 } else { n as f32 };
    (0..n).map(move |i| a + (b - a) * i as f32 / d)
}

/// Positions and unit normals of a parametric surface by central differences, appended to `g`.
fn surface<F: Fn(f32, f32) -> V3>(f: F, us: &[f32], vs: &[f32], g: &mut Geom) {
    let e = 1e-3;
    for &v in vs {
        for &u in us {
            let p = f(u, v);
            let pu = sub(f(u + e, v), f(u - e, v));
            let pv = sub(f(u, v + e), f(u, v - e));
            g.pts.push(p);
            g.nrm.push(norm(cross(pu, pv)));
        }
    }
}

fn fit(g: &mut Geom, target: f32) {
    let mx = g.pts.iter().map(|p| len(*p)).fold(0f32, f32::max).max(1e-6);
    let k = target / mx;
    for p in g.pts.iter_mut() {
        *p = scale(*p, k);
    }
}

pub struct X {
    pub name: &'static str,
    pub kind: Kind,
    pub two_sided: bool,
    pub rest: Vec<(V3, f32)>,
    pub is_4d: bool,
    ppu: f32,
    p: P,
}

enum P {
    Polytope { verts: Vec<[f32; 4]>, edges: Vec<(usize, usize)>, rates: [f32; 3], ph: [f32; 3] },
    Gyroid { k: f32, phase: f32, rate: f32, pts: Vec<V3>, nrm: Vec<V3> },
    Attractor { traj: Vec<V3>, arc: Vec<f32>, speed: f32, window: f32 },
    Super { a: [f32; 8], b: [f32; 8], phase: f32, period: f32, us: Vec<f32>, vs: Vec<f32> },
    Klein { phase: f32, rate: f32, tab: Vec<[f32; 8]> },
    Armillary { rings: Vec<(f32, V3, V3, f32, f32)> },
    Metaballs { balls: Vec<(f32, V3, V3, f32, f32)>, dirs: Vec<V3> },
    Nebula { stars: Vec<V3>, drift: Vec<(V3, f32, f32)>, edges: Vec<(usize, usize)> },
    Calabi { n: usize, phase: f32, rate: f32, us: Vec<f32>, vs: Vec<f32>, k: f32, tab: Vec<[f32; 12]> },
    MobiusStack { strips: Vec<(f32, f32, f32, f32, f32, f32)>, us: Vec<f32>, vs: Vec<f32> },
}

impl X {
    pub fn new(name: &str, rng: &mut Rng) -> X {
        let mk = |name, kind, two_sided, rest: Vec<(V3, f32)>, p| X { name, kind, two_sided, rest, is_4d: false,
                                                                      ppu: 20.0, p };
        match name {
            "tesseract" => {
                // the 8-cell (tesseract) most of the time, the 24-cell now and then
                let (verts, edges) = if rng.f() < 0.7 { tesseract() } else { cell24() };
                let mut x = mk("tesseract", Kind::Lines, false, vec![([1.0, 0.0, 0.0], -0.5), ([0.0, 1.0, 0.0], 0.6)],
                               P::Polytope { verts, edges,
                                             rates: [rng.uniform(0.07, 0.12) * rng.sign(), rng.uniform(0.04, 0.09),
                                                     rng.uniform(0.03, 0.06) * rng.sign()],
                                             ph: [rng.uniform(0.0, TAU), rng.uniform(0.0, TAU), rng.uniform(0.0, TAU)] });
                x.is_4d = true;
                x
            }
            "gyroid" => mk("gyroid", Kind::Surface, true, vec![([1.0, 0.0, 0.0], -0.4)],
                           P::Gyroid { k: rng.uniform(1.3, 1.8), phase: rng.uniform(0.0, TAU),
                                       rate: rng.uniform(0.05, 0.09), pts: vec![], nrm: vec![] }),
            "attractor" => {
                let (traj, arc) = if rng.f() < 0.5 { lorenz(rng) } else { aizawa(rng) };
                let total = *arc.last().unwrap_or(&1.0);
                mk("attractor", Kind::Lines, false, vec![([1.0, 0.0, 0.0], -1.2)],
                   P::Attractor { traj, arc, speed: total / rng.uniform(200.0, 300.0), window: total * 0.3 })
            }
            "supershape" => {
                let pick = |rng: &mut Rng| -> [f32; 8] {
                    let presets = [[6.0, 1.0, 1.0, 1.0, 3.0, 1.0, 1.0, 1.0], [7.0, 0.2, 1.7, 1.7, 7.0, 0.2, 1.7, 1.7],
                                   [5.0, 0.1, 1.7, 1.7, 1.0, 0.3, 0.5, 0.5], [4.0, 1.0, 1.0, 1.0, 4.0, 1.0, 1.0, 1.0],
                                   [8.0, 0.5, 0.5, 8.0, 2.0, 1.0, 1.0, 1.0], [3.0, 0.5, 1.7, 1.7, 6.0, 1.0, 1.0, 1.0]];
                    presets[(rng.next_u64() % presets.len() as u64) as usize]
                };
                let a = pick(rng);
                let mut b = pick(rng);
                while b == a {
                    b = pick(rng);
                }
                mk("supershape", Kind::Surface, false, vec![([1.0, 0.0, 0.0], -0.6)],
                   P::Super { a, b, phase: rng.uniform(0.0, 1.0), period: rng.uniform(80.0, 120.0), us: vec![],
                              vs: vec![] })
            }
            "klein" => mk("klein", Kind::Surface, true, vec![([1.0, 0.0, 0.0], -0.7)],
                          P::Klein { phase: rng.uniform(0.0, TAU), rate: rng.uniform(0.1, 0.18), tab: vec![] }),
            "armillary" => {
                let n = rng.pick(&[4usize, 5, 6]);
                let rings = (0..n)
                    .map(|i| {
                        let r = 0.95 - 0.5 * i as f32 / (n - 1) as f32;
                        let a0 = norm([rng.gauss(0.0, 1.0), rng.gauss(0.0, 1.0), rng.gauss(0.0, 1.0)]);
                        let spin = norm([rng.gauss(0.0, 1.0), rng.gauss(0.0, 1.0), rng.gauss(0.0, 1.0)]);
                        (r, a0, spin, rng.uniform(0.15, 0.45) * rng.sign(), rng.uniform(0.0, TAU))
                    })
                    .collect();
                mk("armillary", Kind::Lines, false, vec![([1.0, 0.0, 0.0], -0.3)], P::Armillary { rings })
            }
            "metaballs" => {
                let n = rng.pick(&[4usize, 5, 6]);
                let balls = (0..n)
                    .map(|_| {
                        let r = rng.uniform(0.2, 0.3);
                        let ax = [rng.uniform(0.25, 0.5), rng.uniform(0.25, 0.5), rng.uniform(0.15, 0.35)];
                        let rates = [rng.uniform(0.08, 0.2), rng.uniform(0.08, 0.2), rng.uniform(0.08, 0.2)];
                        (r, ax, rates, rng.uniform(0.0, TAU), rng.uniform(0.0, TAU))
                    })
                    .collect();
                mk("metaballs", Kind::Surface, false, vec![], P::Metaballs { balls, dirs: vec![] })
            }
            "nebula" => {
                let (stars, drift, edges) = nebula(rng);
                mk("nebula", Kind::Lines, false, vec![], P::Nebula { stars, drift, edges })
            }
            "calabi_yau" => {
                let n = rng.pick(&[3usize, 4, 5, 5]);
                let mut x = mk("calabi_yau", Kind::Surface, true, vec![([1.0, 0.0, 0.0], -0.4)],
                               P::Calabi { n, phase: rng.uniform(0.0, TAU), rate: rng.uniform(0.03, 0.06), us: vec![],
                                           vs: vec![], k: 1.0, tab: vec![] });
                // scale once at a reference angle so the slow drift doesn't pump the size
                let mut g = Geom::new();
                x.build(12.0);
                x.geom(0.0, &mut g, (0.0, 0.0));
                let mx = g.pts.iter().map(|p| len(*p)).fold(0f32, f32::max).max(1e-6);
                if let P::Calabi { k, .. } = &mut x.p {
                    *k = 0.9 / mx;
                }
                x
            }
            _ => {
                let strips = (0..3)
                    .map(|i| {
                        let r = 0.42 + 0.25 * i as f32;
                        let w = 0.1 + 0.02 * i as f32;
                        let twists = rng.pick(&[1.0f32, 2.0, 3.0]);
                        let z = (i as f32 - 1.0) * 0.18;
                        (r, w, twists, rng.uniform(0.05, 0.12) * rng.sign(), rng.uniform(0.0, TAU), z)
                    })
                    .collect();
                mk("mobius_stack", Kind::Surface, true, vec![([1.0, 0.0, 0.0], -0.9)],
                   P::MobiusStack { strips, us: vec![], vs: vec![] })
            }
        }
    }

    fn n(&self, length: f32, k: f32) -> usize {
        ((length * self.ppu * k) as usize).max(8)
    }

    pub fn build(&mut self, ppu: f32) {
        self.ppu = ppu;
        let (u1, v1) = match self.p {
            P::Super { .. } => (self.n(TAU * 0.75, 1.15), self.n(PI * 0.75, 1.15)),
            P::Klein { .. } => (self.n(TAU * 0.75, 1.15), self.n(TAU * 0.3, 1.15)),
            P::Calabi { n, .. } => (self.n(1.1 / n as f32 * 2.0, 1.2), self.n(0.9 / n as f32 * 2.0, 1.2)),
            P::MobiusStack { .. } => (self.n(TAU * 0.95, 1.2), self.n(0.3, 1.4)),
            _ => (0, 0),
        };
        match &mut self.p {
            P::Super { us, vs, .. } => {
                *us = grid(u1, -PI, PI, false).collect();
                *vs = grid(v1, -PI / 2.0 + 0.01, PI / 2.0 - 0.01, true).collect();
            }
            P::Klein { tab, .. } => {
                // the figure-8 Klein bottle splits into a(u,v) = r + q(u,v) and w(u,v); r is the only thing that
                // drifts, so q, w and their partial derivatives are tabulated once: [u, cos u, sin u, q, w, q_u+.., ..]
                tab.clear();
                let q = |u: f32, v: f32| (u / 2.0).cos() * v.sin() - (u / 2.0).sin() * (2.0 * v).sin();
                let w = |u: f32, v: f32| (u / 2.0).sin() * v.sin() + (u / 2.0).cos() * (2.0 * v).sin();
                let e = 1e-3;
                for v in grid(v1, 0.0, TAU, false) {
                    for u in grid(u1, 0.0, TAU, false) {
                        tab.push([u, q(u, v), w(u, v), (q(u + e, v) - q(u - e, v)) / (2.0 * e),
                                  (q(u, v + e) - q(u, v - e)) / (2.0 * e), (w(u + e, v) - w(u - e, v)) / (2.0 * e),
                                  (w(u, v + e) - w(u, v - e)) / (2.0 * e), 0.0]);
                    }
                }
            }
            P::Calabi { us, vs, tab, n, .. } => {
                *us = grid(u1, -1.0, 1.0, true).collect();
                *vs = grid(v1, 0.0, PI / 2.0, true).collect();
                // z1, z2 (complex) and their partials don't depend on time; only the projection angle drifts
                tab.clear();
                let nn = *n as f32;
                let e = 1e-3;
                for k1 in 0..*n {
                    for k2 in 0..*n {
                        let z = |x: f32, y: f32| -> [f32; 4] {
                            let (chr, chi) = (x.cosh() * y.cos(), x.sinh() * y.sin());
                            let (shr, shi) = (x.sinh() * y.cos(), x.cosh() * y.sin());
                            let pw = |re: f32, im: f32, ph0: f32| -> (f32, f32) {
                                let r = (re * re + im * im).sqrt().powf(2.0 / nn);
                                let th = im.atan2(re) * 2.0 / nn + ph0;
                                (r * th.cos(), r * th.sin())
                            };
                            let z1 = pw(chr, chi, TAU * k1 as f32 / nn);
                            let z2 = pw(shi, -shr, TAU * k2 as f32 / nn);
                            [z1.0, z2.0, z1.1, z2.1]
                        };
                        for &y in vs.iter() {
                            for &x in us.iter() {
                                let c = z(x, y);
                                let (a, b) = (z(x + e, y), z(x - e, y));
                                let (cc, d) = (z(x, y + e), z(x, y - e));
                                let mut row = [0f32; 12];
                                for i in 0..4 {
                                    row[i] = c[i];
                                    row[4 + i] = (a[i] - b[i]) / (2.0 * e);
                                    row[8 + i] = (cc[i] - d[i]) / (2.0 * e);
                                }
                                tab.push(row);
                            }
                        }
                    }
                }
            }
            P::MobiusStack { us, vs, .. } => {
                *us = grid(u1, 0.0, TAU, false).collect();
                *vs = grid(v1, -1.0, 1.0, true).collect();
            }
            P::Gyroid { k, pts, nrm, .. } => {
                // sample the gyroid's zero set inside the unit sphere along grid lines on all three axes
                let n = ((ppu * 1.6) as usize).clamp(40, 260);
                let kk = *k * PI;
                let c: Vec<f32> = grid(n, -1.0, 1.0, true).collect();
                let s: Vec<(f32, f32)> = c.iter().map(|v| (v * kk).sin_cos()).collect();
                pts.clear();
                nrm.clear();
                for axis in 0..3 {
                    for a in 0..n {
                        for b in 0..n {
                            let (ya, yb) = (c[a], c[b]);
                            if ya * ya + yb * yb > 1.0 {
                                continue;
                            }
                            let f = |i: usize| -> f32 {
                                // coordinates (x along the line, then the two fixed ones), rotated per axis
                                let (p0, p1, p2) = match axis {
                                    0 => (i, a, b),
                                    1 => (b, i, a),
                                    _ => (a, b, i),
                                };
                                let (sx, cx) = s[p0];
                                let (sy, cy) = s[p1];
                                let (sz, cz) = s[p2];
                                sx * cy + sy * cz + sz * cx
                            };
                            let mut prev = f(0);
                            for i in 1..n {
                                let cur = f(i);
                                if (prev < 0.0) != (cur < 0.0) {
                                    let t = prev / (prev - cur);
                                    let x = c[i - 1] + (c[i] - c[i - 1]) * t;
                                    let p = match axis {
                                        0 => [x, ya, yb],
                                        1 => [yb, x, ya],
                                        _ => [ya, yb, x],
                                    };
                                    if len(p) <= 1.0 {
                                        let (px, py, pz) = (p[0] * kk, p[1] * kk, p[2] * kk);
                                        let g = [px.cos() * py.cos() - pz.sin() * px.sin(),
                                                 -px.sin() * py.sin() + py.cos() * pz.cos(),
                                                 -py.sin() * pz.sin() + pz.cos() * px.cos()];
                                        pts.push(p);
                                        nrm.push(norm(g));
                                    }
                                }
                                prev = cur;
                            }
                        }
                    }
                }
            }
            P::Metaballs { dirs, .. } => {
                // directions on a sphere (Fibonacci), dense enough for the surface at this size
                let n = ((4.0 * PI * 0.3 * 0.3) * ppu * ppu * 0.7) as usize;
                let n = n.clamp(300, 20000);
                let ga = PI * (3.0 - 5f32.sqrt());
                *dirs = (0..n)
                    .map(|i| {
                        let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
                        let r = (1.0 - y * y).sqrt();
                        let th = ga * i as f32;
                        [r * th.cos(), y, r * th.sin()]
                    })
                    .collect();
            }
            _ => {}
        }
    }

    pub fn geom(&self, t: f32, g: &mut Geom, user4: (f32, f32)) {
        g.pts.clear();
        g.nrm.clear();
        g.occ.clear();
        let ppu = self.ppu;
        match &self.p {
            P::Polytope { verts, edges, rates, ph } => {
                // rotate in the xw, yw and zw planes, then project 4D -> 3D in perspective
                let a = [rates[0] * t + ph[0] + user4.0, rates[1] * t + ph[1] + user4.1, rates[2] * t + ph[2]];
                let (s0, c0) = a[0].sin_cos();
                let (s1, c1) = a[1].sin_cos();
                let (s2, c2) = a[2].sin_cos();
                let pr: Vec<V3> = verts
                    .iter()
                    .map(|v| {
                        let (mut x, mut y, mut z, mut w) = (v[0], v[1], v[2], v[3]);
                        let (nx, nw) = (c0 * x - s0 * w, s0 * x + c0 * w);
                        x = nx;
                        w = nw;
                        let (ny, nw) = (c1 * y - s1 * w, s1 * y + c1 * w);
                        y = ny;
                        w = nw;
                        let (nz, nw) = (c2 * z - s2 * w, s2 * z + c2 * w);
                        z = nz;
                        w = nw;
                        let k = 1.5 / (2.2 - w);
                        [x * k, y * k, z * k]
                    })
                    .collect();
                for &(i, j) in edges {
                    let (a, b) = (pr[i], pr[j]);
                    let d = sub(b, a);
                    let n = ((len(d) * ppu * 1.7) as usize).max(4);
                    for k in 0..=n {
                        g.pts.push(add(a, scale(d, k as f32 / n as f32)));
                    }
                }
            }
            P::Gyroid { phase, rate, pts, nrm, .. } => {
                // the clipping sphere breathes, so the sliced shell's rim keeps moving
                let r = 0.72 + 0.26 * sstep(0.5 + 0.5 * (phase + t * rate).sin());
                for (p, n) in pts.iter().zip(nrm) {
                    if len(*p) <= r {
                        g.pts.push(*p);
                        g.nrm.push(*n);
                    }
                }
            }
            P::Attractor { traj, arc, speed, window } => {
                // a ribbon that draws itself: a window of the trajectory slides along it, head first
                let total = *arc.last().unwrap();
                let head = (t * speed).rem_euclid(total);
                let ds = 1.0 / (ppu * 1.7);
                let mut s = head - window;
                let mut idx = 0usize;
                let n = traj.len();
                let wrap = |s: f32| s.rem_euclid(total);
                let mut cur = wrap(s);
                // find the starting index
                while idx + 1 < n && arc[idx + 1] < cur {
                    idx += 1;
                }
                while s < head {
                    cur = wrap(s);
                    if cur < arc[idx] {
                        idx = 0;
                    }
                    while idx + 1 < n && arc[idx + 1] < cur {
                        idx += 1;
                    }
                    let j = (idx + 1).min(n - 1);
                    let seg = (arc[j] - arc[idx]).max(1e-9);
                    let f = ((cur - arc[idx]) / seg).clamp(0.0, 1.0);
                    g.pts.push(add(traj[idx], scale(sub(traj[j], traj[idx]), f)));
                    s += ds;
                }
            }
            P::Super { a, b, phase, period, us, vs } => {
                let x = (phase + t / period).rem_euclid(1.0);
                let tri = 1.0 - (2.0 * x - 1.0).abs();
                let m = sstep((tri - 0.3) / 0.4);
                let p: Vec<f32> = (0..8).map(|i| a[i] + (b[i] - a[i]) * m).collect();
                let sf = |th: f32, m: f32, n1: f32, n2: f32, n3: f32| -> f32 {
                    let c = (m * th / 4.0).cos().abs().powf(n2);
                    let s = (m * th / 4.0).sin().abs().powf(n3);
                    (c + s).max(1e-6).powf(-1.0 / n1)
                };
                // separable: r1 depends on u only, r2 on v only, so the powf work is O(nu + nv)
                let e = 1e-3;
                let r1: Vec<(f32, f32, f32, f32)> = us
                    .iter()
                    .map(|&u| {
                        let r = sf(u, p[0], p[1], p[2], p[3]);
                        let d = (sf(u + e, p[0], p[1], p[2], p[3]) - sf(u - e, p[0], p[1], p[2], p[3])) / (2.0 * e);
                        (r, d, u.cos(), u.sin())
                    })
                    .collect();
                let r2: Vec<(f32, f32, f32, f32)> = vs
                    .iter()
                    .map(|&v| {
                        let r = sf(v, p[4], p[5], p[6], p[7]);
                        let d = (sf(v + e, p[4], p[5], p[6], p[7]) - sf(v - e, p[4], p[5], p[6], p[7])) / (2.0 * e);
                        (r, d, v.cos(), v.sin())
                    })
                    .collect();
                for &(b, db, cv, sv) in &r2 {
                    for &(a, da, cu, su) in &r1 {
                        g.pts.push([a * cu * b * cv, a * su * b * cv, b * sv]);
                        let pu = [b * cv * (da * cu - a * su), b * cv * (da * su + a * cu), 0.0];
                        let k = db * cv - b * sv;
                        let pv = [a * cu * k, a * su * k, db * sv + b * cv];
                        g.nrm.push(norm(cross(pu, pv)));
                    }
                }
                fit(g, 0.95);
            }
            P::Klein { phase, rate, tab } => {
                // the figure-8 immersion of the Klein bottle: p = ((r+q) cos u, (r+q) sin u, w) / (r + 1.6)
                let r = 2.0 + 0.25 * (phase + t * rate).sin();
                let k = 1.0 / (r + 1.6);
                for row in tab {
                    let [u, q, w, qu, qv, wu, wv, _] = *row;
                    let (su, cu) = u.sin_cos();
                    let a = r + q;
                    g.pts.push([a * cu * k, a * su * k, w * k]);
                    let pu = [qu * cu - a * su, qu * su + a * cu, wu];
                    let pv = [qv * cu, qv * su, wv];
                    g.nrm.push(norm(cross(pu, pv)));
                }
            }
            P::Armillary { rings } => {
                for &(r, a0, spin, rate, ph) in rings {
                    // ring plane normal = a0 rotated about `spin` by rate*t
                    let ang = ph + rate * t;
                    let (s, c) = ang.sin_cos();
                    let k = spin;
                    let nrm = add(add(scale(a0, c), scale(cross(k, a0), s)),
                                  scale(k, crate::shapes::dot(k, a0) * (1.0 - c)));
                    let nrm = norm(nrm);
                    let e1 = norm(cross(nrm, if nrm[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] }));
                    let e2 = cross(nrm, e1);
                    let n = ((TAU * r * ppu * 1.7) as usize).max(16);
                    for i in 0..n {
                        let (sa, ca) = (TAU * i as f32 / n as f32).sin_cos();
                        g.pts.push(scale(add(scale(e1, ca), scale(e2, sa)), r));
                    }
                }
                // the axle
                let n = ((1.9 * ppu * 1.7) as usize).max(8);
                for i in 0..=n {
                    g.pts.push([0.0, 0.0, -0.95 + 1.9 * i as f32 / n as f32]);
                }
            }
            P::Metaballs { balls, dirs } => {
                let cs: Vec<(V3, f32)> = balls
                    .iter()
                    .map(|&(r, ax, rt, p1, p2)| {
                        ([ax[0] * (rt[0] * t + p1).sin(), ax[1] * (rt[1] * t + p2).sin(),
                          ax[2] * (rt[2] * t + p1 + p2).cos()], r)
                    })
                    .collect();
                let field = |p: V3| -> (f32, V3) {
                    let mut f = 0.0;
                    let mut gr = [0.0; 3];
                    for (c, r) in &cs {
                        let d = sub(p, *c);
                        let d2 = crate::shapes::dot(d, d).max(1e-6);
                        let v = r * r / d2;
                        f += v;
                        gr = add(gr, scale(d, -2.0 * v / d2));
                    }
                    (f, gr)
                };
                for (c, r) in &cs {
                    for d in dirs {
                        // Newton along the ray from this ball's centre to the f = 1 isosurface
                        let mut tt = *r;
                        for _ in 0..4 {
                            let p = add(*c, scale(*d, tt));
                            let (f, gr) = field(p);
                            let df = crate::shapes::dot(gr, *d);
                            if df.abs() < 1e-6 {
                                break;
                            }
                            tt = (tt - (f - 1.0) / df).clamp(0.02, 1.5);
                        }
                        let p = add(*c, scale(*d, tt));
                        let (f, gr) = field(p);
                        if (f - 1.0).abs() < 0.08 && len(p) < 1.0 {
                            g.pts.push(p);
                            g.nrm.push(norm(scale(gr, -1.0)));
                        }
                    }
                }
            }
            P::Nebula { stars, drift, edges } => {
                let pos: Vec<V3> = stars
                    .iter()
                    .zip(drift)
                    .map(|(s, (a, rt, ph))| add(*s, scale(*a, (rt * t + ph).sin())))
                    .collect();
                // stars: tiny 3D crosses, so each reads as a cluster of dots
                let cr = 1.2 / ppu;
                for p in &pos {
                    for d in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                        for k in -2..=2 {
                            g.pts.push(add(*p, scale(d, cr * k as f32)));
                        }
                    }
                }
                // constellation lines between near neighbours, fading in and out with distance
                for &(i, j) in edges {
                    let (a, b) = (pos[i], pos[j]);
                    let d = sub(b, a);
                    let l = len(d);
                    if l > 0.42 {
                        continue;
                    }
                    let n = ((l * ppu * 1.2) as usize).max(3);
                    for k in 1..n {
                        if k % 2 == 0 {
                            continue; // dotted, so lines stay lighter than the stars
                        }
                        g.pts.push(add(a, scale(d, k as f32 / n as f32)));
                    }
                }
            }
            P::Calabi { phase, rate, k, tab, .. } => {
                // Hanson's slice of the quintic: z1 = e^(2 pi i k1/n) cosh(x+iy)^(2/n),
                // z2 = e^(2 pi i k2/n) (-i sinh(x+iy))^(2/n), shown as (Re z1, Re z2, cos a Im z1 + sin a Im z2)
                let al = PI / 4.0 + 0.5 * (phase + t * rate).sin();
                let (sa, ca) = al.sin_cos();
                let kk = *k;
                let proj = |a: f32, b: f32, c: f32, d: f32| -> V3 { [a, b, ca * c + sa * d] };
                for r in tab {
                    let p = proj(r[0], r[1], r[2], r[3]);
                    g.pts.push(scale(p, kk));
                    g.nrm.push(norm(cross(proj(r[4], r[5], r[6], r[7]), proj(r[8], r[9], r[10], r[11]))));
                }
            }
            P::MobiusStack { strips, us, vs } => {
                for &(r, w, h, rate, ph, z) in strips {
                    let rot = ph + rate * t;
                    let f = |u: f32, v: f32| -> V3 {
                        let rr = r + w * v * (h * u / 2.0).cos();
                        let a = u + rot;
                        [rr * a.cos(), rr * a.sin(), z + w * v * (h * u / 2.0).sin()]
                    };
                    // fewer samples on the inner strips
                    let step = if r < 0.5 { 2 } else { 1 };
                    let uu: Vec<f32> = us.iter().copied().step_by(step).collect();
                    surface(f, &uu, vs, g);
                }
            }
        }
    }
}

fn tesseract() -> (Vec<[f32; 4]>, Vec<(usize, usize)>) {
    let mut v = Vec::new();
    for i in 0..16u32 {
        let c = |b: u32| if i & (1 << b) != 0 { 0.5 } else { -0.5 };
        v.push([c(0), c(1), c(2), c(3)]);
    }
    let mut e = Vec::new();
    for i in 0..16usize {
        for b in 0..4 {
            let j = i ^ (1 << b);
            if i < j {
                e.push((i, j));
            }
        }
    }
    (v, e)
}

fn cell24() -> (Vec<[f32; 4]>, Vec<(usize, usize)>) {
    let mut v: Vec<[f32; 4]> = Vec::new();
    let k = 1.0 / 2f32.sqrt();
    for a in 0..4 {
        for b in a + 1..4 {
            for sa in [-1.0, 1.0] {
                for sb in [-1.0, 1.0] {
                    let mut p = [0.0; 4];
                    p[a] = sa * k;
                    p[b] = sb * k;
                    v.push(p);
                }
            }
        }
    }
    let mut e = Vec::new();
    for i in 0..v.len() {
        for j in i + 1..v.len() {
            let d: f32 = (0..4).map(|c| (v[i][c] - v[j][c]).powi(2)).sum();
            if (d - 1.0).abs() < 1e-3 {
                e.push((i, j));
            }
        }
    }
    (v, e)
}

/// Centre a trajectory, scale it into the unit sphere and compute cumulative arc length.
fn finish_traj(mut pts: Vec<V3>) -> (Vec<V3>, Vec<f32>) {
    let n = pts.len() as f32;
    let mean = scale(pts.iter().fold([0.0; 3], |a, p| add(a, *p)), 1.0 / n);
    let mut mx = 0f32;
    for p in pts.iter_mut() {
        *p = sub(*p, mean);
        mx = mx.max(len(*p));
    }
    for p in pts.iter_mut() {
        *p = scale(*p, 0.95 / mx);
    }
    let mut arc = Vec::with_capacity(pts.len());
    let mut s = 0.0;
    arc.push(0.0);
    for i in 1..pts.len() {
        s += len(sub(pts[i], pts[i - 1]));
        arc.push(s);
    }
    (pts, arc)
}

fn lorenz(rng: &mut Rng) -> (Vec<V3>, Vec<f32>) {
    let (sg, rho, beta) = (10.0f32, 28.0f32, 8.0f32 / 3.0);
    let mut p = [rng.uniform(-1.0, 1.0), rng.uniform(-1.0, 1.0), rng.uniform(20.0, 25.0)];
    let dt = 0.003;
    let mut out = Vec::with_capacity(40000);
    for i in 0..41000 {
        let d = [sg * (p[1] - p[0]), p[0] * (rho - p[2]) - p[1], p[0] * p[1] - beta * p[2]];
        p = add(p, scale(d, dt));
        if i >= 1000 {
            out.push([p[0], p[2] - 25.0, p[1]]);
        }
    }
    finish_traj(out)
}

fn aizawa(rng: &mut Rng) -> (Vec<V3>, Vec<f32>) {
    let (a, b, c, d, e, f) = (0.95f32, 0.7f32, 0.6f32, 3.5f32, 0.25f32, 0.1f32);
    let mut p = [0.1 + rng.uniform(-0.05, 0.05), 0.0, 0.0];
    let dt = 0.008;
    let mut out = Vec::with_capacity(40000);
    for i in 0..42000 {
        let (x, y, z) = (p[0], p[1], p[2]);
        let dp = [(z - b) * x - d * y, d * x + (z - b) * y,
                  c + a * z - z * z * z / 3.0 - (x * x + y * y) * (1.0 + e * z) + f * z * x * x * x];
        p = add(p, scale(dp, dt));
        if i >= 2000 {
            out.push([p[0], p[2] - 0.5, p[1]]);
        }
    }
    finish_traj(out)
}

fn nebula(rng: &mut Rng) -> (Vec<V3>, Vec<(V3, f32, f32)>, Vec<(usize, usize)>) {
    let clumps: Vec<(V3, f32)> = (0..3)
        .map(|_| ([rng.gauss(0.0, 0.3), rng.gauss(0.0, 0.25), rng.gauss(0.0, 0.3)], rng.uniform(0.15, 0.3)))
        .collect();
    let mut stars = Vec::new();
    while stars.len() < 150 {
        let (c, s) = clumps[stars.len() % 3];
        let p = add(c, [rng.gauss(0.0, s), rng.gauss(0.0, s), rng.gauss(0.0, s)]);
        if len(p) < 0.92 {
            stars.push(p);
        }
    }
    let drift = stars
        .iter()
        .map(|_| ([rng.gauss(0.0, 0.03), rng.gauss(0.0, 0.03), rng.gauss(0.0, 0.03)], rng.uniform(0.05, 0.15),
                  rng.uniform(0.0, TAU)))
        .collect();
    let mut edges = Vec::new();
    for i in 0..stars.len() {
        let mut d: Vec<(f32, usize)> = (0..stars.len())
            .filter(|j| *j != i)
            .map(|j| (len(sub(stars[i], stars[j])), j))
            .collect();
        d.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, j) in d.iter().take(2) {
            let e = if i < j { (i, j) } else { (j, i) };
            if !edges.contains(&e) {
                edges.push(e);
            }
        }
    }
    (stars, drift, edges)
}
