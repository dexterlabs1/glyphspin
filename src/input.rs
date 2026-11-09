//! Low-level mouse hook on its own thread. The wallpaper sits behind the icon layer and never receives input, so a
//! WH_MOUSE_LL hook watches the mouse and forwards desktop gestures to the controller window.
//!
//! The hook only observes: it never swallows an event (always CallNextHookEx), so normal desktop use (icons,
//! rubber-band selection, double-click, right-click menu) cannot break, even if this process stalls. Middle-drag
//! rotates (Explorer binds nothing to the middle button on the desktop; it's also Blender's orbit), the wheel zooms
//! (except with Ctrl, which is Explorer's icon-size gesture), a left click on bare desktop sends a ripple.
//!
//! The callback does only WindowFromPoint, a few integer compares, atomics and PostMessage. It is installed only
//! while the desktop is visible: LL hooks add a hop to every mouse event system-wide, including in games.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_IN_MDOWN: u32 = WM_APP + 1;
pub const WM_IN_MOVE: u32 = WM_APP + 2;
pub const WM_IN_MUP: u32 = WM_APP + 3;
pub const WM_IN_LDOWN: u32 = WM_APP + 4;
pub const WM_IN_LUP: u32 = WM_APP + 5;
pub const WM_IN_WHEEL: u32 = WM_APP + 6;

const TH_INSTALL: u32 = WM_APP + 50;
const TH_REMOVE: u32 = WM_APP + 51;

// desktop windows the pointer may be over (refreshed on attach)
pub static DESK: [AtomicIsize; 6] = [const { AtomicIsize::new(0) }; 6];
static TARGET: AtomicIsize = AtomicIsize::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static LDOWN: AtomicBool = AtomicBool::new(false);
static MOVE_PENDING: AtomicBool = AtomicBool::new(false);
pub static LAST_MOVE: AtomicU64 = AtomicU64::new(0);
pub static LAST_MOVE_T: AtomicU32 = AtomicU32::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);
pub static HOOKED: AtomicBool = AtomicBool::new(false);

pub fn pack(x: i32, y: i32) -> u64 {
    (x as u32 as u64) | ((y as u32 as u64) << 32)
}

pub fn unpack(v: u64) -> (i32, i32) {
    (v as u32 as i32, (v >> 32) as u32 as i32)
}

pub fn set_desktop_windows(hwnds: &[HWND]) {
    for (i, slot) in DESK.iter().enumerate() {
        slot.store(hwnds.get(i).map(|h| h.0 as isize).unwrap_or(0), Ordering::Relaxed);
    }
}

fn over_desktop(pt: POINT) -> bool {
    unsafe {
        let h = WindowFromPoint(pt);
        if h.is_invalid() {
            return false;
        }
        let v = h.0 as isize;
        DESK.iter().any(|s| {
            let d = s.load(Ordering::Relaxed);
            d != 0 && d == v
        })
    }
}

fn post(msg: u32, w: u64, l: isize) {
    let t = TARGET.load(Ordering::Relaxed);
    if t != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(t as *mut _)), msg, WPARAM(w as usize), LPARAM(l));
        }
    }
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let ms = &*(lp.0 as *const MSLLHOOKSTRUCT);
            let pt = ms.pt;
            match wp.0 as u32 {
                WM_MOUSEMOVE => {
                    if DRAGGING.load(Ordering::Relaxed) {
                        LAST_MOVE.store(pack(pt.x, pt.y), Ordering::Relaxed);
                        LAST_MOVE_T.store(ms.time, Ordering::Relaxed);
                        if !MOVE_PENDING.swap(true, Ordering::AcqRel) {
                            post(WM_IN_MOVE, pack(pt.x, pt.y), ms.time as isize);
                        }
                    }
                }
                WM_MBUTTONDOWN => {
                    if over_desktop(pt) {
                        DRAGGING.store(true, Ordering::Relaxed);
                        post(WM_IN_MDOWN, pack(pt.x, pt.y), ms.time as isize);
                    }
                }
                WM_MBUTTONUP => {
                    if DRAGGING.swap(false, Ordering::Relaxed) {
                        post(WM_IN_MUP, pack(pt.x, pt.y), ms.time as isize);
                    }
                }
                WM_LBUTTONDOWN => {
                    if over_desktop(pt) {
                        LDOWN.store(true, Ordering::Relaxed);
                        post(WM_IN_LDOWN, pack(pt.x, pt.y), ms.time as isize);
                    }
                }
                WM_LBUTTONUP => {
                    if LDOWN.swap(false, Ordering::Relaxed) {
                        post(WM_IN_LUP, pack(pt.x, pt.y), ms.time as isize);
                    }
                }
                WM_MOUSEWHEEL => {
                    if (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) == 0 && over_desktop(pt) {
                        let delta = (ms.mouseData >> 16) as u16 as i16;
                        post(WM_IN_WHEEL, pack(pt.x, pt.y), delta as isize);
                    }
                }
                _ => {}
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// The main thread calls this after handling WM_IN_MOVE, so the next move posts again.
pub fn move_consumed() {
    MOVE_PENDING.store(false, Ordering::Release);
}

/// Start the hook thread (idle until `install`).
pub fn start(target: HWND) {
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("mouse-hook".into())
        .stack_size(64 * 1024)
        .spawn(|| unsafe {
            let mut hook: Option<HHOOK> = None;
            let mut msg = MSG::default();
            // create the thread's message queue before publishing the id, so no PostThreadMessage is lost
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            THREAD_ID.store(GetCurrentThreadId(), Ordering::Release);
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                match msg.message {
                    TH_INSTALL => {
                        if hook.is_none() {
                            let hinst = GetModuleHandleW(None).unwrap_or_default();
                            hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), Some(hinst.into()), 0).ok();
                            HOOKED.store(hook.is_some(), Ordering::Relaxed);
                        }
                    }
                    TH_REMOVE => {
                        if let Some(h) = hook.take() {
                            let _ = UnhookWindowsHookEx(h);
                        }
                        HOOKED.store(false, Ordering::Relaxed);
                        // a drag in progress ends with the hook
                        if DRAGGING.swap(false, Ordering::Relaxed) {
                            post(WM_IN_MUP, 0, 0);
                        }
                        LDOWN.store(false, Ordering::Relaxed);
                    }
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            if let Some(h) = hook.take() {
                let _ = UnhookWindowsHookEx(h);
            }
        })
        .expect("hook thread");
}

fn post_thread(msg: u32) {
    for _ in 0..50 {
        let id = THREAD_ID.load(Ordering::Acquire);
        if id != 0 {
            unsafe {
                let _ = PostThreadMessageW(id, msg, WPARAM(0), LPARAM(0));
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

pub fn install() {
    post_thread(TH_INSTALL);
}

pub fn remove() {
    post_thread(TH_REMOVE);
}

pub fn stop() {
    post_thread(WM_QUIT);
}
