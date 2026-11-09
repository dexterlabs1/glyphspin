//! Headless mode for testing without a GPU or a desktop. It runs the same per-frame pipeline as the live app
//! (scene step + compose, OLED care: dimmer, APL limiter, auto reroll, pixel orbit) and shades cells on the CPU
//! exactly like shaders/glyph.hlsl's SDR path.
//!
//!   glyphspin --offline out.bmp [--shape S] [--seed N] [--frames N] [--style ramp|braille]
//!                   [--size W H] [--ripple-at F] [--reroll-at F] [--effects off|subtle|full] [--oled on|off]
//!   glyphspin --offline heat.bmp --burn SECONDS [--oled on|off] [--idle] [--sample S] [--heat-scale X]
//!
//! --burn simulates SECONDS of display at the idle frame rate (12 fps) as fast as the CPU allows, accumulates
//! per-pixel linear luminance every --sample seconds (default 5), prints max / mean / hottest pixel, and writes a
//! heat map (1920x1080, normalised to --heat-scale or to its own max). --idle simulates nobody touching the PC.
//!
//! Cross-platform checks: `--atlas procedural` draws every tile procedurally with the cell width exactly at the target
//! (no OS font, so the grid and pixels are the same on every platform); `--cells PATH` writes the last frame's cell
//! grid (4 halfs per cell: glyph, level, tint, glow) and prints its FNV-1a hash.

use crate::raster::Style;
use crate::scene::{AudioFrame, Params, Scene};
use crate::atlas::{self, ATLAS_COLS, Atlas, TILE_COUNT};
use crate::{oled, shapes, util};

struct Shader {
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    cols: usize,
    rows: usize,
    atlas_w: u32,
    atlas: Vec<u8>,
    bands: Vec<[f32; 4]>,
    amber: [f32; 4],
    glow_col: [f32; 4],
    glow_gain: f32,
}

struct Frame<'a> {
    glyph: &'a [u32],
    data: &'a [[f32; 3]], // level, tint, glow
    origin: (i32, i32),
    bright: f32,
    fx_bright: f32,
    bg_dim: f32,
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

impl Shader {
    fn band(&self, l: f32) -> [f32; 3] {
        let l = l.clamp(0.0, 5.0);
        let i = (l.floor() as usize).min(4);
        let f = l - i as f32;
        let (a, b) = (self.bands[i], self.bands[i + 1]);
        [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
    }

    /// Linear RGB of pixel (x, y) (SDR path of the shader).
    #[inline]
    fn pixel(&self, f: &Frame, x: u32, y: u32) -> [f32; 3] {
        let (cw, ch) = (self.cw as f32, self.ch as f32);
        let px = x as f32 + 0.5 - f.origin.0 as f32;
        let py = y as f32 + 0.5 - f.origin.1 as f32;
        let (gx, gy) = (px / cw, py / ch);
        let (cx, cy) = (gx.floor() as i32, gy.floor() as i32);
        let bg = self.bands[0];
        let mut col = [bg[0] * f.bg_dim, bg[1] * f.bg_dim, bg[2] * f.bg_dim];
        let inside = |cx: i32, cy: i32| cx >= 0 && cy >= 0 && cx < self.cols as i32 && cy < self.rows as i32;
        if inside(cx, cy) {
            let i = cy as usize * self.cols + cx as usize;
            let g = f.glyph[i];
            if g > 0 {
                let d = f.data[i];
                let lx = (px - cx as f32 * cw) as u32;
                let ly = (py - cy as f32 * ch) as u32;
                let tx = (g % ATLAS_COLS) * self.cw + lx;
                let ty = (g / ATLAS_COLS) * self.ch + ly;
                let a = self.atlas[(ty * self.atlas_w + tx) as usize] as f32 / 255.0;
                if a > 0.0 {
                    let mut gc = self.band(d[0]);
                    if d[1] > 0.0 {
                        let am = [self.amber[0], self.amber[1], self.amber[2]];
                        let k = luma(gc) / luma(am).max(1e-4);
                        let t = d[1].clamp(0.0, 1.0);
                        for c in 0..3 {
                            gc[c] += (am[c] * k - gc[c]) * t;
                        }
                    }
                    let o = (d[0] - 5.0).max(0.0);
                    let wk = (o * 0.5).clamp(0.0, 1.0) * 0.6;
                    let b = f.bright + (f.fx_bright - f.bright) * o.clamp(0.0, 1.0);
                    for c in 0..3 {
                        let v = (gc[c] + (1.0 - gc[c]) * wk) * b;
                        col[c] += (v - col[c]) * a;
                    }
                }
            }
        }
        // glow (bilinear)
        let (fx, fy) = (gx - 0.5, gy - 0.5);
        let (g0x, g0y) = (fx.floor() as i32, fy.floor() as i32);
        let gl_at = |cx: i32, cy: i32| if inside(cx, cy) { f.data[cy as usize * self.cols + cx as usize][2] } else { 0.0 };
        let (tx, ty) = (fx - g0x as f32, fy - g0y as f32);
        let gl = ((gl_at(g0x, g0y) * (1.0 - tx) + gl_at(g0x + 1, g0y) * tx) * (1.0 - ty)
            + (gl_at(g0x, g0y + 1) * (1.0 - tx) + gl_at(g0x + 1, g0y + 1) * tx) * ty) * f.fx_bright;
        for c in 0..3 {
            col[c] += self.glow_col[c] * gl * self.glow_gain;
        }
        col
    }

    /// Run `f` over every pixel row band on all cores.
    fn par_rows<F: Fn(u32, &mut [f32]) + Sync>(&self, out: &mut [f32], stride: usize, f: F) {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
        let rows_per = (self.h as usize).div_ceil(threads);
        std::thread::scope(|s| {
            for (k, chunk) in out.chunks_mut(rows_per * self.w as usize * stride).enumerate() {
                let f = &f;
                s.spawn(move || {
                    for (r, row) in chunk.chunks_mut(self.w as usize * stride).enumerate() {
                        f((k * rows_per + r) as u32, row);
                    }
                });
            }
        });
    }
}

/// `build_atlas` is the platform's font atlas for a target cell width (GDI on Windows, Core Text on macOS).
pub fn run(args: &[String], build_atlas: fn(u32) -> Atlas) -> i32 {
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let out = arg("--offline").unwrap_or_else(|| "offline.bmp".into());
    let seed: u64 = arg("--seed").and_then(|v| v.parse().ok()).unwrap_or(7);
    let shape = arg("--shape").and_then(|n| shapes::SHAPES.iter().copied().find(|x| *x == n));
    let frames: usize = arg("--frames").and_then(|v| v.parse().ok()).unwrap_or(90);
    let (w, h) = match args.iter().position(|a| a == "--size") {
        Some(i) => (args[i + 1].parse().unwrap_or(3840u32), args[i + 2].parse().unwrap_or(2160u32)),
        None => (3840, 2160),
    };
    let style = match arg("--style").as_deref() {
        Some("ramp") => Style::Ramp,
        Some("braille") => Style::Braille,
        _ => Style::Auto,
    };
    let effects = match arg("--effects").as_deref() {
        Some("off") => 0.0,
        Some("subtle") => 0.5,
        _ => 1.0,
    };
    let care = arg("--oled").as_deref() != Some("off");
    let idle = args.iter().any(|a| a == "--idle");
    let burn: Option<f32> = arg("--burn").and_then(|v| v.parse().ok());
    let sample_s: f32 = arg("--sample").and_then(|v| v.parse().ok()).unwrap_or(5.0);
    let heat_scale: Option<f32> = arg("--heat-scale").and_then(|v| v.parse().ok());
    let ripple_at: Option<usize> = arg("--ripple-at").and_then(|v| v.parse().ok());
    let reroll_at: Option<usize> = arg("--reroll-at").and_then(|v| v.parse().ok());
    let at = |k: &str| -> Option<usize> { arg(k).and_then(|v| v.parse().ok()) };
    let (morph_at, sweep_at, explode_at, spin_at, blend_at) =
        (at("--morph-at"), at("--sweep-at"), at("--explode-at"), at("--spin-at"), at("--blend-at"));
    let mode = match arg("--mode").as_deref() {
        Some("wire") => Some(crate::fx::Mode::Wire),
        Some("points") => Some(crate::fx::Mode::Points),
        Some("solid") => Some(crate::fx::Mode::Solid),
        _ => None,
    };
    let audio_sim = args.iter().any(|a| a == "--audio-sim");
    let user4: Option<(f32, f32)> = args.iter().position(|a| a == "--user4")
        .map(|i| (args[i + 1].parse().unwrap_or(0.0), args[i + 2].parse().unwrap_or(0.0)));

    // grid exactly like the live app's (medium glyphs, one spare cell for the orbit)
    let cw_target = ((h as f32 / 84.0) / 2.0).round() as u32;
    let atlas = match arg("--atlas").as_deref() {
        Some("procedural") => atlas::procedural(cw_target),
        _ => build_atlas(cw_target),
    };
    let cells_out = arg("--cells");
    let (cw, ch) = (atlas.cw, atlas.ch);
    let cols = (w.div_ceil(cw) + 2) as usize;
    let rows = (h.div_ceil(ch) + 2) as usize;
    let base = ((w as i32 - (cols as u32 * cw) as i32) / 2, (h as i32 - (rows as u32 * ch) as i32) / 2);
    let coverage = oled::tile_coverage(&atlas.pixels, atlas.w, cw, ch, ATLAS_COLS, TILE_COUNT);
    let mut scene = Scene::new(seed, shape);
    if !care {
        scene.oled_k = 0.0;
    }
    scene.set_grid(cols, rows, cw as f32, ch as f32, h as f32);
    if let Some(m) = mode {
        scene.fx.mode = m;
        scene.fx.from = m;
    }
    if arg("--ambient").as_deref() == Some("density") {
        scene.fx.ambient = crate::fx::Ambient::Density;
    }
    if let Some(u) = user4 {
        scene.shape.user4 = u;
    }
    let p = Params { style, effects, audio: if audio_sim { 1.0 } else { 0.0 }, oled: care,
                     bounce: args.iter().any(|a| a == "--bounce"),
                     weights: Default::default() };
    let mut af = AudioFrame::default();

    let black = args.iter().any(|a| a == "--black");
    let pal = crate::theme::palette(arg("--theme").as_deref().unwrap_or("teal"), black);
    let bands: Vec<[f32; 4]> = pal.bands.to_vec();
    let mut band_luma = [0f32; 6];
    for i in 0..6 {
        band_luma[i] = luma([bands[i][0], bands[i][1], bands[i][2]]);
    }
    let sh = Shader {
        w, h, cw, ch, cols, rows, atlas_w: atlas.w, atlas: atlas.pixels.clone(), bands,
        amber: pal.second, glow_col: pal.glow, glow_gain: 0.35,
    };

    let mut dimmer = oled::Dimmer::new();
    let mut apl = oled::AplLimiter { k: 1.0 };
    let mut auto = oled::AutoReroll::new(seed as u32);
    let orbit_ph = (((seed >> 8) % 6283) as f32 / 1000.0, ((seed >> 20) % 6283) as f32 / 1000.0);
    let dt = 1.0 / 12.0;
    let total_frames = match burn {
        Some(s) => (s / dt) as usize,
        None => frames,
    };
    let sample_every = ((sample_s / dt).round() as usize).max(1);
    let mut heat = if burn.is_some() { vec![0f32; (w * h) as usize] } else { Vec::new() };
    let mut samples = 0usize;
    let mut compose_ms = 0.0f64;
    let mut worst = 0.0f64;
    let mut rerolls = 0;
    let mut glyph = vec![0u32; cols * rows];
    let mut data = vec![[0f32; 3]; cols * rows];
    let t_start = std::time::Instant::now();

    for fno in 0..total_frames {
        let t = fno as f32 * dt;
        if Some(fno) == ripple_at {
            scene.ripple(w as f32 * 0.3, h as f32 * 0.6, 1.8 * effects);
        }
        if Some(fno) == reroll_at {
            scene.reroll(None, effects, Some(crate::fx::Transition::Dissolve));
        }
        if Some(fno) == morph_at {
            scene.reroll(None, effects, Some(crate::fx::Transition::Morph));
        }
        if Some(fno) == sweep_at {
            scene.fx.sweep_t = 0.0;
            scene.fx.sweep_n = crate::shapes::norm([0.4, 1.0, 0.3]);
        }
        if Some(fno) == explode_at {
            scene.fx.explode_target = 1.0;
        }
        if Some(fno) == spin_at {
            scene.kick([0.0, 7.0, 1.0]);
        }
        if Some(fno) == blend_at {
            scene.fx.from = scene.fx.mode;
            scene.fx.mode = if scene.fx.mode == crate::fx::Mode::Wire { crate::fx::Mode::Solid } else { crate::fx::Mode::Wire };
            scene.fx.blend = 0.0;
        }
        if audio_sim {
            let tt = fno as f32 * dt;
            af.active = true;
            af.bass = 0.5 + 0.5 * (tt * 6.0).sin();
            for b in 0..8 {
                af.spec[b] = (0.5 + 0.5 * (tt * 3.0 + b as f32 * 0.9).sin()).powi(2);
            }
        }
        let idle_s = if idle { t } else { 0.0 };
        let target = oled::Dimmer::target(idle_s, care, 10, 30);
        dimmer.step(target, dt);
        if auto.due(scene.time, if care { 25 } else { 0 }) && !scene.is_busy_rerolling() {
            scene.reroll(None, 0.0, Some(crate::fx::Transition::Morph));
            rerolls += 1;
        }
        let t0 = std::time::Instant::now();
        scene.step(dt, &p, &af);
        scene.compose(dt, &p);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if fno >= 5 {
            compose_ms += ms;
            worst = worst.max(ms);
        }
        {
            let (g, l) = scene.layers();
            apl.step(oled::apl(g, l, &coverage, &band_luma), dt);
        }
        let orb = oled::orbit(scene.time, scene.oled_k, cw, ch, orbit_ph);
        let origin = (base.0 + orb.0, base.1 + orb.1);
        let rest = 1.0 + scene.oled_k * (oled::REST_SCALE - 1.0);
        let fx_bright = dimmer.v * apl.k;
        let bright = rest * fx_bright;
        let want = (burn.is_some() && fno % sample_every == 0) || (burn.is_none() && fno + 1 == total_frames);
        if want {
            for i in 0..cols * rows {
                let o = i * 4;
                glyph[i] = util::f16_to_f32(scene.cells[o]) as u32;
                data[i] = [util::f16_to_f32(scene.cells[o + 1]), util::f16_to_f32(scene.cells[o + 2]),
                           util::f16_to_f32(scene.cells[o + 3])];
            }
            let fr = Frame { glyph: &glyph, data: &data, origin, bright, fx_bright, bg_dim: dimmer.v };
            if burn.is_some() {
                sh.par_rows(&mut heat, 1, |y, row| {
                    for x in 0..w {
                        row[x as usize] += luma(sh.pixel(&fr, x, y));
                    }
                });
                samples += 1;
            } else {
                let mut rgb = vec![0f32; (w * h * 3) as usize];
                sh.par_rows(&mut rgb, 3, |y, row| {
                    for x in 0..w {
                        let c = sh.pixel(&fr, x, y);
                        row[x as usize * 3..x as usize * 3 + 3].copy_from_slice(&c);
                    }
                });
                let bytes: Vec<u8> = rgb.iter().map(|v| (util::srgb_encode(*v) * 255.0 + 0.5) as u8).collect();
                if let Err(e) = util::write_bmp(std::path::Path::new(&out), w as usize, h as usize, &bytes) {
                    println!("write failed: {e}");
                    return 1;
                }
            }
        }
    }
    let n = total_frames.saturating_sub(5).max(1) as f64;
    println!("shape {} grid {}x{} cell {}x{} | step+compose avg {:.3} ms, worst {:.3} ms over {} frames | obj cells {} \
              | oled {} | dim {:.2} | apl gain {:.2} | auto rerolls {} | wall {:.1} s",
             scene.shape_name(), cols, rows, cw, ch, compose_ms / n, worst, total_frames, scene.object_cells(), care,
             dimmer.v, apl.k, rerolls, t_start.elapsed().as_secs_f32());
    if let Some(path) = cells_out {
        let bytes: Vec<u8> = scene.cells.iter().flat_map(|v| v.to_le_bytes()).collect();
        let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3));
        let _ = std::fs::write(&path, &bytes);
        println!("cells {} halfs, fnv1a {hash:016x}, font '{}'", scene.cells.len(), atlas.face);
    }

    if burn.is_some() {
        let samples = samples.max(1) as f32;
        let bg_y = band_luma[0];
        let mut max = 0f32;
        let mut at = (0u32, 0u32);
        let mut sum = 0f64;
        for (i, v) in heat.iter_mut().enumerate() {
            *v /= samples; // mean luminance over the run, fraction of SDR white
            sum += *v as f64;
            if *v > max {
                max = *v;
                at = (i as u32 % w, i as u32 / w);
            }
        }
        let mean = (sum / heat.len() as f64) as f32;
        let above = |v: f32| (v - bg_y).max(0.0);
        let mean_above = heat.iter().map(|v| above(*v) as f64).sum::<f64>() as f32 / heat.len() as f32;
        let lit = heat.iter().filter(|v| above(**v) > 0.001).count() as f32 / heat.len() as f32;
        println!("burn {:.0} s, {} samples: mean luminance {:.5}, max {:.5} at {:?} -> max/mean {:.1}; above background: \
                  mean {:.5}, max {:.5} -> max/mean {:.1}; pixels ever lit >0.1% of white: {:.1}%",
                 burn.unwrap(), samples, mean, max, at, max / mean.max(1e-9), mean_above, above(max),
                 above(max) / mean_above.max(1e-9), lit * 100.0);
        // heat map: 1920x1080 box-filtered, black -> teal -> amber -> white
        let scale = heat_scale.unwrap_or(max).max(1e-6);
        let (ow, oh) = (1920u32, 1080u32);
        let (fx, fy) = (w / ow, h / oh);
        let mut rgb = vec![0u8; (ow * oh * 3) as usize];
        for y in 0..oh {
            for x in 0..ow {
                let mut m = 0f32;
                for yy in 0..fy {
                    for xx in 0..fx {
                        m = m.max(heat[((y * fy + yy) * w + x * fx + xx) as usize]);
                    }
                }
                let t = (m / scale).clamp(0.0, 1.0).sqrt();
                let stops = [(0.0, [0.0, 0.0, 0.0]), (0.4, [0.06, 0.35, 0.33]), (0.75, [0.88, 0.65, 0.29]),
                             (1.0, [1.0, 1.0, 1.0])];
                let mut c = [0.0; 3];
                for k in 0..3 {
                    let (t0, c0) = stops[k];
                    let (t1, c1) = stops[k + 1];
                    if t >= t0 && t <= t1 {
                        let f = (t - t0) / (t1 - t0);
                        for j in 0..3 {
                            c[j] = c0[j] + (c1[j] - c0[j]) * f;
                        }
                    }
                }
                let o = ((y * ow + x) * 3) as usize;
                for j in 0..3 {
                    rgb[o + j] = (c[j] * 255.0) as u8;
                }
            }
        }
        if let Err(e) = util::write_bmp(std::path::Path::new(&out), ow as usize, oh as usize, &rgb) {
            println!("write failed: {e}");
            return 1;
        }
        println!("heat map scale {:.5} (pass --heat-scale to compare runs on one scale)", scale);
    }
    0
}
