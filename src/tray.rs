//! Tray icon and its menu. The icon is the embedded resource 1 (assets/glyphspin.ico, made by tools/brand.py);
//! if it's missing, a small teal ring is drawn at startup instead.

use crate::settings::{ACCENTS, Settings};
use crate::shapes::SHAPES;
use crate::util::wide;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

pub const WM_TRAY: u32 = WM_APP + 30;
const TRAY_ID: u32 = 1;

pub const CMD_REROLL: u32 = 100;
pub const CMD_SHAPE: u32 = 200; // +0 random, +1.. SHAPES
pub const CMD_STYLE: u32 = 300;
pub const CMD_SIZE: u32 = 310;
pub const CMD_ACCENT: u32 = 320;
pub const CMD_FPS: u32 = 330;
pub const CMD_HDR: u32 = 340;
pub const CMD_HIGHLIGHT: u32 = 350;
pub const CMD_EFFECTS: u32 = 360;
pub const CMD_AUDIO_SRC: u32 = 370;
pub const CMD_AUDIO_LVL: u32 = 380;
pub const CMD_OLED: u32 = 410;
pub const CMD_DIM: u32 = 420;
pub const CMD_BLACK: u32 = 430;
pub const CMD_AUTOREROLL: u32 = 440;
pub const CMD_PAUSE: u32 = 400;
pub const CMD_STARTUP: u32 = 401;
pub const CMD_QUIT: u32 = 499;

pub const STYLES: [&str; 3] = ["auto", "ramp", "braille"];
pub const SIZES: [&str; 3] = ["fine", "medium", "coarse"];
pub const FPS: [u32; 3] = [15, 30, 60];
pub const HDRS: [&str; 3] = ["auto", "on", "off"];
pub const HIGHLIGHTS: [u32; 5] = [0, 25, 50, 75, 100];
pub const LEVELS: [&str; 3] = ["off", "subtle", "full"];
pub const SOURCES: [&str; 3] = ["apple", "all", "off"];
pub const DIM_MIN: [u32; 4] = [5, 10, 20, 0];
pub const BLACK_MIN: [u32; 4] = [15, 30, 60, 0];
pub const REROLL_MIN: [u32; 4] = [15, 25, 45, 0];

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
    pub fn menu(&self, s: &Settings, info: &[String], startup_on: bool) -> u32 {
        unsafe {
            let m = CreatePopupMenu().unwrap();
            let item = |menu: HMENU, id: u32, text: &str, checked: bool| {
                let w = wide(text);
                let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
                let _ = AppendMenuW(menu, flags, id as usize, PCWSTR(w.as_ptr()));
            };
            let radio = |menu: HMENU, id: u32, text: &str, checked: bool| {
                let w = wide(text);
                let _ = AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(w.as_ptr()));
                if checked {
                    let _ = CheckMenuRadioItem(menu, id, id, id, MF_BYCOMMAND.0);
                }
            };
            let sub = |menu: HMENU, text: &str, child: HMENU| {
                let w = wide(text);
                let _ = AppendMenuW(menu, MF_POPUP, child.0 as usize, PCWSTR(w.as_ptr()));
            };
            let note = |menu: HMENU, text: &str| {
                let w = wide(text);
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(w.as_ptr()));
            };
            let sep = |menu: HMENU| {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            };

            item(m, CMD_REROLL, "Reroll", false);
            let _ = SetMenuDefaultItem(m, CMD_REROLL, 0);

            let shapes = CreatePopupMenu().unwrap();
            radio(shapes, CMD_SHAPE, "Random", s.shape == "random");
            sep(shapes);
            for (i, name) in SHAPES.iter().enumerate() {
                radio(shapes, CMD_SHAPE + 1 + i as u32, &name.replace('_', " "), s.shape == *name);
            }
            sub(m, "Shape", shapes);

            let look = CreatePopupMenu().unwrap();
            for (i, v) in STYLES.iter().enumerate() {
                radio(look, CMD_STYLE + i as u32, &format!("Style: {v}"), s.style == *v);
            }
            sep(look);
            for (i, v) in SIZES.iter().enumerate() {
                radio(look, CMD_SIZE + i as u32, &format!("Glyphs: {v}"), s.glyph_size == *v);
            }
            sep(look);
            for (i, (name, _)) in ACCENTS.iter().enumerate() {
                radio(look, CMD_ACCENT + i as u32, &format!("Colour: {name}"), s.accent == *name);
            }
            sub(m, "Look", look);

            let fx = CreatePopupMenu().unwrap();
            for (i, v) in LEVELS.iter().enumerate() {
                radio(fx, CMD_EFFECTS + i as u32, v, s.effects == *v);
            }
            sub(m, "Effects", fx);

            let au = CreatePopupMenu().unwrap();
            note(au, "React to");
            for (i, (v, label)) in SOURCES.iter().zip(["Apple Music", "All audio", "Off"]).enumerate() {
                radio(au, CMD_AUDIO_SRC + i as u32, label, s.audio_source == *v);
            }
            sep(au);
            note(au, "Intensity");
            for (i, v) in LEVELS.iter().enumerate() {
                radio(au, CMD_AUDIO_LVL + i as u32, v, s.audio_level == *v);
            }
            sub(m, "Music", au);

            let hd = CreatePopupMenu().unwrap();
            for (i, v) in HDRS.iter().enumerate() {
                radio(hd, CMD_HDR + i as u32, &format!("HDR: {v}"), s.hdr == *v);
            }
            sep(hd);
            note(hd, "Highlights");
            for (i, v) in HIGHLIGHTS.iter().enumerate() {
                radio(hd, CMD_HIGHLIGHT + i as u32, &format!("{v}%"), s.highlight == *v);
            }
            sep(hd);
            for line in info {
                note(hd, line);
            }
            sub(m, "Display", hd);

            let ol = CreatePopupMenu().unwrap();
            item(ol, CMD_OLED, "OLED care", s.oled_care);
            sep(ol);
            let mins = |v: u32| if v == 0 { "never".to_string() } else { format!("{v} min") };
            note(ol, "Dim when idle");
            for (i, v) in DIM_MIN.iter().enumerate() {
                radio(ol, CMD_DIM + i as u32, &mins(*v), s.idle_dim_min == *v);
            }
            sep(ol);
            note(ol, "Then black after");
            for (i, v) in BLACK_MIN.iter().enumerate() {
                radio(ol, CMD_BLACK + i as u32, &mins(*v), s.idle_black_min == *v);
            }
            sep(ol);
            note(ol, "New shape every");
            for (i, v) in REROLL_MIN.iter().enumerate() {
                radio(ol, CMD_AUTOREROLL + i as u32, &mins(*v), s.auto_reroll_min == *v);
            }
            sub(m, "OLED care", ol);

            let fp = CreatePopupMenu().unwrap();
            for (i, v) in FPS.iter().enumerate() {
                radio(fp, CMD_FPS + i as u32, &format!("{v} fps max"), s.fps_cap == *v);
            }
            sub(m, "Frame rate", fp);

            sep(m);
            item(m, CMD_PAUSE, "Pause", s.paused);
            item(m, CMD_STARTUP, "Start with Windows", startup_on);
            sep(m);
            item(m, CMD_QUIT, "Quit", false);

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

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe {
            let _ = DestroyIcon(self.icon);
        }
    }
}
