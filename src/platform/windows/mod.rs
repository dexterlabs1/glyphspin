//! The Windows backend: window plumbing (a view parented behind the desktop icons), the frame loop, pause logic,
//! tray commands and the instance side of the CLI (--quit, --dump, --send). The per-frame work is the shared
//! glyphspin_core::engine::Engine; this file feeds it input, timing and visibility and presents its cells.

mod audio;
mod desktop;
mod glyphs;
mod gpu;
mod hdr;
mod input;
mod tray;
mod watch;

use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::RemoteDesktop::*;
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::{Msg, Platform, Surface};
use glyphspin_core::engine::{self, Consts, Engine};
use glyphspin_core::settings::Settings;
use glyphspin_core::{dump, logf, menu, scene, shapes, util};

const CTL_CLASS: PCWSTR = w!("GlyphspinCtl");
const VIEW_CLASS: PCWSTR = w!("GlyphspinView");
const COPY_DUMP: usize = 0xD0;
const COPY_CMD: usize = 0xD1;

const T_WATCH: usize = 1;
const T_WATCH_SOON: usize = 2;
const T_GPU_RETRY: usize = 3;
const T_CLICK: usize = 4;
const T_REATTACH: usize = 5;
const T_HOLD: usize = 6;

struct Stats {
    compose_ms: f64,
    draw_ms: f64,
    frames: u64,
    drawn: u64,
    cpu_ms: f64,
    lat_sum: f64,
    lat_n: u64,
    lat_last: f64,
}

struct App {
    ctl: HWND,
    view: HWND,
    host: Option<desktop::Host>,
    gpu: Option<gpu::Gpu>,
    factory: Option<IDXGIFactory2>,
    output: Option<IDXGIOutput6>,
    adapter: Option<IDXGIAdapter1>,
    mon_rect: RECT,
    disp: hdr::DisplayInfo,
    s: Settings,
    /// scene, cell grid and OLED care
    eng: Engine,
    tray: Option<tray::Tray>,
    // state
    visible: bool,
    quns: Option<&'static str>,
    locked: bool,
    display_on: bool,
    active: bool,
    no_pause: bool,
    last_frame: Instant,
    next_due: Option<Instant>,
    last_onset_seen: f64,
    capture: Option<audio::Capture>,
    dump_path: Option<String>,
    /// test: record N consecutive frames at a fixed 1/15 s step (dir, remaining, index)
    seq: Option<(String, u32, u32)>,
    quit: bool,
    taskbar_msg: u32,
    // input
    ldown: Option<(i32, i32, u32)>,
    click: Option<(i32, i32)>,
    last_mdown: (i32, i32, u32),
    suppress_drag: bool,
    stats: Stats,
    timer: HANDLE,
    winevent: Vec<HWINEVENTHOOK>,
    shown: bool,
    gpu_retry_ms: u32,
    exploding: bool,
}

static mut APP: *mut App = std::ptr::null_mut();

/// Test-only switches (CLI): simulate an HDR display with this SDR white level; capture this exe instead of
/// AppleMusic.exe; capture without waiting for SMTC to report playback.
struct TestFlags {
    sim_hdr_white: Option<f32>,
    audio_exe: Option<String>,
    audio_ungated: bool,
    sim_idle: Option<f32>,
}
static mut TEST: TestFlags = TestFlags { sim_hdr_white: None, audio_exe: None, audio_ungated: false, sim_idle: None };

#[allow(static_mut_refs)]
fn test_flags() -> &'static TestFlags {
    unsafe { &TEST }
}

fn app() -> &'static mut App {
    unsafe { &mut *APP }
}

// ------------------------------------------------------------------ platform

pub struct Windows;

pub(super) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Platform for Windows {
    fn app_dir() -> std::path::PathBuf {
        let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join("Glyphspin");
        if !d.exists() {
            let _ = std::fs::create_dir_all(&d);
            // one-time migration from the pre-release name
            let old = base.join("ascii-wallpaper").join("settings.json");
            if old.exists() {
                let _ = std::fs::copy(&old, d.join("settings.json"));
            }
        }
        d
    }

    fn build_atlas(cw_target: u32) -> glyphspin_core::atlas::Atlas {
        glyphs::build(cw_target)
    }

    fn send(kind: Msg, text: &str) -> bool {
        send_copy(if kind == Msg::Dump { COPY_DUMP } else { COPY_CMD }, text)
    }

    fn quit_running() -> i32 {
        cli_quit()
    }

    fn test_cli(args: &[String]) -> Option<i32> {
        let i = args.iter().position(|a| a == "--meter")?;
        let secs = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(5.0);
        Some(audio::meter(secs))
    }

    fn run(args: &[String]) {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let _mutex = CreateMutexW(None, true, w!("Local\\Glyphspin.Singleton"));
            if GetLastError() == ERROR_ALREADY_EXISTS {
                // already running: a second launch rerolls the shape
                crate::cli_send("reroll");
                return;
            }
            util::log_init();
            std::panic::set_hook(Box::new(|info| {
                util::log(&format!("panic: {info}"));
                desktop::refresh_wallpaper();
            }));
            // a hard crash (access violation) must not leave the last frame stuck on the desktop either
            windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(crash_filter));
            run(args);
        }
    }
}
unsafe extern "system" fn crash_filter(info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    unsafe {
        let code = (*(*info).ExceptionRecord).ExceptionCode.0;
        util::log(&format!("crash: exception {code:#x}"));
    }
    desktop::refresh_wallpaper();
    0 // EXCEPTION_CONTINUE_SEARCH
}

fn find_instance() -> Option<HWND> {
    unsafe { FindWindowW(CTL_CLASS, None).ok() }
}

fn cli_quit() -> i32 {
    let Some(h) = find_instance() else {
        println!("not running");
        return 0;
    };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let proc = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok();
        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        if let Some(p) = proc {
            let r = WaitForSingleObject(p, 5000);
            let _ = CloseHandle(p);
            if r == WAIT_OBJECT_0 {
                println!("quit ok (pid {pid})");
                return 0;
            }
            println!("timed out waiting for pid {pid}");
            return 1;
        }
        println!("posted WM_CLOSE");
        0
    }
}

fn send_copy(kind: usize, text: &str) -> bool {
    let Some(h) = find_instance() else { return false };
    let wt = wide(text);
    let cds = COPYDATASTRUCT { dwData: kind, cbData: (wt.len() * 2) as u32, lpData: wt.as_ptr() as *mut _ };
    unsafe {
        let mut res = 0usize;
        SendMessageTimeoutW(h, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_NORMAL, 5000,
                            Some(&mut res));
    }
    true
}

// ------------------------------------------------------------------ setup

fn register_classes() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW { lpfnWndProc: Some(ctl_proc), hInstance: hinst.into(), lpszClassName: CTL_CLASS,
                             ..Default::default() };
        RegisterClassW(&wc);
        let wc = WNDCLASSW { lpfnWndProc: Some(view_proc), hInstance: hinst.into(), lpszClassName: VIEW_CLASS,
                             ..Default::default() };
        RegisterClassW(&wc);
    }
}

unsafe fn run(args: &[String]) {
    unsafe {
        register_classes();
        let s = Settings::load();
        let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
        TEST = TestFlags {
            sim_hdr_white: arg("--sim-hdr").and_then(|v| v.parse().ok()),
            audio_exe: arg("--audio-exe"),
            audio_ungated: args.iter().any(|a| a == "--audio-ungated"),
            sim_idle: arg("--sim-idle").and_then(|v| v.parse().ok()),
        };
        let seed = arg("--seed").and_then(|v| v.parse().ok()).unwrap_or_else(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
        });
        let shape_name = arg("--shape").or_else(|| (s.shape != "random").then(|| s.shape.clone()));
        let shape = shape_name.and_then(|n| shapes::SHAPES.iter().copied().find(|x| *x == n));
        let mut a = App {
            ctl: HWND::default(),
            view: HWND::default(),
            host: None,
            gpu: None,
            factory: None,
            output: None,
            adapter: None,
            mon_rect: RECT::default(),
            disp: hdr::DisplayInfo::default(),
            eng: Engine::new(seed, shape),
            s,
            tray: None,
            visible: true,
            quns: None,
            locked: false,
            display_on: true,
            active: false,
            no_pause: args.iter().any(|a| a == "--no-pause"),
            last_frame: Instant::now(),
            next_due: None,
            last_onset_seen: 0.0,
            capture: None,
            dump_path: None,
            seq: None,
            quit: false,
            taskbar_msg: RegisterWindowMessageW(w!("TaskbarCreated")),
            ldown: None,
            click: None,
            last_mdown: (0, 0, 0),
            suppress_drag: false,
            stats: Stats { compose_ms: 0.0, draw_ms: 0.0, frames: 0, drawn: 0, cpu_ms: 0.0, lat_sum: 0.0, lat_n: 0, lat_last: 0.0 },
            timer: CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
                .unwrap_or_default(),
            winevent: Vec::new(),
            shown: false,
            gpu_retry_ms: 2000,
            exploding: false,
        };
        APP = &mut a;
        let hinst = GetModuleHandleW(None).unwrap();
        a.ctl = CreateWindowExW(WS_EX_TOOLWINDOW, CTL_CLASS, w!("Glyphspin"), WS_POPUP, 0, 0, 0, 0, None, None,
                                Some(hinst.into()), None)
            .unwrap();
        let _ = WTSRegisterSessionNotification(a.ctl, NOTIFY_FOR_THIS_SESSION);
        let _ = RegisterPowerSettingNotification(HANDLE(a.ctl.0), &GUID_CONSOLE_DISPLAY_STATE,
                                                 DEVICE_NOTIFY_WINDOW_HANDLE);
        for (lo, hi) in [(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                         (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
                         (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND)] {
            let h = SetWinEventHook(lo, hi, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
            if !h.is_invalid() {
                a.winevent.push(h);
            }
        }
        a.tray = Some(tray::Tray::new(a.ctl));
        input::start(a.ctl);
        audio::start_watcher(a.ctl);
        setup_display();
        attach_view();
        SetTimer(Some(a.ctl), T_WATCH, 1000, None);
        watch_tick();
        update_tip();
        message_loop();
        shutdown();
        APP = std::ptr::null_mut();
    }
}

fn message_loop() {
    unsafe {
        let mut msg = MSG::default();
        loop {
            let a = app();
            if a.quit {
                break;
            }
            // arm the frame timer when a frame is scheduled; otherwise sleep until a message arrives
            let handles = [a.timer];
            let wait = if let (true, Some(due)) = (a.active, a.next_due) {
                let now = Instant::now();
                if due > now {
                    let hundred_ns = -(((due - now).as_nanos() / 100) as i64).max(1);
                    let _ = SetWaitableTimer(a.timer, &hundred_ns, 0, None, None, false);
                    MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                } else {
                    WAIT_OBJECT_0
                }
            } else {
                MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
            };
            let _ = wait;
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    app().quit = true;
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let a = app();
            if a.quit {
                break;
            }
            if a.active {
                if let Some(due) = a.next_due {
                    if Instant::now() >= due {
                        frame();
                    }
                }
            }
        }
    }
}

fn setup_display() {
    let a = app();
    let (_, rect, name) = desktop::primary_monitor();
    a.mon_rect = rect;
    a.factory = gpu::create_factory().ok();
    a.adapter = None;
    a.output = None;
    if let Some(f) = &a.factory {
        if let Ok(f1) = windows::core::Interface::cast::<IDXGIFactory1>(f) {
            if let Some((ad, out)) = hdr::find_output(&f1, &name) {
                a.adapter = Some(ad);
                a.output = Some(out);
            }
        }
    }
    a.disp = hdr::query(a.output.as_ref(), &name);
    if let Some(wl) = test_flags().sim_hdr_white {
        a.disp.hdr_active = true;
        a.disp.sdr_white_nits = wl;
    }
    logf!("monitor {} rect {:?} {:?}", name, (rect.left, rect.top, rect.right, rect.bottom), a.disp);
}

fn want_hdr() -> bool {
    let a = app();
    match a.s.hdr.as_str() {
        "on" => true,
        "off" => false,
        _ => a.disp.hdr_active,
    }
}

fn mon_size() -> (u32, u32) {
    let r = app().mon_rect;
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

/// Build the glyph atlas and the cell grid for the current monitor and glyph size.
fn rebuild_grid() {
    let a = app();
    let (w, h) = mon_size();
    let cw_target = Engine::cw_target(&a.s, h);
    let atlas = glyphs::build(cw_target);
    a.eng.set_grid(w, h, &atlas, cw_target);
    logf!("grid {}x{} cells of {}x{} px, font '{}', {} glyphs drawn procedurally", a.eng.cols, a.eng.rows, a.eng.cw,
          a.eng.ch, atlas.face, atlas.missing);
    if let Some(g) = a.gpu.as_mut() {
        if let Err(e) = g.set_atlas(&atlas.pixels, atlas.w, atlas.h) {
            logf!("atlas upload failed {e:?}");
        }
    }
}

fn create_gpu() -> bool {
    let a = app();
    let Some(f) = a.factory.clone() else { return false };
    match gpu::Gpu::new(f, a.adapter.as_ref()) {
        Ok(g) => {
            a.gpu = Some(g);
            true
        }
        Err(e) => {
            logf!("gpu init failed: {e:?}");
            false
        }
    }
}

/// The GPU is gone or never came up: hide the view (so the normal wallpaper shows) and retry with backoff.
fn gpu_unavailable() {
    let a = app();
    if let Some(g) = a.gpu.as_mut() {
        g.unbind();
    }
    a.gpu = None;
    set_shown(false);
    unsafe {
        SetTimer(Some(a.ctl), T_GPU_RETRY, a.gpu_retry_ms, None);
    }
    logf!("gpu unavailable; retry in {} ms", a.gpu_retry_ms);
    a.gpu_retry_ms = (a.gpu_retry_ms * 2).min(60_000);
}

fn set_shown(on: bool) {
    let a = app();
    if a.shown != on && !a.view.is_invalid() {
        desktop::show(a.view, on);
        a.shown = on;
    }
}

fn desk_windows() -> Vec<HWND> {
    let a = app();
    match &a.host {
        Some(h) => vec![h.progman, h.parent, h.workerw, h.defview, h.listview, a.view],
        None => vec![],
    }
}

fn attach_view() -> bool {
    let a = app();
    let Some(host) = desktop::find_host() else {
        logf!("no wallpaper host found");
        return false;
    };
    logf!("host {:?}", host);
    unsafe {
        if !a.view.is_invalid() && IsWindow(Some(a.view)).as_bool() {
            if let Some(g) = a.gpu.as_mut() {
                g.unbind();
            }
            let _ = DestroyWindow(a.view);
        }
        let hinst = GetModuleHandleW(None).unwrap();
        a.view = CreateWindowExW(WS_EX_NOACTIVATE, VIEW_CLASS, w!("Glyphspin view"), WS_POPUP, 0, 0, 1, 1,
                                 None, None, Some(hinst.into()), None)
            .unwrap_or_default();
    }
    if !desktop::attach(a.view, &host, a.mon_rect) {
        logf!("attach failed");
        return false;
    }
    a.host = Some(host);
    input::set_desktop_windows(&desk_windows());
    a.shown = false;
    if a.gpu.is_none() && !create_gpu() {
        gpu_unavailable();
        return true;
    }
    bind_gpu();
    true
}

/// Bind the GPU to the view window, size the swapchain and upload the atlas.
fn bind_gpu() {
    let a = app();
    let (w, h) = mon_size();
    let hdr = want_hdr();
    let Some(g) = a.gpu.as_mut() else { return };
    if let Err(e) = g.bind_window(a.view) {
        logf!("bind failed {e:?}");
    }
    if let Err(e) = g.resize(w, h, hdr) {
        logf!("resize failed {e:?}");
    }
    rebuild_grid();
    a.next_due = Some(Instant::now());
}

fn reattach() {
    logf!("reattaching");
    let a = app();
    if let Some(g) = a.gpu.as_mut() {
        g.unbind();
    }
    a.host = None;
    if !attach_view() {
        unsafe {
            SetTimer(Some(a.ctl), T_REATTACH, 1500, None);
        }
    }
    update_active();
}

fn shutdown() {
    let a = app();
    input::remove();
    input::stop();
    a.capture = None;
    a.tray = None;
    for h in a.winevent.drain(..) {
        unsafe {
            let _ = UnhookWinEvent(h);
        }
    }
    if let Some(g) = a.gpu.as_mut() {
        g.unbind();
    }
    unsafe {
        if !a.view.is_invalid() && IsWindow(Some(a.view)).as_bool() {
            let _ = DestroyWindow(a.view);
        }
    }
    a.gpu = None;
    desktop::refresh_wallpaper();
    logf!("shutdown complete");
}

// ------------------------------------------------------------------ pause logic

fn watch_tick() {
    let a = app();
    if let Some(host) = a.host {
        if !desktop::still_attached(a.view, &host) {
            reattach();
            return;
        }
        if !desktop::z_order_ok(a.view, &host) {
            logf!("z-order drifted; restoring");
            desktop::place(a.view, &host, a.mon_rect);
        }
    } else {
        return;
    }
    if let Some(f) = &a.factory {
        if !unsafe { f.IsCurrent() }.as_bool() {
            on_display_change();
            return;
        }
    }
    let vis = watch::desktop_visible(a.mon_rect, &desk_windows());
    if vis != a.visible {
        logf!("desktop visible={vis}; centre hit: {}", watch::centre_hit(a.mon_rect));
    }
    a.visible = vis;
    a.quns = watch::notification_block();
    update_active();
    // asleep at black: resume on input
    if a.active && a.next_due.is_none() && idle_seconds() < 2.0 {
        wake();
    }
}

fn update_active() {
    let a = app();
    let visible = a.no_pause || (a.visible && a.quns.is_none());
    let display_ok = a.display_on || a.no_pause;
    let active = !a.s.paused && visible && !a.locked && display_ok && a.host.is_some() && a.gpu.is_some();
    if active != a.active {
        a.active = active;
        logf!("active={} (paused={} visible={} quns={:?} locked={} display_on={})", active, a.s.paused, a.visible,
              a.quns, a.locked, a.display_on);
        if active {
            input::install();
            a.last_frame = Instant::now();
            a.next_due = Some(Instant::now());
        } else {
            input::remove();
            a.eng.scene.drag_end();
            a.next_due = None;
        }
    }
    update_capture();
}

fn update_capture() {
    let a = app();
    let (apple_playing, any_playing) = {
        let m = audio::MEDIA.lock().unwrap();
        (m.apple_playing, m.any_playing)
    };
    let want: Option<audio::Source> = if !a.active || a.s.audio_level == "off" {
        None
    } else {
        match a.s.audio_source.as_str() {
            "apple" if apple_playing || test_flags().audio_ungated => {
                match test_flags().audio_exe.clone() {
                    Some(exe) => match exe.strip_prefix("pid:") {
                        Some(pid) => pid.parse().ok().map(audio::Source::Process),
                        None => audio::find_process(&exe).map(audio::Source::Process),
                    },
                    // the process with Apple Music's audio session (re-checked every watch tick), else the app
                    None => {
                        let cur = match a.capture.as_ref().map(|c| c.source) {
                            Some(audio::Source::Process(p)) => Some(p),
                            _ => None,
                        };
                        audio::apple_audio_process(cur).or_else(|| audio::find_process("AppleMusic.exe"))
                            .map(audio::Source::Process)
                    }
                }
            }
            "all" if any_playing || test_flags().audio_ungated => Some(audio::Source::Endpoint),
            _ => None,
        }
    };
    let keep = match (&a.capture, want) {
        (Some(c), Some(w)) => c.source == w && c.running(),
        (None, None) => true,
        _ => false,
    };
    if !keep {
        a.capture = None; // joins the old thread
        if let Some(w) = want {
            a.capture = Some(audio::Capture::start(w));
            a.next_due = a.next_due.or(Some(Instant::now()));
        }
    }
}

fn on_display_change() {
    logf!("display change");
    let a = app();
    setup_display();
    // the old device may belong to a different adapter now; rebuild everything GPU-side
    if let Some(g) = a.gpu.as_mut() {
        g.unbind();
    }
    a.gpu = None;
    if let Some(host) = a.host {
        desktop::place(a.view, &host, a.mon_rect);
    }
    if create_gpu() {
        bind_gpu();
    } else {
        gpu_unavailable();
    }
    update_tip();
    update_active();
}

// ------------------------------------------------------------------ frame

fn params() -> scene::Params {
    engine::params(&app().s)
}

fn consts() -> Consts {
    let a = app();
    let hdr = a.gpu.as_ref().map(|g| g.hdr).unwrap_or(false);
    let white = if hdr && a.disp.hdr_active { (a.disp.sdr_white_nits / 80.0).max(1.0) } else { 1.0 };
    let max = if a.disp.max_nits >= 100.0 { a.disp.max_nits } else { a.s.peak_nits };
    let peak = (a.s.peak_nits.min(max) / 80.0).max(white);
    a.eng.consts(&a.s, hdr, white, peak)
}

fn idle_seconds() -> f32 {
    // test: pretend the last input was `sim_idle` seconds before launch
    if let Some(v) = test_flags().sim_idle {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        return v + T0.get_or_init(Instant::now).elapsed().as_secs_f32();
    }
    unsafe {
        let mut li = windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO {
            cbSize: std::mem::size_of::<windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if windows::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo(&mut li).as_bool() {
            windows::Win32::System::SystemInformation::GetTickCount().wrapping_sub(li.dwTime) as f32 / 1000.0
        } else {
            0.0
        }
    }
}

fn frame() {
    let a = app();
    let now = Instant::now();
    let dt = if a.seq.is_some() { 1.0 / 15.0 } else { (now - a.last_frame).as_secs_f32().clamp(0.0, 0.1) };
    a.last_frame = now;
    let t0 = Instant::now();
    let p = params();
    let af = if a.capture.is_some() { *audio::FEATURES.lock().unwrap() } else { scene::AudioFrame::default() };
    // OLED care: idle dimming, auto reroll; then the scene, the APL limiter and the pixel orbit
    if a.eng.pre_step(&a.s, dt, idle_seconds()) {
        update_tip();
    }
    let engine::Step { busy, changed } = a.eng.step(&p, dt, &af);
    a.stats.compose_ms += t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = Instant::now();
    a.stats.frames += 1;
    let want_dump = a.dump_path.is_some() || a.seq.is_some();
    if changed || want_dump {
        let c = consts();
        let (cols, rows) = (a.eng.cols as u32, a.eng.rows as u32);
        if let Some(g) = a.gpu.as_mut() {
            let r = g.upload_cells(&a.eng.scene.cells, cols, rows).and_then(|_| g.draw(&c, want_dump));
            match r {
                Ok(Some((px, _))) => {
                    a.gpu_retry_ms = 2000;
                    if let Some(path) = a.dump_path.take() {
                        let (w, h, hdr) = (g.width as usize, g.height as usize, g.hdr);
                        write_dump(&path, &px, w, h, hdr, c.white);
                    } else if let Some((dir, left, idx)) = a.seq.take() {
                        // half-resolution 8-bit frames (SDR swapchain) for making the README loop
                        let (w, h) = (g.width as usize, g.height as usize);
                        if !g.hdr {
                            let (hw, hh) = (w / 2, h / 2);
                            let mut rgb = vec![0u8; hw * hh * 3];
                            for y in 0..hh {
                                for x in 0..hw {
                                    for c in 0..3 {
                                        let mut acc = 0u32;
                                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                                            acc += px[((y * 2 + dy) * w + x * 2 + dx) * 4 + (2 - c)] as u32;
                                        }
                                        rgb[(y * hw + x) * 3 + c] = (acc / 4) as u8;
                                    }
                                }
                            }
                            let p = std::path::Path::new(&dir).join(format!("f{idx:04}.bmp"));
                            let _ = util::write_bmp(&p, hw, hh, &rgb);
                        }
                        if left > 1 {
                            a.seq = Some((dir, left - 1, idx + 1));
                        } else {
                            let _ = std::fs::write(std::path::Path::new(&dir).join("done.txt"), "done");
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    logf!("draw failed {e:?}");
                    if g.device_lost() || e.code() == DXGI_ERROR_DEVICE_REMOVED || e.code() == DXGI_ERROR_DEVICE_RESET {
                        logf!("device lost");
                        gpu_unavailable();
                        update_active();
                        return;
                    }
                }
            }
            a.stats.drawn += 1;
        }
        if a.gpu.as_ref().map(|g| g.ready()).unwrap_or(false) {
            set_shown(true);
        }
    }
    a.stats.draw_ms += t1.elapsed().as_secs_f64() * 1000.0;
    // capture -> photons latency for the onset this frame reacted to
    if a.eng.scene.last_onset_capture_s != a.last_onset_seen {
        a.last_onset_seen = a.eng.scene.last_onset_capture_s;
        let lat = audio::qpc_seconds() - a.last_onset_seen;
        if lat > 0.0 && lat < 2.0 {
            a.stats.lat_sum += lat;
            a.stats.lat_n += 1;
            a.stats.lat_last = lat;
        }
    }
    a.stats.cpu_ms += t0.elapsed().as_secs_f64() * 1000.0;
    // faded to black: stop drawing until there is input again (the 1 Hz watch tick and the hook wake it)
    if a.eng.asleep() && !want_dump {
        a.next_due = None;
        return;
    }
    let next = now + std::time::Duration::from_secs_f32(Engine::interval(&a.s, busy, want_dump));
    a.next_due = Some(next.max(Instant::now()));
}

fn write_dump(path: &str, px: &[u8], w: usize, h: usize, hdr: bool, white: f32) {
    let p = std::path::Path::new(path);
    let st = dump::write(p, px, w, h, hdr, white, 80.0);
    let a = app();
    let info = format!(
        "size {w}x{h} | swapchain {} | display colorspace {} hdr_active {} sdr_white {:.0} nits max_lum {:.0} nits | \
         white scale {:.2} peak {:.0} nits | frame max {:.1} nits at {:?}, avg {:.3} nits, {} px above SDR white | \
         shape {} grid {}x{} cell {}x{} obj cells {}",
        if hdr { "R16G16B16A16_FLOAT scRGB (G10 P709)" } else { "B8G8R8A8_UNORM sRGB" },
        a.disp.color_space, a.disp.hdr_active, a.disp.sdr_white_nits, a.disp.max_nits, white,
        consts().peak * 80.0, st.max, st.max_px, st.avg, st.over_white, a.eng.scene.shape_name(), a.eng.cols,
        a.eng.rows, a.eng.cw, a.eng.ch, a.eng.scene.object_cells()
    );
    let _ = std::fs::write(p.with_extension("txt"), info);
}

fn update_tip() {
    let a = app();
    let m = audio::MEDIA.lock().unwrap();
    let mut tip = format!("Glyphspin - {}", a.eng.scene.shape_name().replace('_', " "));
    if m.apple_playing && !m.title.is_empty() {
        tip = format!("{tip}\n{}", m.title);
    }
    drop(m);
    if a.s.paused {
        tip.push_str(" (paused)");
    }
    if let Some(t) = &a.tray {
        t.set_tip(&tip);
    }
}

// ------------------------------------------------------------------ commands

fn startup_enabled() -> bool {
    unsafe {
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), w!("Glyphspin"),
                     RRF_RT_REG_SZ, None, None, Some(&mut size))
            .is_ok()
    }
}

/// Only ever called from the tray toggle.
fn set_startup(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"), Some(0),
                         KEY_SET_VALUE, &mut key)
            .is_err()
        {
            return;
        }
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v = wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
            let _ = RegSetValueExW(key, w!("Glyphspin"), Some(0), REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, w!("Glyphspin"));
        }
        let _ = RegCloseKey(key);
    }
}

/// Draw the next frame now (input, commands); the frame then picks its own rate.
fn wake() {
    let a = app();
    if a.active {
        a.next_due = Some(Instant::now());
    }
}

fn reroll(name: Option<&'static str>) {
    let a = app();
    let fx = params().effects;
    a.eng.scene.reroll(name, fx, None);
    update_tip();
    wake();
}

fn apply_setting(key: &str, val: &str) {
    let a = app();
    let old = a.s.clone();
    if !a.s.set(key, val) {
        return;
    }
    if a.s == old {
        return;
    }
    a.s.save();
    if a.s.shape != old.shape {
        let lock = shapes::SHAPES.iter().copied().find(|x| *x == a.s.shape);
        a.eng.scene.shape_lock = lock;
        if let Some(n) = lock {
            reroll(Some(n));
        }
    }
    if a.s.glyph_size != old.glyph_size {
        rebuild_grid();
    }
    if a.s.hdr != old.hdr {
        let (w, h) = mon_size();
        let hdr = want_hdr();
        if let Some(g) = a.gpu.as_mut() {
            if let Err(e) = g.resize(w, h, hdr) {
                logf!("resize failed {e:?}");
            }
        }
    }
    update_active();
    update_tip();
    wake();
}

fn command(cmd: u32) {
    let a = app();
    match menu::action(cmd, &a.s) {
        menu::Action::Reroll => {
            let lock = a.eng.scene.shape_lock;
            reroll(lock.filter(|n| *n != a.eng.scene.shape_name()));
        }
        menu::Action::Set(k, v) => apply_setting(k, &v),
        menu::Action::ToggleStartup => {
            let on = !startup_enabled();
            set_startup(on);
            a.s.start_with_windows = on;
            a.s.save();
        }
        menu::Action::Quit => unsafe {
            let _ = PostMessageW(Some(a.ctl), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        menu::Action::None => {}
    }
}

fn text_command(cmd: &str) {
    let a = app();
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    logf!("command: {cmd}");
    match parts.as_slice() {
        ["reroll"] => reroll(None),
        ["reroll", name] => reroll(shapes::SHAPES.iter().copied().find(|x| x == name)),
        // test hooks for the effects (same code paths the gestures use)
        ["ripple", ..] | ["fx", ..] => {
            if a.eng.fx_command(&a.s, parts.as_slice()) {
                wake();
            }
        }
        ["set", kv] => {
            if let Some((k, v)) = kv.split_once('=') {
                apply_setting(k, v);
            }
        }
        ["taskbarcreated"] => unsafe {
            let _ = PostMessageW(Some(a.ctl), a.taskbar_msg, WPARAM(0), LPARAM(0));
        },
        ["stats", path] => {
            let st = &a.stats;
            let m = audio::MEDIA.lock().unwrap();
            let f = *audio::FEATURES.lock().unwrap();
            let text = format!(
                "active {} visible {} quns {:?} locked {} display_on {} paused {} hooked {} centre_hit {}\n\
                 frames {} drawn {} avg_cpu_ms {:.3} (step+compose {:.3}, upload+draw+present {:.3})\n\
                 audio: source {} level {} capture {:?} apple_playing {} apple_present {} any_playing {} title '{}'\n\
                 features bass {:.2} mid {:.2} high {:.2} loud {:.2} onsets {}\n\
                 onset->frame latency: last {:.1} ms avg {:.1} ms over {}\n\
                 display {:?}\n\
                 grid {}x{} cell {}x{} shape {} theme {} black {} rotate {} fading {}",
                a.active, a.visible, a.quns, a.locked, a.display_on, a.s.paused,
                input::HOOKED.load(std::sync::atomic::Ordering::Relaxed), watch::centre_hit(a.mon_rect),
                st.frames, st.drawn, if st.frames > 0 { st.cpu_ms / st.frames as f64 } else { 0.0 },
                st.compose_ms / st.frames.max(1) as f64, st.draw_ms / st.frames.max(1) as f64,
                a.s.audio_source, a.s.audio_level, a.capture.as_ref().map(|c| c.source), m.apple_playing,
                m.apple_present, m.any_playing, m.title, f.bass, f.mid, f.high, f.loud, f.onset_count,
                st.lat_last * 1000.0, if st.lat_n > 0 { st.lat_sum / st.lat_n as f64 * 1000.0 } else { 0.0 },
                st.lat_n, a.disp, a.eng.cols, a.eng.rows, a.eng.cw, a.eng.ch, a.eng.scene.shape_name(),
                a.eng.theme.name, a.s.black_bg, a.s.theme_rotate, a.eng.theme.fading()
            );
            drop(m);
            let _ = std::fs::write(path, text);
        }
        ["seq", n, dir] => {
            a.seq = Some((dir.to_string(), n.parse().unwrap_or(30), 0));
            wake();
        }
        ["resetstats"] => {
            a.stats = Stats { compose_ms: 0.0, draw_ms: 0.0, frames: 0, drawn: 0, cpu_ms: 0.0, lat_sum: 0.0, lat_n: 0, lat_last: 0.0 };
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ input

fn to_grid(x: i32, y: i32) -> (f32, f32) {
    let a = app();
    ((x - a.mon_rect.left - a.eng.origin.0) as f32, (y - a.mon_rect.top - a.eng.origin.1) as f32)
}

fn in_monitor(x: i32, y: i32) -> bool {
    let r = app().mon_rect;
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn handle_input(msg: u32, wp: WPARAM, lp: LPARAM) {
    let a = app();
    if !a.active {
        return;
    }
    let (x, y) = input::unpack(wp.0 as u64);
    let t = lp.0 as u32;
    match msg {
        input::WM_IN_MDOWN => {
            let (lx, ly, lt) = a.last_mdown;
            let dbl = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
            a.last_mdown = (x, y, t);
            if t.wrapping_sub(lt) < dbl && (x - lx).abs() < 8 && (y - ly).abs() < 8 {
                a.suppress_drag = true;
                a.last_mdown = (0, 0, 0);
                reroll(None);
            } else {
                a.suppress_drag = false;
                let (gx, gy) = to_grid(x, y);
                let shift = unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(
                        windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT.0 as i32) as u16 & 0x8000 != 0
                };
                a.eng.scene.drag_start(gx, gy, t as f64 / 1000.0, shift);
            }
        }
        input::WM_IN_MOVE => {
            let (x, y) = input::unpack(input::LAST_MOVE.load(std::sync::atomic::Ordering::Relaxed));
            let t = input::LAST_MOVE_T.load(std::sync::atomic::Ordering::Relaxed);
            input::move_consumed();
            if !a.suppress_drag {
                let (gx, gy) = to_grid(x, y);
                let fx = params().effects;
                a.eng.scene.drag_move(gx, gy, t as f64 / 1000.0, fx);
            }
        }
        input::WM_IN_MUP => {
            a.eng.scene.drag_end();
        }
        input::WM_IN_LDOWN => {
            a.ldown = Some((x, y, t));
            if params().effects > 0.0 {
                // a click-hold (still for 350 ms) opens the exploded view
                unsafe {
                    SetTimer(Some(a.ctl), T_HOLD, 350, None);
                }
            }
        }
        input::WM_IN_LUP => {
            let was_exploding = a.exploding;
            if a.exploding {
                a.exploding = false;
                a.eng.scene.fx.explode_target = 0.0;
            }
            if let Some((dx, dy, dt)) = a.ldown.take() {
                if (x - dx).abs() <= 6 && (y - dy).abs() <= 6 && t.wrapping_sub(dt) < 600 && in_monitor(x, y)
                    && params().effects > 0.0 && !was_exploding
                {
                    // was it on an icon? ask the list view a moment later, from this thread, never from the hook
                    a.click = Some((x, y));
                    unsafe {
                        SetTimer(Some(a.ctl), T_CLICK, 40, None);
                    }
                }
            }
        }
        input::WM_IN_WHEEL => {
            a.eng.scene.wheel(lp.0 as i16 as f32 / 120.0);
        }
        _ => {}
    }
    wake();
}

fn click_check() {
    let a = app();
    let Some((x, y)) = a.click.take() else { return };
    let mut on_icon = false;
    if let Some(h) = a.host {
        if !h.listview.is_invalid() {
            unsafe {
                let mut sel = 0usize;
                let mut hot = 0usize;
                let r1 = SendMessageTimeoutW(h.listview, 0x1032 /* LVM_GETSELECTEDCOUNT */, WPARAM(0), LPARAM(0),
                                             SMTO_ABORTIFHUNG, 50, Some(&mut sel));
                let r2 = SendMessageTimeoutW(h.listview, 0x103D /* LVM_GETHOTITEM */, WPARAM(0), LPARAM(0),
                                             SMTO_ABORTIFHUNG, 50, Some(&mut hot));
                if r1.0 != 0 && sel as isize > 0 {
                    on_icon = true;
                }
                if r2.0 != 0 && (hot as isize) >= 0 && hot as i32 != -1 {
                    on_icon = true;
                }
            }
        }
    }
    if !on_icon {
        let (gx, gy) = to_grid(x, y);
        let fx = params().effects;
        a.eng.scene.ripple(gx, gy, 1.8 * fx);
        wake();
    }
}

// ------------------------------------------------------------------ window procs

unsafe extern "system" fn win_event(_h: HWINEVENTHOOK, _e: u32, _hwnd: HWND, _o: i32, _c: i32, _t: u32, _ms: u32) {
    unsafe {
        if !APP.is_null() {
            SetTimer(Some(app().ctl), T_WATCH_SOON, 150, None);
        }
    }
}

unsafe extern "system" fn ctl_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if APP.is_null() {
            return DefWindowProcW(h, m, wp, lp);
        }
        let a = app();
        if m == a.taskbar_msg && m != 0 {
            logf!("TaskbarCreated: re-adding tray icon and re-attaching");
            if let Some(t) = &a.tray {
                t.add();
            }
            reattach();
            update_tip();
            return LRESULT(0);
        }
        match m {
            WM_TIMER => {
                match wp.0 {
                    T_WATCH => watch_tick(),
                    T_WATCH_SOON => {
                        let _ = KillTimer(Some(h), T_WATCH_SOON);
                        watch_tick();
                    }
                    T_GPU_RETRY => {
                        let _ = KillTimer(Some(h), T_GPU_RETRY);
                        if a.gpu.is_none() {
                            if create_gpu() {
                                logf!("gpu back");
                                a.gpu_retry_ms = 2000;
                                bind_gpu();
                            } else {
                                gpu_unavailable();
                            }
                        }
                        update_active();
                    }
                    T_CLICK => {
                        let _ = KillTimer(Some(h), T_CLICK);
                        click_check();
                    }
                    T_HOLD => {
                        let _ = KillTimer(Some(h), T_HOLD);
                        if let Some((dx, dy, _)) = a.ldown {
                            let mut pt = POINT::default();
                            let _ = GetCursorPos(&mut pt);
                            if (pt.x - dx).abs() <= 6 && (pt.y - dy).abs() <= 6 && in_monitor(dx, dy) {
                                a.exploding = true;
                                a.eng.scene.fx.explode_target = 1.0;
                                wake();
                            }
                        }
                    }
                    T_REATTACH => {
                        let _ = KillTimer(Some(h), T_REATTACH);
                        reattach();
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            input::WM_IN_MDOWN..=input::WM_IN_WHEEL => {
                handle_input(m, wp, lp);
                LRESULT(0)
            }
            audio::WM_AUDIO_STATE => {
                update_capture();
                update_tip();
                LRESULT(0)
            }
            tray::WM_TRAY => {
                let ev = (lp.0 & 0xffff) as u32;
                if ev == WM_CONTEXTMENU || ev == WM_LBUTTONUP || ev == WM_RBUTTONUP || ev == 0x400 {
                    let info = vec![
                        format!("{} : {}", a.disp.device_name,
                                if a.disp.hdr_active { "HDR on" } else { "SDR" }),
                        format!("SDR white {:.0} nits, peak {:.0} nits", a.disp.sdr_white_nits, a.disp.max_nits),
                        format!("swapchain {}", if a.gpu.as_ref().map(|g| g.hdr).unwrap_or(false) {
                            "FP16 scRGB"
                        } else {
                            "8-bit sRGB"
                        }),
                    ];
                    let extras = menu::Extras { startup_label: "Start with Windows", startup_on: startup_enabled(),
                                                display_info: &info, music_notes: &[] };
                    let items = menu::build(&a.s, &extras);
                    let cmd = a.tray.as_ref().map(|t| t.menu(&items)).unwrap_or(0);
                    if cmd != 0 {
                        command(cmd);
                    }
                }
                LRESULT(0)
            }
            WM_WTSSESSION_CHANGE => {
                match wp.0 as u32 {
                    WTS_SESSION_LOCK => a.locked = true,
                    WTS_SESSION_UNLOCK => a.locked = false,
                    _ => {}
                }
                update_active();
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                if wp.0 as u32 == PBT_POWERSETTINGCHANGE {
                    let pbs = &*(lp.0 as *const POWERBROADCAST_SETTING);
                    if pbs.PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
                        // 0 off, 1 on, 2 dimmed
                        a.display_on = pbs.Data[0] != 0;
                        update_active();
                    }
                }
                LRESULT(1)
            }
            WM_DISPLAYCHANGE => {
                on_display_change();
                LRESULT(0)
            }
            WM_COPYDATA => {
                let cds = &*(lp.0 as *const COPYDATASTRUCT);
                let s = std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
                let text = String::from_utf16_lossy(s).trim_end_matches('\0').to_string();
                match cds.dwData {
                    COPY_DUMP => {
                        a.dump_path = Some(text);
                        wake();
                        if !a.active {
                            // still honour the dump while paused: draw one frame
                            frame();
                        }
                    }
                    COPY_CMD => text_command(&text),
                    _ => {}
                }
                LRESULT(1)
            }
            WM_ENDSESSION => {
                if wp.0 != 0 {
                    shutdown();
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                a.quit = true;
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, m, wp, lp),
        }
    }
}

unsafe extern "system" fn view_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match m {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = Default::default();
                windows::Win32::Graphics::Gdi::BeginPaint(h, &mut ps);
                let _ = windows::Win32::Graphics::Gdi::EndPaint(h, &ps);
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(h, m, wp, lp),
        }
    }
}
