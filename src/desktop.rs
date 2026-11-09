//! Finding the wallpaper layer behind the desktop icons, parenting our view window into it, and putting the
//! user's wallpaper back on exit.
//!
//! Two shell layouts exist:
//! - Windows 11 24H2+ (verified here on build 26200): Progman is a WS_EX_NOREDIRECTIONBITMAP top-level window whose
//!   children are SHELLDLL_DefView (WS_EX_LAYERED, holds the icon list view) and, once Progman has been sent 0x052C,
//!   a WorkerW that paints the static wallpaper. Our window becomes a third child of Progman, z-ordered between
//!   DefView and that WorkerW.
//! - Older builds: 0x052C splits the desktop into a top-level WorkerW that holds DefView, and a second top-level
//!   WorkerW right behind it. Our window becomes a child of that second WorkerW. (Coded; tested only on 24H2 so far.)

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Host {
    pub progman: HWND,
    pub defview: HWND,
    pub listview: HWND,
    /// The window our view is parented to (Progman on 24H2, the back WorkerW on older builds).
    pub parent: HWND,
    /// The WorkerW that paints the static wallpaper (24H2: a child of Progman).
    pub workerw: HWND,
    pub modern: bool,
}

unsafe fn find_child(parent: HWND, class: windows::core::PCWSTR) -> HWND {
    unsafe { FindWindowExW(Some(parent), None, class, None).unwrap_or_default() }
}

/// Locate (and if needed create, via 0x052C) the wallpaper layer.
pub fn find_host() -> Option<Host> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), None).ok()?;
        // Ask Explorer to create the wallpaper WorkerW. Harmless if it already exists.
        let mut res = 0usize;
        SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, Some(&mut res));

        let defview = find_child(progman, w!("SHELLDLL_DefView"));
        let worker_child = find_child(progman, w!("WorkerW"));
        if !defview.is_invalid() && !worker_child.is_invalid() {
            let listview = find_child(defview, w!("SysListView32"));
            return Some(Host { progman, defview, listview, parent: progman, workerw: worker_child, modern: true });
        }

        // Legacy: find the top-level WorkerW holding DefView; the next WorkerW in z-order is the wallpaper layer.
        struct Found {
            holder: HWND,
            defview: HWND,
        }
        unsafe extern "system" fn enum_proc(h: HWND, l: LPARAM) -> BOOL {
            unsafe {
                let f = &mut *(l.0 as *mut Found);
                let dv = FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), None).unwrap_or_default();
                if !dv.is_invalid() {
                    f.holder = h;
                    f.defview = dv;
                    return false.into();
                }
                true.into()
            }
        }
        let mut f = Found { holder: HWND::default(), defview: HWND::default() };
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut f as *mut Found as isize));
        if f.holder.is_invalid() {
            return None;
        }
        let behind = FindWindowExW(None, Some(f.holder), w!("WorkerW"), None).unwrap_or_default();
        if behind.is_invalid() {
            return None;
        }
        let listview = find_child(f.defview, w!("SysListView32"));
        Some(Host { progman, defview: f.defview, listview, parent: behind, workerw: behind, modern: false })
    }
}

/// Primary monitor rectangle in physical screen pixels (the process is per-monitor-DPI aware).
pub fn primary_monitor() -> (HMONITOR, RECT, String) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let _ = GetMonitorInfoW(mon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO);
        let name = String::from_utf16_lossy(&mi.szDevice).trim_end_matches('\0').to_string();
        (mon, mi.monitorInfo.rcMonitor, name)
    }
}

/// Parent `view` into the host and cover `rect` (screen coordinates).
pub fn attach(view: HWND, host: &Host, rect: RECT) -> bool {
    unsafe {
        // hidden until the first frame has been presented: an empty child of a NOREDIRECTIONBITMAP parent can show
        // as a black rectangle over the wallpaper
        let style = (WS_CHILD | WS_CLIPSIBLINGS | WS_DISABLED).0 as isize;
        SetWindowLongPtrW(view, GWL_STYLE, style);
        if SetParent(view, Some(host.parent)).is_err() {
            return false;
        }
        place(view, host, rect)
    }
}

/// Position the view over `rect` and (24H2) keep it directly below DefView.
pub fn place(view: HWND, host: &Host, rect: RECT) -> bool {
    unsafe {
        let mut pts = [POINT { x: rect.left, y: rect.top }];
        MapWindowPoints(None, Some(host.parent), &mut pts);
        let after = if host.modern { Some(host.defview) } else { Some(HWND_TOP) };
        SetWindowPos(view, after, pts[0].x, pts[0].y, rect.right - rect.left, rect.bottom - rect.top,
                     SWP_NOACTIVATE)
            .is_ok()
    }
}

pub fn show(view: HWND, on: bool) {
    unsafe {
        let _ = ShowWindow(view, if on { SW_SHOWNA } else { SW_HIDE });
    }
}

/// True when the view is still where it belongs (parent alive, and on 24H2 directly under DefView).
pub fn still_attached(view: HWND, host: &Host) -> bool {
    unsafe {
        if !IsWindow(Some(view)).as_bool() || !IsWindow(Some(host.parent)).as_bool() {
            return false;
        }
        if GetParent(view).unwrap_or_default() != host.parent {
            return false;
        }
        true
    }
}

pub fn z_order_ok(view: HWND, host: &Host) -> bool {
    unsafe { !host.modern || GetWindow(host.defview, GW_HWNDNEXT).unwrap_or_default() == view }
}

/// Re-apply the current wallpaper so Explorer repaints the layer we covered. fUpdateIni = 0: nothing is written.
pub fn refresh_wallpaper() {
    unsafe {
        let mut buf = [0u16; 1024];
        if SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr() as *mut _),
                                 SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok() {
            let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(buf.as_mut_ptr() as *mut _),
                                          SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
    }
}
