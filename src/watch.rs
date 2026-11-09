//! Is the desktop visible? The renderer fully pauses (no frames, no presents, no mouse hook) when it isn't.
//!
//! Coverage is tested by hit-testing a 16x9 grid of points over the monitor with WindowFromPoint: the desktop is
//! visible when any point lands on the shell's desktop windows. Hit-test-transparent overlays (NVIDIA's, Discord's)
//! are skipped by WindowFromPoint itself, which a window-rectangle walk would get wrong. Fullscreen D3D/presentation
//! mode comes from SHQueryUserNotificationState; lock and display-off arrive as window messages (see main).

use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn desktop_visible(rect: RECT, desk: &[HWND]) -> bool {
    unsafe {
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        // skip a strip at the edges (taskbar, auto-hide reveal)
        for j in 0..9 {
            for i in 0..16 {
                let pt = POINT {
                    x: rect.left + (w * (2 * i + 1)) / 32,
                    y: rect.top + (h * (2 * j + 1)) / 18,
                };
                let hw = WindowFromPoint(pt);
                if hw.is_invalid() {
                    continue;
                }
                let root = GetAncestor(hw, GA_ROOT);
                if desk.iter().any(|d| !d.is_invalid() && (*d == hw || *d == root)) {
                    return true;
                }
            }
        }
        false
    }
}

/// None when the shell says the desktop is fine to draw on; Some(reason) when a fullscreen app, presentation mode
/// or a not-present state (lock, screensaver, user switch) is active.
/// Class of the window under the monitor centre and of its root, for diagnosing the visibility test.
pub fn centre_hit(rect: RECT) -> String {
    unsafe {
        let pt = POINT { x: (rect.left + rect.right) / 2, y: (rect.top + rect.bottom) / 2 };
        let h = WindowFromPoint(pt);
        let root = GetAncestor(h, GA_ROOT);
        let cls = |w: HWND| {
            let mut b = [0u16; 128];
            let n = GetClassNameW(w, &mut b);
            String::from_utf16_lossy(&b[..n.max(0) as usize])
        };
        format!("{} (root {})", cls(h), cls(root))
    }
}

pub fn notification_block() -> Option<&'static str> {
    unsafe {
        match SHQueryUserNotificationState() {
            Ok(QUNS_BUSY) => Some("fullscreen app"),
            Ok(QUNS_RUNNING_D3D_FULL_SCREEN) => Some("D3D fullscreen"),
            Ok(QUNS_PRESENTATION_MODE) => Some("presentation mode"),
            Ok(QUNS_NOT_PRESENT) => Some("not present"),
            _ => None,
        }
    }
}
