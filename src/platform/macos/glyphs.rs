//! Glyph atlas on macOS: every glyph the renderer can emit, drawn once with Core Text into an 8-bit coverage bitmap
//! (grayscale antialiasing, font smoothing off), the same layout as the Windows GDI atlas (glyphspin_core::atlas).
//! Font order: SF Mono (the system monospaced font), then Menlo; braille comes from Apple Braille when the main
//! font lacks it; anything still missing is drawn procedurally.

use glyphspin_core::atlas::{ATLAS_COLS, Atlas, TILE_COUNT, draw_procedural, glyph_char};
use objc2_app_kit::{NSFont, NSFontWeightRegular};
use objc2_core_foundation::{CFRetained, CFString, CGPoint};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGGlyph};
use objc2_core_text::CTFont;
use std::ptr::NonNull;

fn ct_font(name: &str, size: f64) -> Option<CFRetained<CTFont>> {
    let f = unsafe { CTFont::with_name(&CFString::from_str(name), size, std::ptr::null()) };
    // Core Text substitutes a default font for unknown names: keep it only if the family matches
    let fam = unsafe { f.family_name() }.to_string();
    fam.to_ascii_lowercase().contains(&name.to_ascii_lowercase()).then_some(f)
}

/// The system monospaced font (SF Mono) at `size` points, as a CTFont (NSFont is toll-free bridged).
fn system_mono(size: f64) -> Option<CFRetained<CTFont>> {
    let ns = NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular });
    let ptr = objc2::rc::Retained::into_raw(ns) as *mut CTFont;
    // SAFETY: NSFont and CTFont are toll-free bridged; ownership moves to the CFRetained
    NonNull::new(ptr).map(|p| unsafe { CFRetained::from_raw(p) })
}

fn glyph_of(font: &CTFont, c: char) -> Option<CGGlyph> {
    let mut u = [0u16; 2];
    let s = c.encode_utf16(&mut u);
    let mut g = [0 as CGGlyph; 2];
    let ok = unsafe {
        font.glyphs_for_characters(NonNull::new(s.as_mut_ptr()).unwrap(), NonNull::new(g.as_mut_ptr()).unwrap(),
                                   s.len() as isize)
    };
    (ok && g[0] != 0).then_some(g[0])
}

fn advance(font: &CTFont, g: CGGlyph) -> f64 {
    let mut gg = [g];
    unsafe {
        font.advances_for_glyphs(objc2_core_text::CTFontOrientation::Horizontal, NonNull::new(gg.as_mut_ptr()).unwrap(),
                                 std::ptr::null_mut(), 1)
    }
}

/// Build the atlas for a target cell width `cw_target` in pixels (cell height is 2x width, so braille dots are
/// square and a row is two columns tall like a terminal). The font size is chosen so the advance is exactly
/// `cw_target`.
pub fn build(cw_target: u32) -> Atlas {
    let cw = cw_target.max(4);
    let ch = cw * 2;
    let cols = ATLAS_COLS;
    let rows = TILE_COUNT.div_ceil(cols);
    let (w, h) = (cols * cw, rows * ch);

    // size for an advance of exactly cw pixels
    let probe = system_mono(100.0).or_else(|| ct_font("Menlo", 100.0));
    let (main, face) = match &probe {
        Some(p) => {
            let adv = glyph_of(p, '0').map(|g| advance(p, g)).unwrap_or(60.0).max(1.0);
            let size = cw as f64 * 100.0 / adv;
            match system_mono(size) {
                Some(f) => (Some(f), "SF Mono".to_string()),
                None => (ct_font("Menlo", size), "Menlo".to_string()),
            }
        }
        None => (None, String::new()),
    };
    let size = main.as_ref().map(|f| unsafe { f.size() }).unwrap_or(cw as f64 * 1.7);
    let menlo = if face == "Menlo" { None } else { ct_font("Menlo", size) };
    let braille = ct_font("Apple Braille", size);

    let mut pixels = vec![0u8; (w * h) as usize];
    let space = CGColorSpace::new_device_gray();
    let ctx = unsafe {
        CGBitmapContextCreate(pixels.as_mut_ptr() as *mut _, w as usize, h as usize, 8, w as usize, space.as_deref(),
                              0 /* kCGImageAlphaNone */)
    };
    let mut missing = 0;
    let mut proc_tiles = Vec::new();
    if let Some(ctx) = &ctx {
        CGContext::set_gray_fill_color(Some(ctx), 1.0, 1.0);
        CGContext::set_allows_font_smoothing(Some(ctx), false);
        CGContext::set_should_smooth_fonts(Some(ctx), false);
        CGContext::set_should_antialias(Some(ctx), true);
        for i in 1..TILE_COUNT {
            let c = glyph_char(i);
            let mut fonts: Vec<&CTFont> = Vec::new();
            if i >= 13 {
                // braille: the main font, else Apple Braille
                fonts.extend(main.as_deref());
                fonts.extend(braille.as_deref());
            } else {
                fonts.extend(main.as_deref());
                fonts.extend(menlo.as_deref());
            }
            let hit = fonts.iter().find_map(|f| glyph_of(f, c).map(|g| (*f, g)));
            let Some((font, g)) = hit else {
                missing += 1;
                proc_tiles.push(i);
                continue;
            };
            let (asc, desc) = unsafe { (font.ascent(), font.descent()) };
            let line = asc + desc;
            let yoff = (ch as f64 - line) / 2.0; // top of the line box within the cell
            let adv = advance(font, g);
            let tx = ((i % cols) * cw) as f64 + ((cw as f64 - adv) / 2.0).max(0.0);
            let top = ((i / cols) * ch) as f64 + yoff;
            // CG's origin is the bitmap's bottom-left; the baseline sits `asc` below the line box's top
            let pos = [CGPoint { x: tx, y: h as f64 - (top + asc) }];
            let mut gg = [g];
            unsafe {
                font.draw_glyphs(NonNull::new(gg.as_mut_ptr()).unwrap(), NonNull::new(pos.as_ptr() as *mut _).unwrap(),
                                 1, ctx);
            }
        }
    } else {
        missing = TILE_COUNT - 1;
        proc_tiles = (1..TILE_COUNT).collect();
    }
    drop(ctx);
    for i in proc_tiles {
        draw_procedural(&mut pixels, w, i, cw, ch);
    }
    Atlas { cw, ch, w, h, pixels, face, missing }
}
