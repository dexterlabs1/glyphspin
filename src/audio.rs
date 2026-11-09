//! Music reactivity.
//!
//! - SMTC (Windows.Media.Control) tells us when Apple Music is actually playing and what. A watcher thread waits on
//!   SMTC change events (no polling) and posts WM_AUDIO_STATE to the controller.
//! - Capture runs only while the chosen source is playing and the wallpaper is visible. "Apple Music" uses
//!   per-process loopback (ActivateAudioInterfaceAsync, PROCESS_LOOPBACK, include the process tree) on
//!   AppleMusic.exe, so Discord pings and game audio don't drive the visuals. "All audio" is a plain WASAPI loopback
//!   of the default render endpoint.
//! - Analysis: 1024-point FFT at ~60 Hz, three bands with automatic gain, spectral-flux onsets with an adaptive
//!   threshold, attack/release envelopes.

use crate::scene::AudioFrame;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::Media::Control::*;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Interface, implement};

pub const WM_AUDIO_STATE: u32 = WM_APP + 20;

#[derive(Default)]
pub struct MediaState {
    pub apple_playing: bool,
    pub apple_present: bool,
    pub any_playing: bool,
    pub title: String,
}

pub static MEDIA: Mutex<MediaState> = Mutex::new(MediaState {
    apple_playing: false,
    apple_present: false,
    any_playing: false,
    title: String::new(),
});
pub static FEATURES: Mutex<AudioFrame> = Mutex::new(AudioFrame {
    active: false,
    bass: 0.0,
    mid: 0.0,
    high: 0.0,
    loud: 0.0,
    onset_count: 0,
    onset_strength: 0.0,
    onset_capture_s: 0.0,
    spec: [0.0; 8],
});
static CTL: AtomicIsize = AtomicIsize::new(0);
static WAKE: AtomicIsize = AtomicIsize::new(0);

pub fn qpc_seconds() -> f64 {
    unsafe {
        let mut f = 0i64;
        let mut c = 0i64;
        let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut c);
        c as f64 / f as f64
    }
}

// ------------------------------------------------------------------ SMTC watcher

fn is_apple(aumid: &str) -> bool {
    let a = aumid.to_ascii_lowercase();
    a.contains("applemusic") || a.contains("itunes")
}

fn evaluate(mgr: &GlobalSystemMediaTransportControlsSessionManager) -> MediaState {
    let mut st = MediaState::default();
    let Ok(sessions) = mgr.GetSessions() else { return st };
    let n = sessions.Size().unwrap_or(0);
    for i in 0..n {
        let Ok(s) = sessions.GetAt(i) else { continue };
        let id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        let playing = s
            .GetPlaybackInfo()
            .and_then(|p| p.PlaybackStatus())
            .map(|ps| ps == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
            .unwrap_or(false);
        if playing {
            st.any_playing = true;
        }
        if is_apple(&id) {
            st.apple_present = true;
            st.apple_playing |= playing;
            if let Ok(props) = s.TryGetMediaPropertiesAsync().and_then(|op| op.join()) {
                let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
                let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
                st.title = if artist.is_empty() { title } else { format!("{title} - {artist}") };
            }
        }
    }
    st
}

fn signal() {
    let e = WAKE.load(Ordering::Relaxed);
    if e != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(e as *mut _));
        }
    }
}

/// Start the SMTC watcher. It sleeps on an event that SMTC change notifications set.
pub fn start_watcher(ctl: windows::Win32::Foundation::HWND) {
    CTL.store(ctl.0 as isize, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("smtc".into()).stack_size(256 * 1024).spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(evt) = CreateEventW(None, false, true, None) else { return };
        WAKE.store(evt.0 as isize, Ordering::Relaxed);
        let mgr = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => m,
            Err(e) => {
                crate::logf!("smtc unavailable: {e:?}");
                return;
            }
        };
        let h = windows::Foundation::TypedEventHandler::new(|_, _| {
            signal();
            Ok(())
        });
        let _ = mgr.SessionsChanged(&h);
        let _ = mgr.CurrentSessionChanged(&windows::Foundation::TypedEventHandler::new(|_, _| {
            signal();
            Ok(())
        }));
        let mut subs: Vec<(GlobalSystemMediaTransportControlsSession, i64, i64)> = Vec::new();
        loop {
            WaitForSingleObject(evt, INFINITE);
            // let a burst of notifications settle
            std::thread::sleep(std::time::Duration::from_millis(60));
            for (s, a, b) in subs.drain(..) {
                let _ = s.RemovePlaybackInfoChanged(a);
                let _ = s.RemoveMediaPropertiesChanged(b);
            }
            if let Ok(sessions) = mgr.GetSessions() {
                for i in 0..sessions.Size().unwrap_or(0) {
                    if let Ok(s) = sessions.GetAt(i) {
                        let a = s.PlaybackInfoChanged(&windows::Foundation::TypedEventHandler::new(|_, _| {
                            signal();
                            Ok(())
                        }));
                        let b = s.MediaPropertiesChanged(&windows::Foundation::TypedEventHandler::new(|_, _| {
                            signal();
                            Ok(())
                        }));
                        if let (Ok(a), Ok(b)) = (a, b) {
                            subs.push((s, a, b));
                        }
                    }
                }
            }
            let st = evaluate(&mgr);
            let changed = {
                let mut m = MEDIA.lock().unwrap();
                let c = m.apple_playing != st.apple_playing || m.any_playing != st.any_playing || m.title != st.title
                    || m.apple_present != st.apple_present;
                *m = st;
                c
            };
            if changed {
                let ctl = CTL.load(Ordering::Relaxed);
                let _ = PostMessageW(Some(windows::Win32::Foundation::HWND(ctl as *mut _)), WM_AUDIO_STATE,
                                     windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
            }
        }
    });
}

// ------------------------------------------------------------------ capture

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Process(u32),
    Endpoint,
}

/// PID of the root process named `exe`: when several share the name (Apple Music runs more than one
/// AppleMusic.exe), pick the one whose parent is not also `exe`, so INCLUDE_TARGET_PROCESS_TREE covers the rest.
pub fn find_process(exe: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut all: Vec<(u32, u32)> = Vec::new(); // (pid, parent) of matching processes
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let name = String::from_utf16_lossy(&pe.szExeFile);
                if name.trim_end_matches('\0').eq_ignore_ascii_case(exe) {
                    all.push((pe.th32ProcessID, pe.th32ParentProcessID));
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        all.iter().find(|(_, parent)| !all.iter().any(|(p, _)| p == parent)).or(all.first()).map(|(pid, _)| *pid)
    }
}

/// Test helper: print the default render endpoint's peak meter every 250 ms for `secs` seconds.
pub fn meter(secs: f32) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let run = || -> windows::core::Result<()> {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let m: windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation = dev.Activate(CLSCTX_ALL, None)?;
            let t0 = std::time::Instant::now();
            while t0.elapsed().as_secs_f32() < secs {
                let mut pk = 0f32;
                for _ in 0..5 {
                    pk = pk.max(m.GetPeakValue()?);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                println!("{:5.2}s peak {:.4}", t0.elapsed().as_secs_f32(), pk);
            }
            Ok(())
        };
        match run() {
            Ok(()) => 0,
            Err(e) => {
                println!("meter failed: {e:?}");
                1
            }
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated(isize);

impl IAgileObject_Impl for Activated_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        unsafe {
            let _ = SetEvent(HANDLE(self.0 as *mut _));
        }
        Ok(())
    }
}

unsafe fn activate_process_loopback(pid: u32) -> windows::core::Result<IAudioClient> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // ManuallyDrop: PROPVARIANT's Drop runs PropVariantClear, which would CoTaskMemFree our stack blob
        let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        (*pv.Anonymous.Anonymous).vt = VT_BLOB;
        (*pv.Anonymous.Anonymous).Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        (*pv.Anonymous.Anonymous).Anonymous.blob.pBlobData = &params as *const _ as *mut u8;
        let evt = CreateEventW(None, true, false, None)?;
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(evt.0 as isize).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID,
                                             Some(&*pv as *const PROPVARIANT), &handler)?;
        let r = WaitForSingleObject(evt, 3000);
        let _ = CloseHandle(evt);
        if r != WAIT_OBJECT_0 {
            return Err(windows::core::Error::from(E_FAIL));
        }
        let mut hr = windows::core::HRESULT(0);
        let mut unk = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        unk.ok_or_else(|| windows::core::Error::from(E_FAIL))?.cast()
    }
}

pub struct Capture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub source: Source,
}

impl Capture {
    pub fn start(source: Source) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("audio".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                if let Err(e) = unsafe { capture_loop(source, &s2) } {
                    crate::logf!("audio capture ended: {e:?}");
                }
                let mut f = FEATURES.lock().unwrap();
                f.active = false;
                f.bass = 0.0;
                f.mid = 0.0;
                f.high = 0.0;
                f.loud = 0.0;
                f.spec = [0.0; 8];
            })
            .ok();
        Capture { stop, handle, source }
    }

    pub fn running(&self) -> bool {
        self.handle.as_ref().map(|h| !h.is_finished()).unwrap_or(false)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

unsafe fn capture_loop(source: Source, stop: &AtomicBool) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let (client, fmt_ptr, owned): (IAudioClient, *mut WAVEFORMATEX, bool) = match source {
            Source::Process(pid) => {
                let c = activate_process_loopback(pid)?;
                // process loopback has no mix format: ask for 48 kHz stereo float
                let f = Box::new(WAVEFORMATEX {
                    wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                    nChannels: 2,
                    nSamplesPerSec: 48000,
                    nAvgBytesPerSec: 48000 * 8,
                    nBlockAlign: 8,
                    wBitsPerSample: 32,
                    cbSize: 0,
                });
                (c, Box::into_raw(f), true)
            }
            Source::Endpoint => {
                let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let c: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
                let f = c.GetMixFormat()?;
                (c, f, false)
            }
        };
        let fmt = *fmt_ptr;
        let mut flags = AUDCLNT_STREAMFLAGS_LOOPBACK;
        if owned {
            flags |= AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        }
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, fmt_ptr, None);
        // float if tag 3, or EXTENSIBLE with 32 bits (the shared-mode mix format is float in practice)
        let is_float = fmt.wFormatTag == 3 || (fmt.wFormatTag == 0xFFFE && fmt.wBitsPerSample == 32);
        let channels = fmt.nChannels.max(1) as usize;
        let rate = fmt.nSamplesPerSec as f32;
        let bits = fmt.wBitsPerSample;
        if owned {
            drop(Box::from_raw(fmt_ptr));
        } else {
            CoTaskMemFree(Some(fmt_ptr as *const _));
        }
        init?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        crate::logf!("audio capture started: {source:?} {rate} Hz {channels} ch float={is_float}");

        let timer = CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)?;
        let mut an = Analyzer::new(rate);
        let mut mono = Vec::with_capacity(4096);
        while !stop.load(Ordering::Relaxed) {
            let due = -100_000i64; // 10 ms
            let _ = SetWaitableTimer(timer, &due, 0, None, None, false);
            WaitForSingleObject(timer, 50);
            loop {
                let n = cap.GetNextPacketSize()?;
                if n == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut fl = 0u32;
                let mut qpc = 0u64;
                cap.GetBuffer(&mut data, &mut frames, &mut fl, None, Some(&mut qpc))?;
                mono.clear();
                let silent = fl & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                for f in 0..frames as usize {
                    let mut acc = 0.0f32;
                    if !silent {
                        for c in 0..channels {
                            let k = f * channels + c;
                            acc += if is_float {
                                *(data as *const f32).add(k)
                            } else if bits == 16 {
                                *(data as *const i16).add(k) as f32 / 32768.0
                            } else {
                                0.0
                            };
                        }
                    }
                    mono.push(acc / channels as f32);
                }
                cap.ReleaseBuffer(frames)?;
                // qpc is in 100 ns units at the first frame of the packet
                let t_end = qpc as f64 * 1e-7 + frames as f64 / rate as f64;
                an.push(&mono, t_end);
            }
        }
        let _ = client.Stop();
        let _ = CloseHandle(timer);
        Ok(())
    }
}

// ------------------------------------------------------------------ analysis

const N: usize = 1024;

struct Analyzer {
    rate: f32,
    hop: usize,
    ring: Vec<f32>,
    pos: usize,
    since: usize,
    window: Vec<f32>,
    tw: Vec<(f32, f32)>,
    re: Vec<f32>,
    im: Vec<f32>,
    prev_log: Vec<f32>,
    flux_hist: Vec<f32>,
    peaks: [f32; 4],
    env: [f32; 4],
    spec_peak: [f32; 8],
    spec_env: [f32; 8],
    onset_count: u64,
    last_onset_t: f64,
}

impl Analyzer {
    fn new(rate: f32) -> Analyzer {
        let window = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
        let tw = (0..N / 2).map(|k| {
            let a = -2.0 * std::f32::consts::PI * k as f32 / N as f32;
            (a.cos(), a.sin())
        }).collect();
        Analyzer {
            rate,
            hop: (rate / 60.0) as usize,
            ring: vec![0.0; N],
            pos: 0,
            since: 0,
            window,
            tw,
            re: vec![0.0; N],
            im: vec![0.0; N],
            prev_log: vec![0.0; N / 2],
            flux_hist: Vec::with_capacity(64),
            peaks: [1e-3; 4],
            env: [0.0; 4],
            spec_peak: [1e-3; 8],
            spec_env: [0.0; 8],
            onset_count: 0,
            last_onset_t: 0.0,
        }
    }

    fn push(&mut self, samples: &[f32], t_end: f64) {
        let n = samples.len();
        for (k, &s) in samples.iter().enumerate() {
            self.ring[self.pos] = s;
            self.pos = (self.pos + 1) % N;
            self.since += 1;
            if self.since >= self.hop {
                self.since = 0;
                let t = t_end - (n - 1 - k) as f64 / self.rate as f64;
                self.analyse(t);
            }
        }
    }

    fn fft(&mut self) {
        let (re, im) = (&mut self.re, &mut self.im);
        // bit reversal
        let mut j = 0;
        for i in 1..N {
            let mut bit = N >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= N {
            let step = N / len;
            for s in (0..N).step_by(len) {
                for k in 0..len / 2 {
                    let (c, si) = self.tw[k * step];
                    let (a, b) = (s + k, s + k + len / 2);
                    let tr = re[b] * c - im[b] * si;
                    let ti = re[b] * si + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    fn analyse(&mut self, t: f64) {
        let mut rms = 0.0;
        for i in 0..N {
            let s = self.ring[(self.pos + i) % N];
            rms += s * s;
            self.re[i] = s * self.window[i];
            self.im[i] = 0.0;
        }
        let rms = (rms / N as f32).sqrt();
        self.fft();
        let hz = self.rate / N as f32;
        let bin = |f: f32| ((f / hz) as usize).clamp(1, N / 2 - 1);
        let ranges = [(bin(30.0), bin(180.0)), (bin(180.0), bin(2000.0)), (bin(2000.0), bin(10000.0))];
        let mut bands = [0f32; 4];
        // eight log-spaced bands, 60 Hz .. 12 kHz, for the surface displacement
        let edges: [usize; 9] = std::array::from_fn(|i| bin(60.0 * 200f32.powf(i as f32 / 8.0)));
        let mut spec = [0f32; 8];
        let mut flux = 0.0;
        let flux_top = bin(4000.0);
        for k in 1..N / 2 {
            let m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() / N as f32;
            let lg = (1.0 + 1000.0 * m).ln();
            if k < flux_top {
                let d = lg - self.prev_log[k];
                if d > 0.0 {
                    flux += d * if k < ranges[0].1 { 1.5 } else { 1.0 };
                }
            }
            self.prev_log[k] = lg;
            for (b, (lo, hi)) in ranges.iter().enumerate() {
                if k >= *lo && k < *hi {
                    bands[b] += m * m;
                }
            }
            for b in 0..8 {
                if k >= edges[b] && k < edges[b + 1].max(edges[b] + 1) {
                    spec[b] += m * m;
                }
            }
        }
        for (b, (lo, hi)) in ranges.iter().enumerate() {
            bands[b] = (bands[b] / (*hi - *lo).max(1) as f32).sqrt();
        }
        bands[3] = rms;
        let silent = rms < 2e-4;
        for b in 0..8 {
            let v = (spec[b] / (edges[b + 1].max(edges[b] + 1) - edges[b]) as f32).sqrt();
            self.spec_peak[b] = (self.spec_peak[b] * 0.9986).max(v).max(1e-4);
            let t = if silent { 0.0 } else { ((v / self.spec_peak[b] - 0.3) / 0.7).clamp(0.0, 1.0) };
            let k = if t > self.spec_env[b] { 0.35 } else { 0.08 };
            self.spec_env[b] += (t - self.spec_env[b]) * k;
        }
        // automatic gain: normalise each band to its slowly decaying peak (~8 s half-life at 60 Hz)
        let decay = 0.9986f32;
        let mut norm = [0f32; 4];
        for b in 0..4 {
            self.peaks[b] = (self.peaks[b] * decay).max(bands[b]).max(1e-4);
            norm[b] = if silent { 0.0 } else { (bands[b] / self.peaks[b]).clamp(0.0, 1.0) };
        }
        // shape: ignore the quiet floor of each band so calm passages settle
        let shaped = |x: f32, floor: f32| ((x - floor) / (1.0 - floor)).clamp(0.0, 1.0);
        let targets = [shaped(norm[0], 0.35).powf(1.4), shaped(norm[1], 0.3), shaped(norm[2], 0.3), shaped(norm[3], 0.2)];
        let (att, rel) = ([0.5, 0.25, 0.3, 0.06], [0.12, 0.05, 0.08, 0.015]);
        for b in 0..4 {
            let k = if targets[b] > self.env[b] { att[b] } else { rel[b] };
            self.env[b] += (targets[b] - self.env[b]) * k;
        }
        // onsets: flux above mean + 1.5 sigma of the last ~0.75 s
        let mut strength = 0.0;
        if self.flux_hist.len() >= 20 {
            let n = self.flux_hist.len() as f32;
            let mean = self.flux_hist.iter().sum::<f32>() / n;
            let var = self.flux_hist.iter().map(|f| (f - mean) * (f - mean)).sum::<f32>() / n;
            let sd = var.sqrt();
            let thr = mean + 1.5 * sd + 0.5;
            if !silent && flux > thr && t - self.last_onset_t > 0.12 {
                strength = ((flux - mean) / (4.0 * sd + 1.0)).clamp(0.0, 1.0);
                self.last_onset_t = t;
                self.onset_count += 1;
            }
        }
        if self.flux_hist.len() >= 45 {
            self.flux_hist.remove(0);
        }
        self.flux_hist.push(flux);

        let mut f = FEATURES.lock().unwrap();
        f.active = true;
        f.bass = self.env[0];
        f.mid = self.env[1];
        f.high = self.env[2];
        f.loud = self.env[3];
        f.spec = self.spec_env;
        if strength > 0.0 {
            f.onset_count = self.onset_count;
            f.onset_strength = strength;
            f.onset_capture_s = t;
        }
    }
}
