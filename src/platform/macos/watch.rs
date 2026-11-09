//! Is the desktop visible, and is the pointer over it? Both come from the window server's list of on-screen
//! windows (CGWindowListCopyWindowInfo: bounds, layer, alpha and owner are readable without Screen Recording
//! permission). The occlusion-state notification of our own windows is the other half of the visibility test
//! (see mod.rs); this sampling also catches what it misses.
//!
//! - Visibility: a 16x9 grid of points over each screen; the desktop is covered when every point lies inside an
//!   ordinary window (layer 0 up to, not including, the Dock's layer 20). A fullscreen app covers them all.
//! - Bare desktop: the front-most window under the pointer is at a desktop level (negative layer: the wallpaper and
//!   Finder's icon layer), or nothing is there.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_graphics::{
    CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds,
    kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSRect, NSString};

pub struct Win {
    pub layer: i64,
    pub pid: i32,
    pub alpha: f64,
    /// global points, top-left origin (the primary screen's top-left is 0,0)
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn key(k: &CFString) -> &NSString {
    // SAFETY: CFString and NSString are toll-free bridged
    unsafe { &*(k as *const CFString as *const NSString) }
}

fn num(d: &NSDictionary<NSString, AnyObject>, k: &NSString) -> Option<f64> {
    let v = d.objectForKey(k)?;
    v.downcast::<NSNumber>().ok().map(|n| n.as_f64())
}

/// On-screen windows, front to back.
pub fn windows(include_desktop: bool) -> Vec<Win> {
    let mut opt = CGWindowListOption::OptionOnScreenOnly;
    if !include_desktop {
        opt |= CGWindowListOption::ExcludeDesktopElements;
    }
    let Some(arr) = CGWindowListCopyWindowInfo(opt, kCGNullWindowID) else { return Vec::new() };
    // SAFETY: a CFArray of CFDictionary is toll-free bridged to NSArray<NSDictionary>
    let arr: Retained<NSArray<NSDictionary<NSString, AnyObject>>> =
        unsafe { Retained::from_raw(CFRetained::into_raw(arr).as_ptr() as *mut _).unwrap() };
    let (k_layer, k_pid, k_alpha, k_bounds) = unsafe {
        (key(kCGWindowLayer), key(kCGWindowOwnerPID), key(kCGWindowAlpha), key(kCGWindowBounds))
    };
    let (kx, ky, kw, kh) = (NSString::from_str("X"), NSString::from_str("Y"), NSString::from_str("Width"),
                            NSString::from_str("Height"));
    let mut out = Vec::new();
    for d in arr.iter() {
        let Some(b) = d.objectForKey(k_bounds).and_then(|b| b.downcast::<NSDictionary>().ok()) else { continue };
        // SAFETY: the bounds dictionary has NSString keys and NSNumber values
        let b: &NSDictionary<NSString, AnyObject> = unsafe { &*(Retained::as_ptr(&b) as *const _) };
        out.push(Win {
            layer: num(&d, k_layer).unwrap_or(0.0) as i64,
            pid: num(&d, k_pid).unwrap_or(0.0) as i32,
            alpha: num(&d, k_alpha).unwrap_or(1.0),
            x: num(b, &kx).unwrap_or(0.0),
            y: num(b, &ky).unwrap_or(0.0),
            w: num(b, &kw).unwrap_or(0.0),
            h: num(b, &kh).unwrap_or(0.0),
        });
    }
    out
}

fn inside(w: &Win, x: f64, y: f64) -> bool {
    x >= w.x && x < w.x + w.w && y >= w.y && y < w.y + w.h
}

/// True when ordinary windows cover the whole of `frame` (global points, bottom-left origin; `top` is the
/// primary screen's height, to flip into the window list's top-left coordinates).
pub fn covered(frame: NSRect, top: f64, own_pid: i32, wins: &[Win]) -> bool {
    let cover: Vec<&Win> = wins.iter().filter(|w| w.pid != own_pid && (0..20).contains(&w.layer) && w.alpha > 0.01)
        .collect();
    if cover.is_empty() {
        return false;
    }
    let (fx, fy, fw, fh) = (frame.origin.x, top - (frame.origin.y + frame.size.height), frame.size.width,
                            frame.size.height);
    for j in 0..9 {
        for i in 0..16 {
            let x = fx + fw * (2 * i + 1) as f64 / 32.0;
            let y = fy + fh * (2 * j + 1) as f64 / 18.0;
            if !cover.iter().any(|w| inside(w, x, y)) {
                return false;
            }
        }
    }
    true
}

/// Is the front-most window under the global point (x, y top-left points) a desktop-level one?
pub fn over_desktop(x: f64, y: f64, own_pid: i32) -> bool {
    for w in windows(true) {
        if w.pid == own_pid || w.alpha <= 0.01 || !inside(&w, x, y) {
            continue;
        }
        return w.layer < 0;
    }
    true
}
