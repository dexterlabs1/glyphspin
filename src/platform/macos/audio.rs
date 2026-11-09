//! Music reactivity on macOS.
//!
//! - Capture: Core Audio process taps (macOS 14.2+). "Apple Music" taps only the Music app's processes (a stereo
//!   mixdown of them), "All audio" taps the system output. The tap feeds a private aggregate device whose IO proc
//!   copies mono samples into a buffer; an analysis thread drains it every 10 ms into the shared analyser
//!   (glyphspin_core::audio). Taps need the system audio recording permission (the Info.plist carries
//!   NSAudioCaptureUsageDescription); without it the tap delivers silence and the menu says so.
//!   AudioHardwareCreateProcessTap and CATapDescription are looked up at runtime, so the app still starts on
//!   macOS 13 (music is then off, with a menu note).
//! - Now playing: MediaRemote is closed to third-party apps since macOS 15.4, so it isn't used. Instead:
//!   Music.app's own `com.apple.Music.playerInfo` distributed notification (state + title, no permission), and each
//!   audio process's kAudioProcessPropertyIsRunningOutput (is it sending audio to an output right now?), polled once
//!   a second while visible. "Apple Music" captures while Music is playing or outputting; "All audio" while any
//!   process outputs.

use glyphspin_core::audio::{Analyzer, FEATURES, MEDIA};
use glyphspin_core::logf;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

type ObjId = u32;

#[repr(C)]
struct Addr {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
struct SmpteTime {
    subframes: i16,
    subframe_divisor: i16,
    counter: u32,
    kind: u32,
    flags: u32,
    hours: i16,
    minutes: i16,
    seconds: i16,
    frames: i16,
}

#[repr(C)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte: SmpteTime,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Asbd {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

type IoProc = unsafe extern "C" fn(ObjId, *const AudioTimeStamp, *const AudioBufferList, *const AudioTimeStamp,
                                   *mut AudioBufferList, *const AudioTimeStamp, *mut c_void) -> i32;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(id: ObjId, a: *const Addr, qs: u32, qd: *const c_void, size: *mut u32) -> i32;
    fn AudioObjectGetPropertyData(id: ObjId, a: *const Addr, qs: u32, qd: *const c_void, size: *mut u32,
                                  out: *mut c_void) -> i32;
    fn AudioHardwareCreateAggregateDevice(desc: *const c_void, out: *mut ObjId) -> i32;
    fn AudioHardwareDestroyAggregateDevice(id: ObjId) -> i32;
    fn AudioDeviceCreateIOProcID(dev: ObjId, p: IoProc, client: *mut c_void, out: *mut *mut c_void) -> i32;
    fn AudioDeviceDestroyIOProcID(dev: ObjId, id: *mut c_void) -> i32;
    fn AudioDeviceStart(dev: ObjId, id: *mut c_void) -> i32;
    fn AudioDeviceStop(dev: ObjId, id: *mut c_void) -> i32;
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut [u32; 2]) -> i32;
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

const SYSTEM: ObjId = 1;
const GLOBAL: u32 = fourcc(b"glob");
const PROCESS_LIST: u32 = fourcc(b"prs#");
const PROCESS_PID: u32 = fourcc(b"ppid");
const PROCESS_BUNDLE: u32 = fourcc(b"pbid");
const PROCESS_RUNNING_OUTPUT: u32 = fourcc(b"piro");
const DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const DEVICE_UID: u32 = fourcc(b"uid ");
const TAP_FORMAT: u32 = fourcc(b"tfmt");
const MUSIC_BUNDLE: &str = "com.apple.Music";

fn addr(selector: u32) -> Addr {
    Addr { selector, scope: GLOBAL, element: 0 }
}

unsafe fn get<T: Copy + Default>(id: ObjId, sel: u32) -> Option<T> {
    let mut v = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let st = unsafe { AudioObjectGetPropertyData(id, &addr(sel), 0, std::ptr::null(), &mut size, &mut v as *mut T as *mut _) };
    (st == 0).then_some(v)
}

unsafe fn get_cfstring(id: ObjId, sel: u32) -> Option<String> {
    let mut p: *mut NSString = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut NSString>() as u32;
    let st = unsafe { AudioObjectGetPropertyData(id, &addr(sel), 0, std::ptr::null(), &mut size, &mut p as *mut _ as *mut _) };
    if st != 0 || p.is_null() {
        return None;
    }
    // SAFETY: the property returns a +1 CFStringRef, toll-free bridged to NSString
    let s = unsafe { Retained::from_raw(p) }?;
    Some(s.to_string())
}

/// Seconds on the host clock (mach_absolute_time), the clock the taps timestamp their buffers with.
pub fn now_seconds() -> f64 {
    host_to_seconds(unsafe { mach_absolute_time() })
}

fn host_to_seconds(t: u64) -> f64 {
    static TB: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    let k = *TB.get_or_init(|| {
        let mut i = [0u32; 2];
        unsafe { mach_timebase_info(&mut i) };
        if i[1] == 0 { 1e-9 } else { i[0] as f64 / i[1] as f64 * 1e-9 }
    });
    t as f64 * k
}

type CreateTap = unsafe extern "C" fn(*mut AnyObject, *mut ObjId) -> i32;
type DestroyTap = unsafe extern "C" fn(ObjId) -> i32;

fn tap_fns() -> Option<(CreateTap, DestroyTap)> {
    unsafe {
        let rtld_default = -2isize as *mut c_void;
        let c = dlsym(rtld_default, c"AudioHardwareCreateProcessTap".as_ptr());
        let d = dlsym(rtld_default, c"AudioHardwareDestroyProcessTap".as_ptr());
        if c.is_null() || d.is_null() || AnyClass::get(c"CATapDescription").is_none() {
            return None;
        }
        Some((std::mem::transmute::<*mut c_void, CreateTap>(c), std::mem::transmute::<*mut c_void, DestroyTap>(d)))
    }
}

/// Process taps exist on this macOS (14.2+).
pub fn taps_supported() -> bool {
    tap_fns().is_some()
}

struct Proc {
    id: ObjId,
    pid: i32,
    bundle: String,
    output: bool,
}

fn processes() -> Vec<Proc> {
    unsafe {
        let mut size = 0u32;
        if AudioObjectGetPropertyDataSize(SYSTEM, &addr(PROCESS_LIST), 0, std::ptr::null(), &mut size) != 0 {
            return Vec::new();
        }
        let mut ids = vec![0 as ObjId; size as usize / 4];
        if AudioObjectGetPropertyData(SYSTEM, &addr(PROCESS_LIST), 0, std::ptr::null(), &mut size,
                                      ids.as_mut_ptr() as *mut _) != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / 4);
        ids.into_iter().map(|id| Proc {
            id,
            pid: get::<i32>(id, PROCESS_PID).unwrap_or(0),
            bundle: get_cfstring(id, PROCESS_BUNDLE).unwrap_or_default(),
            output: get::<u32>(id, PROCESS_RUNNING_OUTPUT).unwrap_or(0) != 0,
        }).collect()
    }
}

fn is_music(bundle: &str) -> bool {
    bundle == MUSIC_BUNDLE || bundle.starts_with("com.apple.Music.")
}

/// Music.app's last reported player state (from its distributed notification).
static MUSIC_NOTIFIED_PLAYING: AtomicBool = AtomicBool::new(false);

/// Refresh the now-playing state from the audio process list. True if it changed.
pub fn poll() -> bool {
    let own = std::process::id() as i32;
    let procs = processes();
    let music_out = procs.iter().any(|p| is_music(&p.bundle) && p.output);
    let any_out = procs.iter().any(|p| p.output && p.pid != own);
    let music_present = procs.iter().any(|p| is_music(&p.bundle));
    let mut m = MEDIA.lock().unwrap();
    let apple = music_out || MUSIC_NOTIFIED_PLAYING.load(Ordering::Relaxed);
    let changed = m.apple_playing != apple || m.any_playing != any_out || m.apple_present != music_present;
    m.apple_playing = apple;
    m.any_playing = any_out;
    m.apple_present = music_present;
    if !apple {
        m.title.clear();
    }
    changed
}

/// Music.app's `com.apple.Music.playerInfo` userInfo: "Player State" (Playing / Paused / Stopped), "Name", "Artist".
pub fn music_notification(info: Option<&NSDictionary>) {
    let Some(info) = info else { return };
    // SAFETY: the playerInfo dictionary has string keys
    let info: &NSDictionary<NSString, AnyObject> = unsafe { &*(info as *const NSDictionary as *const _) };
    let s = |k: &str| info.objectForKey(&NSString::from_str(k)).and_then(|v| v.downcast::<NSString>().ok())
        .map(|v| v.to_string()).unwrap_or_default();
    let playing = s("Player State") == "Playing";
    MUSIC_NOTIFIED_PLAYING.store(playing, Ordering::Relaxed);
    let mut m = MEDIA.lock().unwrap();
    m.apple_present = true;
    m.apple_playing = playing;
    let (title, artist) = (s("Name"), s("Artist"));
    m.title = if !playing { String::new() } else if artist.is_empty() { title } else { format!("{title} - {artist}") };
}

// ------------------------------------------------------------------ capture

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Apple,
    All,
}

struct Shared {
    samples: Vec<f32>,
    t_end: f64,
    /// any non-zero sample since the capture started (silence for long = no permission)
    heard: bool,
}

struct IoCtx {
    shared: Mutex<Shared>,
    channels: u32,
    interleaved: bool,
    rate: f64,
}

unsafe extern "C" fn io_proc(_dev: ObjId, _now: *const AudioTimeStamp, input: *const AudioBufferList,
                             input_time: *const AudioTimeStamp, _out: *mut AudioBufferList,
                             _out_time: *const AudioTimeStamp, client: *mut c_void) -> i32 {
    unsafe {
        let ctx = &*(client as *const IoCtx);
        let Some(list) = input.as_ref() else { return 0 };
        let n = list.count as usize;
        if n == 0 {
            return 0;
        }
        let bufs = std::slice::from_raw_parts(list.buffers.as_ptr(), n);
        let Ok(mut sh) = ctx.shared.try_lock() else { return 0 }; // never block the IO thread
        let frames;
        if ctx.interleaved || n == 1 {
            let b = &bufs[0];
            let ch = b.channels.max(1) as usize;
            frames = b.bytes as usize / 4 / ch;
            let d = std::slice::from_raw_parts(b.data as *const f32, frames * ch);
            for f in 0..frames {
                let mut acc = 0.0;
                for c in 0..ch {
                    acc += d[f * ch + c];
                }
                sh.samples.push(acc / ch as f32);
            }
        } else {
            frames = bufs[0].bytes as usize / 4;
            for f in 0..frames {
                let mut acc = 0.0;
                for b in bufs {
                    acc += *(b.data as *const f32).add(f);
                }
                sh.samples.push(acc / n as f32);
            }
        }
        if !sh.heard && sh.samples.iter().rev().take(frames).any(|v| *v != 0.0) {
            sh.heard = true;
        }
        if let Some(ts) = input_time.as_ref() {
            sh.t_end = host_to_seconds(ts.host_time) + frames as f64 / ctx.rate;
        }
        // don't grow without bound if the analysis thread stalls
        if sh.samples.len() > 48000 {
            let cut = sh.samples.len() - 48000;
            sh.samples.drain(..cut);
        }
        let _ = ctx.channels;
        0
    }
}

pub struct Capture {
    pub source: Source,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: std::time::Instant,
    ctx: Arc<IoCtx>,
}

struct Tap {
    tap: ObjId,
    agg: ObjId,
    proc_id: *mut c_void,
    destroy: DestroyTap,
}

unsafe fn output_uid() -> Option<Retained<NSString>> {
    unsafe {
        let dev = get::<ObjId>(SYSTEM, DEFAULT_OUTPUT)?;
        get_cfstring(dev, DEVICE_UID).map(|s| NSString::from_str(&s))
    }
}

unsafe fn make_tap(source: Source) -> Result<(ObjId, Retained<NSString>, DestroyTap, Asbd), String> {
    unsafe {
        let (create, destroy) = tap_fns().ok_or("process taps need macOS 14.2")?;
        let cls = AnyClass::get(c"CATapDescription").ok_or("no CATapDescription")?;
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        let desc: *mut AnyObject = match source {
            Source::Apple => {
                let ids: Vec<Retained<NSNumber>> = processes().iter().filter(|p| is_music(&p.bundle))
                    .map(|p| NSNumber::new_u32(p.id)).collect();
                if ids.is_empty() {
                    return Err("Music isn't running".into());
                }
                let arr = NSArray::from_retained_slice(&ids);
                msg_send![alloc, initStereoMixdownOfProcesses: &*arr]
            }
            Source::All => {
                let arr = NSArray::<NSNumber>::from_retained_slice(&[]);
                msg_send![alloc, initStereoGlobalTapButExcludeProcesses: &*arr]
            }
        };
        if desc.is_null() {
            return Err("tap description".into());
        }
        let desc: Retained<AnyObject> = Retained::from_raw(desc).unwrap();
        let _: () = msg_send![&*desc, setPrivate: true];
        let _: () = msg_send![&*desc, setName: &*NSString::from_str("Glyphspin")];
        let uuid: Retained<NSUUID> = msg_send![&*desc, UUID];
        let mut tap: ObjId = 0;
        let st = create(Retained::as_ptr(&desc) as *mut _, &mut tap);
        if st != 0 || tap == 0 {
            return Err(format!("AudioHardwareCreateProcessTap: {st}"));
        }
        let fmt = get::<Asbd>(tap, TAP_FORMAT).unwrap_or(Asbd { sample_rate: 48000.0, channels_per_frame: 2,
                                                               ..Default::default() });
        Ok((tap, uuid.UUIDString(), destroy, fmt))
    }
}

impl Tap {
    unsafe fn new(source: Source, ctx: *const IoCtx, uid: Retained<NSString>, tap: ObjId, destroy: DestroyTap)
                  -> Result<Tap, String> {
        unsafe {
            let agg_uid = NSUUID::new().UUIDString();
            let k = |s: &str| NSString::from_str(s);
            let sub_tap = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                &[&*k("uid"), &*k("drift")],
                &[Retained::into_super(Retained::into_super(uid)), Retained::into_super(Retained::into_super(
                    Retained::into_super(NSNumber::new_bool(true))))],
            );
            let taps = NSArray::from_retained_slice(&[sub_tap]);
            let mut keys = vec![k("name"), k("uid"), k("private"), k("stacked"), k("tapautostart"), k("taps")];
            let mut vals: Vec<Retained<AnyObject>> = vec![
                Retained::into_super(Retained::into_super(k("Glyphspin tap"))),
                Retained::into_super(Retained::into_super(agg_uid)),
                Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_bool(true)))),
                Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_bool(false)))),
                Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_bool(true)))),
                Retained::into_super(Retained::into_super(taps)),
            ];
            if let Some(out) = output_uid() {
                // clock the aggregate from the output device the tapped audio plays on
                let sub = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                    &[&*k("uid")], &[Retained::into_super(Retained::into_super(out.clone()))]);
                keys.push(k("master"));
                vals.push(Retained::into_super(Retained::into_super(out)));
                keys.push(k("subdevices"));
                vals.push(Retained::into_super(Retained::into_super(NSArray::from_retained_slice(&[sub]))));
            }
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let desc = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &vals);
            let mut agg: ObjId = 0;
            let st = AudioHardwareCreateAggregateDevice(Retained::as_ptr(&desc) as *const c_void, &mut agg);
            if st != 0 {
                destroy(tap);
                return Err(format!("AudioHardwareCreateAggregateDevice: {st}"));
            }
            let mut proc_id: *mut c_void = std::ptr::null_mut();
            let st = AudioDeviceCreateIOProcID(agg, io_proc, ctx as *mut c_void, &mut proc_id);
            if st != 0 {
                AudioHardwareDestroyAggregateDevice(agg);
                destroy(tap);
                return Err(format!("AudioDeviceCreateIOProcID: {st}"));
            }
            let st = AudioDeviceStart(agg, proc_id);
            if st != 0 {
                AudioDeviceDestroyIOProcID(agg, proc_id);
                AudioHardwareDestroyAggregateDevice(agg);
                destroy(tap);
                return Err(format!("AudioDeviceStart: {st}"));
            }
            logf!("audio tap started: {source:?}");
            Ok(Tap { tap, agg, proc_id, destroy })
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.agg, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.agg);
            (self.destroy)(self.tap);
        }
    }
}

impl Capture {
    pub fn start(source: Source) -> Option<Capture> {
        let (tap, uid, destroy, fmt) = match unsafe { make_tap(source) } {
            Ok(t) => t,
            Err(e) => {
                logf!("audio: {e}");
                return None;
            }
        };
        let rate = if fmt.sample_rate > 0.0 { fmt.sample_rate } else { 48000.0 };
        let ctx = Arc::new(IoCtx {
            shared: Mutex::new(Shared { samples: Vec::with_capacity(8192), t_end: 0.0, heard: false }),
            channels: fmt.channels_per_frame.max(1),
            interleaved: fmt.format_flags & (1 << 5) == 0, // kAudioFormatFlagIsNonInterleaved
            rate,
        });
        let tap = match unsafe { Tap::new(source, Arc::as_ptr(&ctx), uid, tap, destroy) } {
            Ok(t) => t,
            Err(e) => {
                logf!("audio: {e}");
                return None;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, c2) = (stop.clone(), ctx.clone());
        // the tap lives on the analysis thread and is torn down when it ends
        struct SendTap(Tap);
        unsafe impl Send for SendTap {}
        let tap = SendTap(tap);
        let thread = std::thread::Builder::new().name("audio".into()).spawn(move || {
            let tap = tap;
            let mut an = Analyzer::new(rate as f32);
            let mut buf = Vec::with_capacity(8192);
            while !s2.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(10));
                let t_end = {
                    let mut sh = c2.shared.lock().unwrap();
                    std::mem::swap(&mut buf, &mut sh.samples);
                    sh.t_end
                };
                if !buf.is_empty() {
                    an.push(&buf, t_end);
                    buf.clear();
                }
            }
            drop(tap.0);
            let mut f = FEATURES.lock().unwrap();
            f.active = false;
            f.bass = 0.0;
            f.mid = 0.0;
            f.high = 0.0;
            f.loud = 0.0;
            f.spec = [0.0; 8];
        }).ok()?;
        Some(Capture { source, stop, thread: Some(thread), started: std::time::Instant::now(), ctx })
    }

    pub fn running(&self) -> bool {
        self.thread.as_ref().map(|t| !t.is_finished()).unwrap_or(false)
    }

    /// Five seconds of nothing but zeros while the source plays: the permission is probably missing.
    pub fn silent(&self) -> bool {
        self.started.elapsed().as_secs_f32() > 5.0 && !self.ctx.shared.lock().map(|s| s.heard).unwrap_or(true)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
