//! Glyph atlas layout, shared by the rasteriser, the shaders and both platform atlas builders (GDI on Windows,
//! Core Text on macOS). Every glyph the renderer can emit is drawn once into an R8 coverage texture.
//!
//! Tile index layout (shared with the renderer and the shader):
//!   0            empty
//!   1..=9        RAMP " .·:;+*%#@" minus the space
//!   10..=12      DOTS "·•●" (point lattice, far -> near)
//!   13..=267     braille U+2801..U+28FF  (index = 12 + bits)

pub const RAMP: &str = " .·:;+*%#@";
pub const DOTS: &str = "·•●";
pub const TILE_DOTS: u16 = 10;
pub const TILE_BRAILLE: u16 = 12;
pub const TILE_COUNT: u32 = 268;
pub const ATLAS_COLS: u32 = 16;

pub struct Atlas {
    pub cw: u32,
    pub ch: u32,
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
    pub face: String,
    pub missing: u32,
}

/// The character drawn in tile `i` (1..TILE_COUNT).
pub fn glyph_char(i: u32) -> char {
    if (1..=9).contains(&i) {
        RAMP.chars().nth(i as usize).unwrap()
    } else if (10..=12).contains(&i) {
        DOTS.chars().nth((i - 10) as usize).unwrap()
    } else {
        char::from_u32(0x2800 + (i - 12)).unwrap()
    }
}

/// An atlas drawn entirely by `draw_procedural`, with the cell width exactly `cw_target`. Identical on every
/// platform, which makes `--offline --atlas procedural` renders comparable across operating systems.
pub fn procedural(cw_target: u32) -> Atlas {
    let cw = cw_target.max(4);
    let ch = cw * 2;
    let rows = TILE_COUNT.div_ceil(ATLAS_COLS);
    let (w, h) = (ATLAS_COLS * cw, rows * ch);
    let mut pixels = vec![0u8; (w * h) as usize];
    for i in 1..TILE_COUNT {
        draw_procedural(&mut pixels, w, i, cw, ch);
    }
    Atlas { cw, ch, w, h, pixels, face: "procedural".into(), missing: TILE_COUNT - 1 }
}

/// Fallback for glyphs the font lacks: braille dots and the lattice dots drawn as antialiased discs.
pub fn draw_procedural(px: &mut [u8], w: u32, i: u32, cw: u32, ch: u32) {
    let ox = (i % ATLAS_COLS) * cw;
    let oy = (i / ATLAS_COLS) * ch;
    let mut disc = |cx: f32, cy: f32, r: f32| {
        for y in 0..ch {
            for x in 0..cw {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (r - d + 0.5).clamp(0.0, 1.0);
                let p = &mut px[((oy + y) * w + ox + x) as usize];
                *p = (*p).max((a * 255.0) as u8);
            }
        }
    };
    let (fw, fh) = (cw as f32, ch as f32);
    if i >= 13 {
        let bits = i - 12;
        // braille dot order: (col, row) for bits 0..7
        let pos = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
        for (b, (c, r)) in pos.iter().enumerate() {
            if bits & (1 << b) != 0 {
                disc(fw * (0.3 + 0.4 * *c as f32), fh * (0.15 + 0.233 * *r as f32), fw * 0.14);
            }
        }
    } else if i >= 10 {
        let r = [0.08, 0.17, 0.3][(i - 10) as usize] * fw;
        disc(fw * 0.5, fh * 0.5, r);
    } else {
        disc(fw * 0.5, fh * 0.5, fw * 0.08 * i as f32 / 3.0);
    }
}
