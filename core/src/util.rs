//! Small helpers: half floats, a log file, BMP writing for frame dumps, colour conversions.

use std::io::Write;
use std::sync::Mutex;

pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let r = sign as u32 | ((e as u32) << 10) | (mant >> 13);
    // round to nearest
    (r + ((mant >> 12) & 1)) as u16
}

pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // subnormal
            let mut e = 127 - 15 + 1;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 0x1f {
        sign | 0x7f80_0000 | (mant << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// The platform's settings + log folder (%APPDATA%\Glyphspin, ~/Library/Application Support/Glyphspin). Set once at
/// startup by the platform; asked for on every use, so the folder is (re)created when needed.
static APP_DIR: std::sync::OnceLock<fn() -> std::path::PathBuf> = std::sync::OnceLock::new();

pub fn set_app_dir(f: fn() -> std::path::PathBuf) {
    let _ = APP_DIR.set(f);
}

pub fn app_dir() -> std::path::PathBuf {
    APP_DIR.get().map(|f| f()).unwrap_or_else(std::env::temp_dir)
}

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn log_init() {
    if let Ok(f) = std::fs::File::create(app_dir().join("log.txt")) {
        *LOG.lock().unwrap() = Some(f);
    }
}

pub fn log(msg: &str) {
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let _ = writeln!(f, "[{}.{:03}] {}", t.as_secs() % 100000, t.subsec_millis(), msg);
        }
    }
}

#[macro_export]
macro_rules! logf {
    ($($t:tt)*) => { $crate::util::log(&format!($($t)*)) };
}

/// Write a 24-bit BMP from RGB8 rows (top-down).
pub fn write_bmp(path: &std::path::Path, w: usize, h: usize, rgb: &[u8]) -> std::io::Result<()> {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(-(h as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            b.extend_from_slice(&[rgb[i + 2], rgb[i + 1], rgb[i]]);
        }
        b.resize(b.len() + (row - w * 3), 0);
    }
    std::fs::write(path, b)
}

pub fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

pub fn hex_rgb(hex: &str) -> [f32; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    [p(0), p(2), p(4)]
}

/// Mix two sRGB colours in sRGB space (like the prototype's colour mixing), t = 0 -> a.
pub fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

pub fn lin(c: [f32; 3]) -> [f32; 4] {
    [srgb_decode(c[0]), srgb_decode(c[1]), srgb_decode(c[2]), 1.0]
}
