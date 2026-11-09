//! Music reactivity.
//!
//! - SMTC (Windows.Media.Control) tells us when Apple Music is actually playing and what. A watcher thread waits on
//!   SMTC change events (no polling) and posts WM_AUDIO_STATE to the controller.
//! - Capture runs only while the chosen source is playing and the wallpaper is visible. "Apple Music" uses
//!   per-process loopback (ActivateAudioInterfaceAsync, PROCESS_LOOPBACK, include the process tree) on
//!   AppleMusic.exe, so Discord pings and game audio don't drive the visuals. "All audio" is a plain WASAPI loopback
//!   of the default render endpoint.
//! - Analysis (FFT, bands, onsets) is shared with macOS: glyphspin_core::audio.

pub use glyphspin_core::audio::{FEATURES, MEDIA, MediaState};
use glyphspin_core::audio::Analyzer;
use glyphspin_core::logf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
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
                logf!("smtc unavailable: {e:?}");
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
                    logf!("audio capture ended: {e:?}");
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
        logf!("audio capture started: {source:?} {rate} Hz {channels} ch float={is_float}");

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
