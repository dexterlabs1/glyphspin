//! Tray icon and its menu (the menu itself is glyphspin_core::menu, shared with the macOS menu bar). The icon is
//! the embedded resource 1 (assets/glyphspin.ico, made by tools/brand.py); if it's missing, a small teal ring is
//! drawn at startup instead.

use super::wide;
use glyphspin_core::menu::{Check, Item};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub const WM_TRAY: u32 = WM_APP + 30;
const TRAY_ID: u32 = 1;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

fn make_icon() -> HICON {
    unsafe {
        let n = 32usize;
        let mut px = vec![0u32; n * n];
        let (cx, cy) = (15.5f32, 15.5f32);
        for y in 0..n {
            for x in 0..n {
                // torus seen at an angle: an ellipse ring
                let dx = (x as f32 - cx) / 14.0;
                let dy = (y as f32 - cy) / 9.0;
                let r = (dx * dx + dy * dy).sqrt();
                let ring = (1.0 - ((r - 0.72) / 0.2).abs()).clamp(0.0, 1.0);
                let a = (ring * 1.6).min(1.0);
                if a > 0.0 {
                    let (cr, cg, cb) = (0x4f as f32, 0xc7 as f32, 0xbd as f32);
                    let k = 0.75 + 0.25 * (1.0 - dy.abs());
                    px[y * n + x] = ((a * 255.0) as u32) << 24
                        | (((cr * k) as u32) << 16)
                        | (((cg * k) as u32) << 8)
                        | ((cb * k) as u32);
                }
            }
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n as i32,
                biHeight: -(n as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, n * n);
        let mask = CreateBitmap(n as i32, n as i32, 1, 1, None);
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

fn copy_into(dst: &mut [u16], s: &str) {
    let w = wide(s);
    let n = w.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&w[..n]);
    dst[n] = 0;
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        let icon = unsafe {
            let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
            let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            LoadImageW(Some(hinst.into()), PCWSTR(1 as *const u16), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)
                .map(|h| HICON(h.0))
                .unwrap_or_else(|_| make_icon())
        };
        let t = Tray { hwnd, icon };
        t.add();
        t
    }

    pub fn add(&self) {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: WM_TRAY,
                hIcon: self.icon,
                ..Default::default()
            };
            copy_into(&mut nid.szTip, "Glyphspin");
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
    }

    pub fn set_tip(&self, tip: &str) {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                uFlags: NIF_TIP | NIF_SHOWTIP,
                ..Default::default()
            };
            copy_into(&mut nid.szTip, tip);
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn remove(&self) {
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    /// Show the menu at the cursor and return the chosen command (0 if dismissed).
    pub fn menu(&self, items: &[Item]) -> u32 {
        unsafe {
            let m = CreatePopupMenu().unwrap();
            fill(m, items);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // the owner must be foreground or the menu won't dismiss when clicking elsewhere
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0),
                                     self.hwnd, None);
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(m);
            cmd.0 as u32
        }
    }
}

unsafe fn fill(menu: HMENU, items: &[Item]) {
    unsafe {
        for it in items {
            match it {
                Item::Cmd { id, label, check, default } => {
                    let w = wide(label);
                    match check {
                        Check::Box(on) => {
                            let flags = MF_STRING | if *on { MF_CHECKED } else { MF_UNCHECKED };
                            let _ = AppendMenuW(menu, flags, *id as usize, PCWSTR(w.as_ptr()));
                        }
                        Check::Radio(on) => {
                            let _ = AppendMenuW(menu, MF_STRING, *id as usize, PCWSTR(w.as_ptr()));
                            if *on {
                                let _ = CheckMenuRadioItem(menu, *id, *id, *id, MF_BYCOMMAND.0);
                            }
                        }
                    }
                    if *default {
                        let _ = SetMenuDefaultItem(menu, *id, 0);
                    }
                }
                Item::Note(text) => {
                    let w = wide(text);
                    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(w.as_ptr()));
                }
                Item::Sep => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                Item::Sub(text, children) => {
                    let child = CreatePopupMenu().unwrap();
                    fill(child, children);
                    let w = wide(text);
                    let _ = AppendMenuW(menu, MF_POPUP, child.0 as usize, PCWSTR(w.as_ptr()));
                }
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe {
            let _ = DestroyIcon(self.icon);
        }
    }
}
