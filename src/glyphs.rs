//! Glyph atlas: every glyph the renderer can emit, rasterised once with GDI (grayscale antialiasing, no ClearType:
//! WRGB OLED panels show colour fringes) into an R8 coverage texture.
//!
//! Tile index layout (shared with the renderer and the shader):
//!   0            empty
//!   1..=9        RAMP " .·:;+*%#@" minus the space
//!   10..=12      DOTS "·•●" (point lattice, far -> near)
//!   13..=267     braille U+2801..U+28FF  (index = 12 + bits)

use windows::Win32::Graphics::Gdi::*;
use windows::core::PCWSTR;

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

fn glyph_char(i: u32) -> char {
    if (1..=9).contains(&i) {
        RAMP.chars().nth(i as usize).unwrap()
    } else if (10..=12).contains(&i) {
        DOTS.chars().nth((i - 10) as usize).unwrap()
    } else {
        char::from_u32(0x2800 + (i - 12)).unwrap()
    }
}

/// Build the atlas for a target cell width `cw_target` in physical pixels (cell height is 2x width, so braille dots
/// are square and a row is two columns tall like a terminal).
pub fn build(cw_target: u32) -> Atlas {
    unsafe {
        let em = ((cw_target as f32) / 0.5859).round().max(6.0) as i32;
        let mut font = HFONT::default();
        let mut face = String::new();
        for name in ["Cascadia Mono", "Cascadia Code", "Consolas"] {
            let wname = crate::util::wide(name);
            let f = CreateFontW(-em, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_TT_PRECIS,
                                CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, (FIXED_PITCH.0 | FF_MODERN.0) as u32,
                                PCWSTR(wname.as_ptr()));
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, f.into());
            let mut buf = [0u16; 64];
            let n = GetTextFaceW(dc, Some(&mut buf));
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
            let got = String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize]);
            if got.eq_ignore_ascii_case(name) {
                font = f;
                face = got;
                break;
            }
            let _ = DeleteObject(f.into());
        }

        let dc = CreateCompatibleDC(None);
        let mut tm = TEXTMETRICW::default();
        if !font.is_invalid() {
            SelectObject(dc, font.into());
            let _ = GetTextMetricsW(dc, &mut tm);
        }
        let cw = if tm.tmAveCharWidth > 0 { tm.tmAveCharWidth as u32 } else { cw_target.max(4) };
        let ch = cw * 2;
        let cols = ATLAS_COLS;
        let rows = TILE_COUNT.div_ceil(cols);
        let (w, h) = (cols * cw, rows * ch);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        let old_bmp = SelectObject(dc, dib.into());
        std::ptr::write_bytes(bits as *mut u8, 0, (w * h * 4) as usize);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00ff_ffff));
        SetTextAlign(dc, TA_TOP | TA_LEFT);
        let line = tm.tmAscent + tm.tmDescent;
        let yoff = (ch as i32 - line) / 2;

        let mut missing = 0;
        let mut proc_tiles = Vec::new();
        for i in 1..TILE_COUNT {
            let c = glyph_char(i);
            let mut u = [0u16; 2];
            let s = c.encode_utf16(&mut u);
            let mut gi = [0u16; 1];
            GetGlyphIndicesW(dc, PCWSTR(s.as_ptr()), 1, gi.as_mut_ptr(), GGI_MARK_NONEXISTING_GLYPHS);
            if font.is_invalid() || gi[0] == 0xffff {
                missing += 1;
                proc_tiles.push(i);
                continue;
            }
            let tx = ((i % cols) * cw) as i32;
            let ty = ((i / cols) * ch) as i32 + yoff;
            let _ = TextOutW(dc, tx, ty, s);
        }
        let _ = GdiFlush();
        let src = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        let mut pixels: Vec<u8> = (0..(w * h) as usize).map(|p| src[p * 4 + 1]).collect();
        SelectObject(dc, old_bmp);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(dc);
        if !font.is_invalid() {
            let _ = DeleteObject(font.into());
        }
        for i in proc_tiles {
            draw_procedural(&mut pixels, w, i, cw, ch);
        }
        Atlas { cw, ch, w, h, pixels, face, missing }
    }
}

/// Fallback for glyphs the font lacks: braille dots and the lattice dots drawn as antialiased discs.
fn draw_procedural(px: &mut [u8], w: u32, i: u32, cw: u32, ch: u32) {
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
