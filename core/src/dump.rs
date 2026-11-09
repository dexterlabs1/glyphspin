//! Frame readbacks (`--dump`): decode the swapchain / drawable texels to a BMP and measure the frame.
//! Both platforms read back the same two formats: 8-bit BGRA (SDR) and RGBA FP16 (Windows scRGB, macOS EDR).

use crate::util;

pub struct DumpStats {
    /// brightest pixel's luminance in `unit`s (nits on Windows, multiples of SDR white on macOS)
    pub max: f32,
    pub max_px: (usize, usize),
    pub avg: f64,
    /// pixels brighter than SDR white
    pub over_white: usize,
}

/// Write `px` (`w`x`h`, 8 bytes per pixel when `hdr`, else BGRA8) as a BMP at `path`. HDR values are linear with
/// SDR white at `white`; `unit` converts them to the luminance unit reported in the stats.
pub fn write(path: &std::path::Path, px: &[u8], w: usize, h: usize, hdr: bool, white: f32, unit: f32) -> DumpStats {
    let mut rgb = vec![0u8; w * h * 3];
    let mut max_nits = 0f32;
    let mut max_px = (0, 0);
    let mut sum = 0f64;
    let mut over_white = 0usize;
    for i in 0..w * h {
        let (r, g, b) = if hdr {
            let o = i * 8;
            let r = util::f16_to_f32(u16::from_le_bytes([px[o], px[o + 1]]));
            let g = util::f16_to_f32(u16::from_le_bytes([px[o + 2], px[o + 3]]));
            let b = util::f16_to_f32(u16::from_le_bytes([px[o + 4], px[o + 5]]));
            let y = (0.2126 * r + 0.7152 * g + 0.0722 * b) * unit;
            if y > max_nits {
                max_nits = y;
                max_px = (i % w, i / w);
            }
            if y > white * unit * 1.01 {
                over_white += 1;
            }
            sum += y as f64;
            (util::srgb_encode(r / white), util::srgb_encode(g / white), util::srgb_encode(b / white))
        } else {
            let o = i * 4;
            (px[o + 2] as f32 / 255.0, px[o + 1] as f32 / 255.0, px[o] as f32 / 255.0)
        };
        rgb[i * 3] = (r * 255.0 + 0.5) as u8;
        rgb[i * 3 + 1] = (g * 255.0 + 0.5) as u8;
        rgb[i * 3 + 2] = (b * 255.0 + 0.5) as u8;
    }
    let _ = util::write_bmp(path, w, h, &rgb);
    DumpStats { max: max_nits, max_px, avg: sum / (w * h) as f64, over_white }
}
