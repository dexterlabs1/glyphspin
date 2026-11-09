//! The frame rasteriser (ported from the original Python prototype): project the shape onto a sub-cell grid, z-buffer it,
//! and reduce each cell to one glyph and one of five brightness bands. Works only inside the object's bounding box,
//! so the cost follows the object, not the screen.

use crate::atlas::{TILE_BRAILLE, TILE_DOTS};
use crate::shapes::{Geom, Kind, Shape, V3, dot, norm};

pub const CAM_D: f32 = 4.2;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style {
    Auto,
    Ramp,
    Braille,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Surface,
    Lines,
    Glyphs,
    LinesRamp,
    SurfBraille,
    GlyphsRamp,
    GlyphsBraille,
}

pub struct View {
    pub cols: usize,
    pub rows: usize,
    pub cw: f32,
    pub ch: f32,
    /// object centre in pixels, relative to the grid origin
    pub cx: f32,
    pub cy: f32,
    /// pixels per object unit
    pub s: f32,
}

#[derive(Clone, Copy, Default)]
pub struct ObjCell {
    pub idx: u32,
    pub glyph: u16,
    pub level: f32,
    /// the cell shows the slicing plane's cross-section rim
    pub hot: bool,
}

/// Per-pass options: render the points as another kind (wire / points modes), and a slicing plane in world space
/// (normal, offset, rim width): points beyond it are cut away, points within the rim are flagged.
#[derive(Clone, Copy, Default)]
pub struct Extras {
    pub kind: Option<Kind>,
    pub slice: Option<(V3, f32, f32)>,
}

#[derive(Default)]
pub struct Scratch {
    zbuf: Vec<f32>,
    buf: Vec<f32>,
    dbuf: Vec<f32>,
    cnt: Vec<f32>,
    pidx: Vec<u32>,
    pdep: Vec<f32>,
    pk: Vec<u32>,
    hot: Vec<u8>,
}

const BAYER: [[f32; 4]; 4] = [[0.0, 8.0, 2.0, 10.0], [12.0, 4.0, 14.0, 6.0], [3.0, 11.0, 1.0, 9.0], [15.0, 7.0, 13.0, 5.0]];
const BRAILLE_BITS: [[u16; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

fn hash(a: u32, b: u32) -> f32 {
    let mut h = a.wrapping_mul(0x8da6_b343) ^ b.wrapping_mul(0xd816_3841);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

#[inline]
fn mat_mul(m: &[[f32; 3]; 3], p: V3) -> V3 {
    [m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
     m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
     m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2]]
}

/// Sub-pixels per object unit for `build`, as in the prototype (two sub-columns per cell).
pub fn ppu(view: &View) -> f32 {
    view.s / view.cw * 2.0
}

#[allow(clippy::too_many_arguments)]
pub fn frame(shape: &Shape, g: &Geom, rot: &[[f32; 3]; 3], light: V3, fade: f32, view: &View, style: Style,
             shimmer: f32, t: f32, ex: &Extras, sc: &mut Scratch, out: &mut Vec<ObjCell>) {
    out.clear();
    let kind = ex.kind.unwrap_or(shape.kind);
    let mode = match (kind, style) {
        (Kind::Surface, Style::Braille) => Mode::SurfBraille,
        (Kind::Surface, _) => Mode::Surface,
        (Kind::Lines, Style::Ramp) => Mode::LinesRamp,
        (Kind::Lines, _) => Mode::Lines,
        (Kind::Glyphs, Style::Ramp) => Mode::GlyphsRamp,
        (Kind::Glyphs, Style::Braille) => Mode::GlyphsBraille,
        (Kind::Glyphs, _) => Mode::Glyphs,
    };
    let (rx, ry) = match mode {
        Mode::Lines | Mode::SurfBraille => (2usize, 4usize),
        Mode::Surface | Mode::LinesRamp => (2, 2),
        _ => (1, 1),
    };
    let scale = view.s * (0.9 + 0.1 * fade);
    let reach = scale * CAM_D / (CAM_D - 1.0) + 2.0;
    let c0 = (((view.cx - reach) / view.cw).floor().max(0.0)) as usize;
    let c1 = (((view.cx + reach) / view.cw).ceil() as usize).min(view.cols);
    let r0 = (((view.cy - reach) / view.ch).floor().max(0.0)) as usize;
    let r1 = (((view.cy + reach) / view.ch).ceil() as usize).min(view.rows);
    if c1 <= c0 || r1 <= r0 {
        return;
    }
    let (bw, bh) = (c1 - c0, r1 - r0);
    let (sw, sh) = (bw * rx, bh * ry);
    let n = sw * sh;
    let fx = rx as f32 / view.cw;
    let fy = ry as f32 / view.ch;
    let ox = (c0 * rx) as f32;
    let oy = (r0 * ry) as f32;

    sc.zbuf.clear();
    sc.zbuf.resize(n, f32::INFINITY);

    let project = |p: V3| -> (Option<u32>, f32, V3) {
        let pw = mat_mul(rot, p);
        let persp = CAM_D / (CAM_D - pw[2]);
        let x = view.cx + pw[0] * persp * scale;
        let y = view.cy - pw[1] * persp * scale;
        let ix = (x * fx).floor() - ox;
        let iy = (y * fy).floor() - oy;
        if ix >= 0.0 && iy >= 0.0 && (ix as usize) < sw && (iy as usize) < sh {
            (Some((iy as usize * sw + ix as usize) as u32), -pw[2], pw)
        } else {
            (None, -pw[2], pw)
        }
    };

    // occluder (a solid that only fills the depth buffer)
    if shape.has_occluder {
        for &p in &g.occ {
            if let (Some(i), d, _) = project(p) {
                let v = d + shape.occ_bias;
                let z = &mut sc.zbuf[i as usize];
                if v < *z {
                    *z = v;
                }
            }
        }
    }

    // project the visible points
    sc.pidx.clear();
    sc.pdep.clear();
    sc.pk.clear();
    let has_n = !g.nrm.is_empty();
    let half = norm([light[0], light[1], light[2] + 1.0]);
    for (k, &p) in g.pts.iter().enumerate() {
        let (Some(i), d, pw) = project(p) else { continue };
        if shape.has_occluder && d > sc.zbuf[i as usize] {
            continue;
        }
        let mut rim = 0u32;
        if let Some((n, off, w)) = ex.slice {
            let dist = dot(n, pw) - off;
            if dist > 0.0 {
                continue;
            }
            if dist > -w {
                rim = 1 << 31;
            }
        }
        if has_n && shape.cull {
            let nv = mat_mul(rot, g.nrm[k]);
            let facing = nv[0] * -pw[0] + nv[1] * -pw[1] + nv[2] * (CAM_D - pw[2]);
            if facing <= -0.02 {
                continue;
            }
        }
        sc.pidx.push(i);
        sc.pdep.push(d);
        sc.pk.push(k as u32 | rim);
    }
    // lighting only for the points that win their sub-pixel (most of a dense surface is hidden)
    let lum_of = |k: usize| -> f32 {
        if !has_n {
            return -1.0;
        }
        let mut nv = mat_mul(rot, g.nrm[k]);
        if (shape.two_sided || kind == Kind::Lines) && nv[2] < 0.0 {
            nv = [-nv[0], -nv[1], -nv[2]];
        }
        let lam = dot(nv, light).max(0.0);
        let spec = dot(nv, half).max(0.0).powi(28);
        0.07 + 0.72 * lam + 0.35 * spec
    };
    // the occluder pass already wrote the solid; the line itself is still allowed in front of it
    if shape.has_occluder {
        for v in sc.zbuf.iter_mut() {
            *v = f32::INFINITY;
        }
    }
    for k in 0..sc.pidx.len() {
        let z = &mut sc.zbuf[sc.pidx[k] as usize];
        if sc.pdep[k] < *z {
            *z = sc.pdep[k];
        }
    }

    // per sub-pixel buffers
    sc.buf.clear();
    sc.dbuf.clear();
    sc.cnt.clear();
    let empty = if matches!(mode, Mode::Lines | Mode::Glyphs | Mode::GlyphsRamp | Mode::GlyphsBraille | Mode::LinesRamp) {
        -1.0
    } else {
        0.0
    };
    sc.buf.resize(n, empty);
    sc.dbuf.resize(n, 0.0);
    sc.cnt.resize(n, 0.0);
    let slicing = ex.slice.is_some();
    if slicing {
        sc.hot.clear();
        sc.hot.resize(n, 0);
    }
    for k in 0..sc.pidx.len() {
        let i = sc.pidx[k] as usize;
        if sc.pdep[k] > sc.zbuf[i] {
            continue;
        }
        let dnear = ((1.05 + sc.pdep[k]) / 2.1).clamp(0.0, 1.0);
        let lum = lum_of((sc.pk[k] & 0x7fff_ffff) as usize);
        if slicing {
            sc.hot[i] = (sc.pk[k] >> 31) as u8;
        }
        let v = match mode {
            Mode::Surface | Mode::SurfBraille => lum.max(0.0),
            Mode::Lines | Mode::LinesRamp => {
                let mut b = 0.62 * (1.0 - dnear) + 0.38;
                if lum >= 0.0 {
                    b = 0.45 * b + 0.55 * lum.min(1.0) + 0.1;
                }
                b.clamp(0.0, 1.2) * fade
            }
            _ => (0.3 + 0.75 * (1.0 - dnear)).clamp(0.0, 1.2) * fade,
        };
        sc.buf[i] = v;
        sc.dbuf[i] = dnear;
        sc.cnt[i] = 1.0;
    }

    // reduce to cells
    let tick = (t * 12.0) as u32;
    for cy in 0..bh {
        for cx in 0..bw {
            let gx = c0 + cx;
            let gy = r0 + cy;
            let mut glyph = 0u16;
            let mut band = -1.0f32;
            match mode {
                Mode::Surface | Mode::SurfBraille => {
                    let (mut cs, mut ls, mut ds) = (0.0, 0.0, 0.0);
                    let mut bits = 0u16;
                    for sy in 0..ry {
                        for sx in 0..rx {
                            let i = (cy * ry + sy) * sw + cx * rx + sx;
                            if sc.cnt[i] > 0.0 {
                                cs += 1.0;
                                ls += sc.buf[i];
                                ds += sc.dbuf[i];
                                if mode == Mode::SurfBraille {
                                    let thr = 0.1 + 0.55 * BAYER[(gy * ry + sy) % 4][(gx * rx + sx) % 4] / 16.0;
                                    if sc.buf[i] * fade > thr {
                                        bits |= BRAILLE_BITS[sy][sx];
                                    }
                                }
                            }
                        }
                    }
                    if cs > 0.0 {
                        let cov = cs / (rx * ry) as f32;
                        let shade = ls / cs;
                        let dep = ds / cs;
                        let mut val = shade * (0.45 + 0.55 * cov) * fade;
                        if mode == Mode::Surface {
                            if shimmer > 0.0 {
                                val += shimmer * (hash(gx as u32 * 7 + 1, gy as u32 * 13 + tick) - 0.5) * 0.16;
                            }
                            let ci = ((val * 9.0 + 0.5) as i32).clamp(1, 9);
                            glyph = ci as u16;
                        } else if bits != 0 {
                            glyph = TILE_BRAILLE + bits;
                        }
                        band = (0.62 * val.min(1.0) + 0.38 * (1.0 - dep)) * fade;
                        if glyph == 0 {
                            band = -1.0;
                        }
                    }
                }
                Mode::Lines => {
                    let mut bits = 0u16;
                    let mut mx = -1.0f32;
                    for sy in 0..4 {
                        for sx in 0..2 {
                            let v = sc.buf[(cy * 4 + sy) * sw + cx * 2 + sx];
                            if v >= 0.0 {
                                bits |= BRAILLE_BITS[sy][sx];
                                mx = mx.max(v);
                            }
                        }
                    }
                    if bits != 0 {
                        glyph = TILE_BRAILLE + bits;
                        band = mx;
                    }
                }
                Mode::LinesRamp => {
                    let mut c = 0.0;
                    let mut mx = -1.0f32;
                    for sy in 0..2 {
                        for sx in 0..2 {
                            let v = sc.buf[(cy * 2 + sy) * sw + cx * 2 + sx];
                            if v >= 0.0 {
                                c += 1.0;
                                mx = mx.max(v);
                            }
                        }
                    }
                    if c > 0.0 {
                        let val = mx * (0.45 + 0.55 * c / 4.0);
                        glyph = ((val * 9.0 + 0.5) as i32).clamp(2, 9) as u16;
                        band = mx;
                    }
                }
                Mode::Glyphs | Mode::GlyphsRamp | Mode::GlyphsBraille => {
                    let i = cy * sw + cx;
                    let v = sc.buf[i];
                    if v >= 0.0 {
                        let near = 1.0 - sc.dbuf[i];
                        let gi = ((near * 3.0) as usize).min(2);
                        glyph = match mode {
                            Mode::Glyphs => TILE_DOTS + gi as u16,
                            Mode::GlyphsRamp => (3.0 + near * 6.0).round().clamp(3.0, 9.0) as u16,
                            _ => TILE_BRAILLE + [0x12u16, 0x36, 0xff][gi],
                        };
                        band = v;
                    }
                }
            }
            if band >= 0.0 && glyph != 0 {
                let lv = if band < 0.2 {
                    1
                } else if band < 0.38 {
                    2
                } else if band < 0.56 {
                    3
                } else if band < 0.76 {
                    4
                } else {
                    5
                };
                let mut hot = false;
                if slicing {
                    for sy in 0..ry {
                        for sx in 0..rx {
                            hot |= sc.hot[(cy * ry + sy) * sw + cx * rx + sx] != 0;
                        }
                    }
                }
                out.push(ObjCell { idx: (gy * view.cols + gx) as u32, glyph, level: lv as f32, hot });
            }
        }
    }
}
