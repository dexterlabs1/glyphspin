//! Music analysis shared by both platforms: the now-playing state the platform's media watcher fills in, and the
//! analyser that turns captured samples into the features the scene reacts to (1024-point FFT at ~60 Hz, three
//! bands with automatic gain, eight log-spaced spectrum bands, spectral-flux onsets with an adaptive threshold,
//! attack/release envelopes). Capture itself is platform code.

use crate::scene::AudioFrame;
use std::sync::Mutex;

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
// ------------------------------------------------------------------ analysis

const N: usize = 1024;

pub struct Analyzer {
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
    pub fn new(rate: f32) -> Analyzer {
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

    /// Feed mono samples; `t_end` is the capture time of the last sample on the platform's host clock.
    pub fn push(&mut self, samples: &[f32], t_end: f64) {
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
