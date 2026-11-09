//! Glyph atlas on Windows: every glyph the renderer can emit, rasterised once with GDI (grayscale antialiasing, no
//! ClearType: WRGB OLED panels show colour fringes) into an R8 coverage texture. The tile layout is in
//! glyphspin_core::atlas.

use glyphspin_core::atlas::{ATLAS_COLS, Atlas, TILE_COUNT, draw_procedural, glyph_char};
use windows::Win32::Graphics::Gdi::*;
use windows::core::PCWSTR;

/// Build the atlas for a target cell width `cw_target` in physical pixels (cell height is 2x width, so braille dots
/// are square and a row is two columns tall like a terminal).
pub fn build(cw_target: u32) -> Atlas {
    unsafe {
        let em = ((cw_target as f32) / 0.5859).round().max(6.0) as i32;
        let mut font = HFONT::default();
        let mut face = String::new();
        for name in ["Cascadia Mono", "Cascadia Code", "Consolas"] {
            let wname = super::wide(name);
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
