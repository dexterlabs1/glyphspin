//! The macOS backend: a borderless window per screen at the desktop level (screen.rs) drawn with Metal (metal.rs),
//! a menu-bar extra (menubar.rs), observe-only gestures (input.rs), visibility from occlusion notifications plus
//! window-list sampling (watch.rs), Core Audio process taps (audio.rs) and a Unix-socket command channel (ipc.rs).
//! Every screen runs the shared glyphspin_core::engine::Engine; this file feeds it input, timing and visibility.
//!
//! Full pause (no frames, no event monitor, no audio capture) when a screen's desktop is covered or a fullscreen app
//! is up (per screen), and for all screens while the screens sleep, the session is locked or switched away, the
//! screen saver runs, or Low Power Mode is on.

mod audio;
mod glyphs;
mod input;
mod ipc;
mod menubar;
mod metal;
mod screen;
mod watch;

use super::{Msg, Platform, Surface};
use block2::RcBlock;
use glyphspin_core::engine::{self, Engine};
use glyphspin_core::settings::Settings;
use glyphspin_core::{dump, logf, menu, scene, shapes, util};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDidChangeScreenParametersNotification,
                    NSEvent, NSMenu, NSScreen, NSWindowDidChangeOcclusionStateNotification, NSWindowOcclusionState,
                    NSWorkspace, NSWorkspaceScreensDidSleepNotification, NSWorkspaceScreensDidWakeNotification,
                    NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceSessionDidResignActiveNotification};
use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSNotificationName,
                       NSObjectProtocol, NSOperationQueue, NSPoint, NSProcessInfo,
                       NSProcessInfoPowerStateDidChangeNotification, NSRunLoop, NSRunLoopCommonModes, NSString,
                       NSTimer};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The highlight cap on macOS is relative to SDR white: `peak_nits` / this (600 / 240 = 2.5x by default, the same
/// ratio the Windows build uses at a 240-nit SDR white), never above the screen's current EDR headroom.
const SDR_REF_NITS: f32 = 240.0;

pub struct MacOs;

impl Platform for MacOs {
    fn app_dir() -> std::path::PathBuf {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = home.join("Library").join("Application Support").join("Glyphspin");
        if !d.exists() {
            let _ = std::fs::create_dir_all(&d);
        }
        d
    }

    fn build_atlas(cw_target: u32) -> glyphspin_core::atlas::Atlas {
        glyphs::build(cw_target)
    }

    fn send(kind: Msg, text: &str) -> bool {
        ipc::send(kind, text)
    }

    fn quit_running() -> i32 {
        ipc::quit()
    }

    fn test_cli(_args: &[String]) -> Option<i32> {
        None
    }

    fn run(args: &[String]) {
        if ipc::running() {
            // already running: a second launch rerolls the shape
            crate::cli_send("reroll");
            return;
        }
        util::log_init();
        std::panic::set_hook(Box::new(|info| {
            util::log(&format!("panic: {info}"));
            ipc::remove();
        }));
        run(args);
    }
}

// ------------------------------------------------------------------ state

#[derive(Default)]
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

/// A rotate (middle-drag or Option-drag) in progress, a left press waiting to become a click or a hold.
#[derive(Default)]
struct Pointer {
    drag: Option<usize>,
    ldown: Option<(usize, (f32, f32), f64)>,
    exploding: bool,
    hold: Option<Retained<NSTimer>>,
    /// over-desktop answer cached for a scroll gesture: (when, x, y, answer)
    desk_cache: Option<(Instant, f64, f64, bool)>,
}

struct TestFlags {
    sim_edr: Option<f32>,
    audio_ungated: bool,
    sim_idle: Option<f32>,
}

struct App {
    mtm: MainThreadMarker,
    s: Settings,
    gpu: Option<Rc<metal::Gpu>>,
    screens: Vec<screen::Screen>,
    seed: u64,
    shape: Option<&'static str>,
    menubar: Option<menubar::MenuBar>,
    // pause signals shared by all screens
    locked: bool,
    screens_asleep: bool,
    session_active: bool,
    low_power: bool,
    no_pause: bool,
    any_active: bool,
    capture: Option<audio::Capture>,
    capture_failed: Option<(audio::Source, Instant)>,
    monitor: Option<input::Monitor>,
    timer: Option<Retained<NSTimer>>,
    watch_timer: Option<Retained<NSTimer>>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    dump_path: Option<String>,
    stats: Stats,
    last_onset_seen: f64,
    ptr: Pointer,
    test: TestFlags,
    own_pid: i32,
}

static mut APP: *mut App = std::ptr::null_mut();

fn app() -> &'static mut App {
    unsafe { &mut *APP }
}

fn alive() -> bool {
    unsafe { !APP.is_null() }
}

// ------------------------------------------------------------------ setup

fn run(args: &[String]) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let nsapp = NSApplication::sharedApplication(mtm);
    nsapp.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let s = Settings::load();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let seed = arg("--seed").and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
    });
    let shape_name = arg("--shape").or_else(|| (s.shape != "random").then(|| s.shape.clone()));
    let shape = shape_name.and_then(|n| shapes::SHAPES.iter().copied().find(|x| *x == n));
    let gpu = match metal::Gpu::new() {
        Ok(g) => {
            logf!("metal device {}", g.name);
            Some(g)
        }
        Err(e) => {
            logf!("metal unavailable: {e}");
            None
        }
    };
    let mut a = App {
        mtm,
        s,
        gpu,
        screens: Vec::new(),
        seed,
        shape,
        menubar: None,
        locked: false,
        screens_asleep: false,
        session_active: true,
        low_power: NSProcessInfo::processInfo().isLowPowerModeEnabled(),
        no_pause: args.iter().any(|a| a == "--no-pause"),
        any_active: false,
        capture: None,
        capture_failed: None,
        monitor: None,
        timer: None,
        watch_timer: None,
        observers: Vec::new(),
        dump_path: None,
        stats: Stats::default(),
        last_onset_seen: 0.0,
        ptr: Pointer::default(),
        test: TestFlags {
            sim_edr: arg("--sim-hdr").and_then(|v| v.parse().ok()),
            audio_ungated: args.iter().any(|a| a == "--audio-ungated"),
            sim_idle: arg("--sim-idle").and_then(|v| v.parse().ok()),
        },
        own_pid: std::process::id() as i32,
    };
    unsafe { APP = &mut a };
    logf!("macOS {}, taps {}, low power {}", NSProcessInfo::processInfo().operatingSystemVersionString(),
          audio::taps_supported(), a.low_power);
    a.menubar = Some(menubar::MenuBar::new(mtm));
    build_screens();
    observe();
    ipc::serve(deliver);
    let block = RcBlock::new(|_t: NonNull<NSTimer>| watch_tick());
    a.watch_timer = Some(add_timer(1.0, true, &block));
    audio::poll();
    watch_tick();
    update_tip();
    nsapp.run();
}

fn add_timer(secs: f64, repeats: bool, block: &block2::DynBlock<dyn Fn(NonNull<NSTimer>)>) -> Retained<NSTimer> {
    unsafe {
        let t = NSTimer::timerWithTimeInterval_repeats_block(secs, repeats, block);
        t.setTolerance(if repeats { 0.1 } else { 0.001 });
        // common modes: keep drawing while the menu is open
        NSRunLoop::currentRunLoop().addTimer_forMode(&t, NSRunLoopCommonModes);
        t
    }
}

/// (Re)create the per-screen windows. Kept when the screen set hasn't changed.
fn build_screens() {
    let a = app();
    let Some(gpu) = a.gpu.clone() else { return };
    let list = NSScreen::screens(a.mtm);
    let same = list.len() == a.screens.len()
        && list.iter().zip(&a.screens).all(|(n, s)| n.frame() == s.frame && n.backingScaleFactor() == s.scale);
    if same && !a.screens.is_empty() {
        return;
    }
    a.screens.clear();
    a.ptr = Pointer::default();
    for (i, ns) in list.iter().enumerate() {
        let seed = a.seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut sc = screen::Screen::new(a.mtm, &ns, gpu.clone(), seed, a.shape);
        if let Some(h) = a.test.sim_edr {
            sc.edr_max = h;
            sc.edr_now = h;
        }
        let hdr = want_hdr(&a.s, &sc);
        sc.configure(&a.s, hdr);
        if let Some(lock) = a.shape.filter(|_| a.s.shape != "random") {
            sc.eng.scene.shape_lock = Some(lock);
        }
        a.screens.push(sc);
    }
    update_active();
}

fn want_hdr(s: &Settings, sc: &screen::Screen) -> bool {
    match s.hdr.as_str() {
        "on" => true,
        "off" => false,
        _ => sc.edr_max > 1.0,
    }
}

fn observe() {
    let a = app();
    let queue = NSOperationQueue::mainQueue();
    let mut add = |center: &NSNotificationCenter, name: &NSNotificationName, f: fn(&NSNotification)| {
        let block = RcBlock::new(move |n: NonNull<NSNotification>| {
            if alive() {
                f(unsafe { n.as_ref() });
            }
        });
        let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, Some(&queue), &block) };
        a.observers.push(token);
    };
    let ws = NSWorkspace::sharedWorkspace().notificationCenter();
    unsafe {
        add(&ws, NSWorkspaceScreensDidSleepNotification, |_| set_flag(|a| a.screens_asleep = true));
        add(&ws, NSWorkspaceScreensDidWakeNotification, |_| set_flag(|a| a.screens_asleep = false));
        add(&ws, NSWorkspaceSessionDidResignActiveNotification, |_| set_flag(|a| a.session_active = false));
        add(&ws, NSWorkspaceSessionDidBecomeActiveNotification, |_| set_flag(|a| a.session_active = true));
    }
    let dc = NSDistributedNotificationCenter::defaultCenter();
    let n = |s: &str| NSString::from_str(s);
    add(&dc, &n("com.apple.screenIsLocked"), |_| set_flag(|a| a.locked = true));
    add(&dc, &n("com.apple.screenIsUnlocked"), |_| set_flag(|a| a.locked = false));
    add(&dc, &n("com.apple.screensaver.didstart"), |_| set_flag(|a| a.locked = true));
    add(&dc, &n("com.apple.screensaver.didstop"), |_| set_flag(|a| a.locked = false));
    add(&dc, &n("com.apple.Music.playerInfo"), |n| {
        audio::music_notification(n.userInfo().as_deref());
        update_capture();
        update_tip();
    });
    let nc = NSNotificationCenter::defaultCenter();
    unsafe {
        add(&nc, NSProcessInfoPowerStateDidChangeNotification, |_| {
            set_flag(|a| a.low_power = NSProcessInfo::processInfo().isLowPowerModeEnabled())
        });
        add(&nc, NSApplicationDidChangeScreenParametersNotification, |_| {
            logf!("screen parameters changed");
            build_screens();
            refresh_edr();
            update_tip();
        });
        add(&nc, NSWindowDidChangeOcclusionStateNotification, |_| {
            for sc in app().screens.iter_mut() {
                sc.occluded = !sc.window.occlusionState().contains(NSWindowOcclusionState::Visible);
            }
            update_active();
        });
    }
}

fn set_flag(f: fn(&mut App)) {
    f(app());
    let a = app();
    logf!("locked={} screens_asleep={} session_active={} low_power={}", a.locked, a.screens_asleep, a.session_active,
          a.low_power);
    update_active();
}

/// IPC thread -> main thread.
fn deliver(kind: Msg, text: String, quit: bool) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        if !alive() {
            return;
        }
        if quit {
            shutdown();
        } else if kind == Msg::Dump {
            app().dump_path = Some(text);
            wake();
            if !app().screens.iter().any(|s| s.active) && !app().screens.is_empty() {
                // still honour the dump while paused: draw one frame
                frame_screen(0, Instant::now());
            }
        } else {
            text_command(&text);
        }
    });
}

fn shutdown() {
    logf!("shutting down");
    let a = app();
    a.capture = None;
    a.monitor = None;
    if let Some(t) = a.timer.take() {
        t.invalidate();
    }
    if let Some(t) = a.watch_timer.take() {
        t.invalidate();
    }
    a.screens.clear();
    a.menubar = None;
    ipc::remove();
    logf!("shutdown complete");
    std::process::exit(0);
}

// ------------------------------------------------------------------ pause logic

fn watch_tick() {
    if !alive() {
        return;
    }
    let a = app();
    let wins = watch::windows(false);
    let top = NSScreen::screens(a.mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(0.0);
    for sc in a.screens.iter_mut() {
        let c = watch::covered(sc.frame, top, a.own_pid, &wins);
        if c != sc.covered {
            logf!("'{}' covered={c}", sc.name);
        }
        sc.covered = c;
    }
    refresh_edr();
    let lp = NSProcessInfo::processInfo().isLowPowerModeEnabled();
    if lp != a.low_power {
        a.low_power = lp;
        logf!("low power {lp}");
    }
    if a.any_active && audio::poll() {
        update_tip();
    }
    update_active();
    // asleep at black: resume on input
    if idle_seconds() < 2.0 {
        let now = Instant::now();
        let mut woke = false;
        for sc in a.screens.iter_mut().filter(|s| s.active && s.next_due.is_none()) {
            sc.next_due = Some(now);
            woke = true;
        }
        if woke {
            schedule();
        }
    }
}

fn refresh_edr() {
    let a = app();
    if a.test.sim_edr.is_some() {
        return;
    }
    for (sc, ns) in a.screens.iter_mut().zip(NSScreen::screens(a.mtm).iter()) {
        sc.edr_now = ns.maximumExtendedDynamicRangeColorComponentValue() as f32;
        sc.edr_max = ns.maximumPotentialExtendedDynamicRangeColorComponentValue() as f32;
    }
}

fn update_active() {
    let a = app();
    let global = !a.s.paused && a.gpu.is_some()
        && (a.no_pause || (!a.locked && !a.screens_asleep && a.session_active && !a.low_power));
    let now = Instant::now();
    let mut any = false;
    for sc in a.screens.iter_mut() {
        let visible = a.no_pause || (!(sc.shown && sc.occluded) && !sc.covered);
        let active = global && visible;
        if active != sc.active {
            sc.active = active;
            logf!("'{}' active={active} (paused={} visible={visible} locked={} asleep={} session={} low_power={})",
                  sc.name, a.s.paused, a.locked, a.screens_asleep, a.session_active, a.low_power);
            if active {
                sc.last_frame = now;
                sc.next_due = Some(now);
            } else {
                sc.next_due = None;
                sc.eng.scene.drag_end();
            }
        }
        any |= active;
    }
    if any != a.any_active {
        a.any_active = any;
        a.monitor = if any { input::Monitor::install() } else { None };
        if !any {
            a.ptr = Pointer::default();
        }
    }
    update_capture();
    schedule();
}

fn update_capture() {
    let a = app();
    let (apple, any) = {
        let m = glyphspin_core::audio::MEDIA.lock().unwrap();
        (m.apple_playing, m.any_playing)
    };
    let want = if !a.any_active || a.s.audio_level == "off" || !audio::taps_supported() {
        None
    } else {
        match a.s.audio_source.as_str() {
            "apple" if apple || a.test.audio_ungated => Some(audio::Source::Apple),
            "all" if any || a.test.audio_ungated => Some(audio::Source::All),
            _ => None,
        }
    };
    let keep = match (&a.capture, want) {
        (Some(c), Some(w)) => c.source == w && c.running(),
        (None, None) => true,
        _ => false,
    };
    if keep {
        return;
    }
    a.capture = None;
    let Some(w) = want else { return };
    // a failed start (Music not running yet, no tap) is retried every 10 s, not every tick
    if let Some((src, at)) = a.capture_failed {
        if src == w && at.elapsed() < Duration::from_secs(10) {
            return;
        }
    }
    a.capture = audio::Capture::start(w);
    a.capture_failed = if a.capture.is_none() { Some((w, Instant::now())) } else { None };
    if a.capture.is_some() {
        wake();
    }
}

fn idle_seconds() -> f32 {
    if let Some(v) = app().test.sim_idle {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        return v + T0.get_or_init(Instant::now).elapsed().as_secs_f32();
    }
    CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, CGEventType(!0)) as f32
}

// ------------------------------------------------------------------ frames

/// Arm the frame timer for the earliest due screen.
fn schedule() {
    let a = app();
    if let Some(t) = a.timer.take() {
        t.invalidate();
    }
    let Some(due) = a.screens.iter().filter(|s| s.active).filter_map(|s| s.next_due).min() else { return };
    let secs = due.saturating_duration_since(Instant::now()).as_secs_f64().max(0.0005);
    let block = RcBlock::new(|_t: NonNull<NSTimer>| {
        if alive() {
            app().timer = None;
            frame_tick();
        }
    });
    a.timer = Some(add_timer(secs, false, &block));
}

fn frame_tick() {
    let now = Instant::now();
    for i in 0..app().screens.len() {
        let sc = &app().screens[i];
        if sc.active && sc.next_due.is_some_and(|d| d <= now) {
            frame_screen(i, now);
        }
    }
    schedule();
}

fn consts(i: usize) -> engine::Consts {
    let a = app();
    let sc = &mut a.screens[i];
    let hdr = sc.surface.hdr;
    let peak = if hdr { (a.s.peak_nits / SDR_REF_NITS).min(sc.edr_now.max(1.0)).max(1.0) } else { 1.0 };
    sc.eng.consts(&a.s, hdr, 1.0, peak)
}

fn frame_screen(i: usize, now: Instant) {
    let a = app();
    let p = engine::params(&a.s);
    let af = if a.capture.is_some() { *glyphspin_core::audio::FEATURES.lock().unwrap() } else {
        scene::AudioFrame::default()
    };
    let idle = idle_seconds();
    let want_dump = a.dump_path.is_some() && i == 0;
    let t0 = Instant::now();
    let sc = &mut a.screens[i];
    let dt = (now - sc.last_frame).as_secs_f32().clamp(0.0, 0.1);
    sc.last_frame = now;
    let rerolled = sc.eng.pre_step(&a.s, dt, idle);
    let engine::Step { busy, changed } = sc.eng.step(&p, dt, &af);
    a.stats.compose_ms += t0.elapsed().as_secs_f64() * 1000.0;
    a.stats.frames += 1;
    let t1 = Instant::now();
    if changed || want_dump {
        let c = consts(i);
        let sc = &mut a.screens[i];
        let (cols, rows) = (sc.eng.cols as u32, sc.eng.rows as u32);
        let r = sc.surface.upload_cells(&sc.eng.scene.cells, cols, rows).and_then(|_| sc.surface.draw(&c, want_dump));
        match r {
            Ok(Some((px, _))) => {
                if let Some(path) = a.dump_path.take() {
                    let (w, h, hdr) = (sc.surface.width as usize, sc.surface.height as usize, sc.surface.hdr);
                    write_dump(&path, &px, w, h, hdr, c.peak);
                }
            }
            Ok(None) => {}
            Err(e) => logf!("draw failed: {e}"),
        }
        a.stats.drawn += 1;
        let sc = &mut a.screens[i];
        if !sc.shown && sc.surface.ready() {
            sc.window.orderFrontRegardless();
            sc.shown = true;
            logf!("'{}' shown", sc.name);
        }
    }
    a.stats.draw_ms += t1.elapsed().as_secs_f64() * 1000.0;
    let onset = a.screens[i].eng.scene.last_onset_capture_s;
    if onset != a.last_onset_seen {
        a.last_onset_seen = onset;
        let lat = audio::now_seconds() - onset;
        if lat > 0.0 && lat < 2.0 {
            a.stats.lat_sum += lat;
            a.stats.lat_n += 1;
            a.stats.lat_last = lat;
        }
    }
    a.stats.cpu_ms += t0.elapsed().as_secs_f64() * 1000.0;
    if rerolled {
        update_tip();
    }
    let sc = &mut a.screens[i];
    // faded to black: stop drawing until there is input again (the 1 Hz watch tick wakes it)
    if sc.eng.asleep() && !want_dump {
        sc.next_due = None;
        return;
    }
    sc.next_due = Some(now + Duration::from_secs_f32(Engine::interval(&a.s, busy, want_dump)));
}

fn write_dump(path: &str, px: &[u8], w: usize, h: usize, hdr: bool, peak: f32) {
    let p = std::path::Path::new(path);
    // EDR values are multiples of SDR white, so the stats are too
    let st = dump::write(p, px, w, h, hdr, 1.0, 1.0);
    let a = app();
    let sc = &a.screens[0];
    let info = format!(
        "size {w}x{h} | layer {} | screen '{}' scale {} EDR headroom now {:.2} max {:.2} | peak {:.2}x SDR white | \
         frame max {:.3}x SDR white at {:?}, avg {:.4}, {} px above SDR white | shape {} grid {}x{} cell {}x{} \
         obj cells {} | gpu {}",
        if hdr { "RGBA16Float extended linear sRGB (EDR)" } else { "BGRA8Unorm sRGB" },
        sc.name, sc.scale, sc.edr_now, sc.edr_max, peak, st.max, st.max_px, st.avg, st.over_white,
        sc.eng.scene.shape_name(), sc.eng.cols, sc.eng.rows, sc.eng.cw, sc.eng.ch, sc.eng.scene.object_cells(),
        a.gpu.as_ref().map(|g| g.name.clone()).unwrap_or_default()
    );
    let _ = std::fs::write(p.with_extension("txt"), info);
}

/// Draw the next frame now on every active screen (input, commands).
fn wake() {
    let now = Instant::now();
    for sc in app().screens.iter_mut().filter(|s| s.active) {
        sc.next_due = Some(now);
    }
    schedule();
}

fn update_tip() {
    let a = app();
    let names: Vec<String> = a.screens.iter().map(|s| s.eng.scene.shape_name().replace('_', " ")).collect();
    let mut tip = format!("Glyphspin - {}", names.join(", "));
    {
        let m = glyphspin_core::audio::MEDIA.lock().unwrap();
        if m.apple_playing && !m.title.is_empty() {
            tip = format!("{tip}\n{}", m.title);
        }
    }
    if a.s.paused {
        tip.push_str(" (paused)");
    }
    if let Some(mb) = &a.menubar {
        mb.set_tip(&tip);
    }
}

// ------------------------------------------------------------------ menu + commands

fn login_enabled() -> bool {
    unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
}

/// Only ever called from the menu toggle.
fn set_login(on: bool) {
    let svc = unsafe { SMAppService::mainAppService() };
    let r = unsafe { if on { svc.registerAndReturnError() } else { svc.unregisterAndReturnError() } };
    if let Err(e) = r {
        logf!("login item {}: {}", if on { "register" } else { "unregister" }, e.localizedDescription());
    }
}

/// The menu is about to open: rebuild it from the current state.
pub(super) fn menu_opening(menu: &NSMenu) {
    if !alive() {
        return;
    }
    let a = app();
    let mut info = Vec::new();
    for sc in &a.screens {
        let edr = if sc.edr_max > 1.0 {
            format!("EDR, headroom {:.1}x now, up to {:.1}x", sc.edr_now, sc.edr_max)
        } else {
            "SDR".to_string()
        };
        info.push(format!("{}: {edr}", sc.name));
        info.push(format!("  {}", if sc.surface.hdr { "16-bit float, extended linear sRGB" } else { "8-bit sRGB" }));
    }
    let mut notes = Vec::new();
    if !audio::taps_supported() {
        notes.push("Music needs macOS 14.2 or later".to_string());
    } else if a.capture.as_ref().is_some_and(|c| c.silent()) {
        notes.push("No sound reaching Glyphspin: allow it in".to_string());
        notes.push("System Settings > Privacy & Security >".to_string());
        notes.push("Screen & System Audio Recording".to_string());
    }
    let extras = menu::Extras { startup_label: "Open at Login", startup_on: login_enabled(), display_info: &info,
                                music_notes: &notes };
    let items = menu::build(&a.s, &extras);
    if let Some(mb) = &a.menubar {
        mb.fill(menu, &items);
    }
}

fn reroll(name: Option<&'static str>) {
    let a = app();
    let fx = engine::params(&a.s).effects;
    for sc in a.screens.iter_mut() {
        sc.eng.scene.reroll(name, fx, None);
    }
    update_tip();
    wake();
}

fn apply_setting(key: &str, val: &str) {
    let a = app();
    let old = a.s.clone();
    if !a.s.set(key, val) || a.s == old {
        return;
    }
    a.s.save();
    if a.s.shape != old.shape {
        let lock = shapes::SHAPES.iter().copied().find(|x| *x == a.s.shape);
        for sc in a.screens.iter_mut() {
            sc.eng.scene.shape_lock = lock;
        }
        if let Some(n) = lock {
            reroll(Some(n));
        }
    }
    if a.s.glyph_size != old.glyph_size {
        for sc in a.screens.iter_mut() {
            sc.rebuild_grid(&a.s);
        }
    }
    if a.s.hdr != old.hdr {
        for sc in a.screens.iter_mut() {
            let hdr = want_hdr(&a.s, sc);
            sc.surface.resize(sc.w, sc.h, sc.scale, hdr);
            sc.eng.last_drawn = ((i32::MIN, 0), 0);
        }
    }
    update_active();
    update_tip();
    wake();
}

pub(super) fn command(cmd: u32) {
    if !alive() {
        return;
    }
    let a = app();
    match menu::action(cmd, &a.s) {
        menu::Action::Reroll => {
            let lock = a.screens.first().and_then(|s| s.eng.scene.shape_lock);
            reroll(lock);
        }
        menu::Action::Set(k, v) => apply_setting(k, &v),
        menu::Action::ToggleStartup => {
            let on = !login_enabled();
            set_login(on);
            a.s.start_with_windows = login_enabled();
            a.s.save();
        }
        menu::Action::Quit => shutdown(),
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
        // a ripple goes to the main screen (pixels from its top-left); effects go to every screen
        ["ripple", ..] => {
            if let Some(sc) = a.screens.first_mut() {
                sc.eng.fx_command(&a.s, parts.as_slice());
            }
            wake();
        }
        ["fx", ..] => {
            for sc in a.screens.iter_mut() {
                sc.eng.fx_command(&a.s, parts.as_slice());
            }
            wake();
        }
        ["set", kv] => {
            if let Some((k, v)) = kv.split_once('=') {
                apply_setting(k, v);
            }
        }
        ["stats", path] => {
            let st = &a.stats;
            let m = glyphspin_core::audio::MEDIA.lock().unwrap();
            let f = *glyphspin_core::audio::FEATURES.lock().unwrap();
            let screens: Vec<String> = a.screens.iter().map(|s| format!(
                "'{}' {}x{} @{}x active {} shown {} occluded {} covered {} edr {:.2}/{:.2} hdr {} grid {}x{} cell {}x{} \
                 shape {}", s.name, s.w, s.h, s.scale, s.active, s.shown, s.occluded, s.covered, s.edr_now, s.edr_max,
                s.surface.hdr, s.eng.cols, s.eng.rows, s.eng.cw, s.eng.ch, s.eng.scene.shape_name())).collect();
            let text = format!(
                "paused {} locked {} screens_asleep {} session_active {} low_power {} monitor {}\n\
                 frames {} drawn {} avg_cpu_ms {:.3} (step+compose {:.3}, upload+draw+present {:.3})\n\
                 audio: source {} level {} capture {:?} taps {} apple_playing {} apple_present {} any_playing {} \
                 title '{}'\n\
                 features bass {:.2} mid {:.2} high {:.2} loud {:.2} onsets {}\n\
                 onset->frame latency: last {:.1} ms avg {:.1} ms over {}\n\
                 idle {:.1} s | gpu {}\n{}",
                a.s.paused, a.locked, a.screens_asleep, a.session_active, a.low_power, a.monitor.is_some(),
                st.frames, st.drawn, if st.frames > 0 { st.cpu_ms / st.frames as f64 } else { 0.0 },
                st.compose_ms / st.frames.max(1) as f64, st.draw_ms / st.frames.max(1) as f64,
                a.s.audio_source, a.s.audio_level, a.capture.as_ref().map(|c| c.source), audio::taps_supported(),
                m.apple_playing, m.apple_present, m.any_playing, m.title, f.bass, f.mid, f.high, f.loud,
                f.onset_count, st.lat_last * 1000.0,
                if st.lat_n > 0 { st.lat_sum / st.lat_n as f64 * 1000.0 } else { 0.0 }, st.lat_n, idle_seconds(),
                a.gpu.as_ref().map(|g| g.name.clone()).unwrap_or_default(), screens.join("\n")
            );
            drop(m);
            let _ = std::fs::write(path, text);
        }
        ["resetstats"] => a.stats = Stats::default(),
        _ => {}
    }
}

// ------------------------------------------------------------------ gestures

fn screen_at(p: NSPoint) -> Option<usize> {
    app().screens.iter().position(|s| s.contains(p) && s.active)
}

/// Is the pointer on the bare desktop? Cached briefly so a trackpad scroll doesn't query the window list for every
/// one of its events.
fn on_desktop(p: NSPoint, cache: bool) -> bool {
    let a = app();
    if cache {
        if let Some((t, x, y, v)) = a.ptr.desk_cache {
            if t.elapsed() < Duration::from_millis(300) && (x - p.x).abs() < 20.0 && (y - p.y).abs() < 20.0 {
                return v;
            }
        }
    }
    let top = NSScreen::screens(a.mtm).iter().next().map(|s| s.frame().size.height).unwrap_or(0.0);
    let v = watch::over_desktop(p.x, top - p.y, a.own_pid);
    a.ptr.desk_cache = Some((Instant::now(), p.x, p.y, v));
    v
}

pub(super) fn gesture(g: input::Gesture, p: NSPoint, t: f64) {
    if !alive() || !app().any_active {
        return;
    }
    let a = app();
    let effects = engine::params(&a.s).effects;
    match g {
        input::Gesture::RotateDown { shift, clicks } => {
            let Some(i) = screen_at(p) else { return };
            if !on_desktop(p, false) {
                return;
            }
            if clicks >= 2 {
                a.ptr.drag = None;
                reroll(None);
                return;
            }
            let (gx, gy) = a.screens[i].to_grid(p);
            a.screens[i].eng.scene.drag_start(gx, gy, t, shift);
            a.ptr.drag = Some(i);
        }
        input::Gesture::RotateMove => {
            if let Some(i) = a.ptr.drag {
                let (gx, gy) = a.screens[i].to_grid(p);
                a.screens[i].eng.scene.drag_move(gx, gy, t, effects);
            }
        }
        input::Gesture::RotateUp => {
            if let Some(i) = a.ptr.drag.take() {
                a.screens[i].eng.scene.drag_end();
            }
        }
        input::Gesture::LeftDown => {
            let Some(i) = screen_at(p) else { return };
            if !on_desktop(p, false) {
                return;
            }
            a.ptr.ldown = Some((i, a.screens[i].to_px(p), t));
            if effects > 0.0 {
                // a click-hold (still for 350 ms) opens the exploded view
                let block = RcBlock::new(|_t: NonNull<NSTimer>| hold_check());
                a.ptr.hold = Some(add_timer(0.35, false, &block));
            }
        }
        input::Gesture::LeftUp => {
            if let Some(i) = a.ptr.drag.take() {
                // the end of an Option-drag
                a.screens[i].eng.scene.drag_end();
            }
            if let Some(t) = a.ptr.hold.take() {
                t.invalidate();
            }
            let was_exploding = a.ptr.exploding;
            if let Some((i, (x0, y0), t0)) = a.ptr.ldown.take() {
                if was_exploding {
                    a.screens[i].eng.scene.fx.explode_target = 0.0;
                }
                let (x, y) = a.screens[i].to_px(p);
                if (x - x0).abs() <= 6.0 && (y - y0).abs() <= 6.0 && t - t0 < 0.6 && effects > 0.0 && !was_exploding {
                    let (gx, gy) = a.screens[i].to_grid(p);
                    a.screens[i].eng.scene.ripple(gx, gy, 1.8 * effects);
                }
            }
            a.ptr.exploding = false;
        }
        input::Gesture::Scroll(clicks) => {
            let Some(i) = screen_at(p) else { return };
            if clicks != 0.0 && on_desktop(p, true) {
                a.screens[i].eng.scene.wheel(clicks);
            }
        }
    }
    wake();
}

fn hold_check() {
    if !alive() {
        return;
    }
    let a = app();
    a.ptr.hold = None;
    let Some((i, (x0, y0), _)) = a.ptr.ldown else { return };
    let (x, y) = a.screens[i].to_px(NSEvent::mouseLocation());
    if (x - x0).abs() <= 6.0 && (y - y0).abs() <= 6.0 && NSEvent::pressedMouseButtons() & 1 != 0 {
        a.ptr.exploding = true;
        a.screens[i].eng.scene.fx.explode_target = 1.0;
        wake();
    }
}
