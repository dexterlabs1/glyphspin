//! Settings: a flat JSON object in the platform's settings folder (%APPDATA%\Glyphspin\settings.json on Windows,
//! ~/Library/Application Support/Glyphspin/settings.json on macOS). Hand-rolled (string/number/bool values only) so
//! the binary carries no serde.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub shape: String,      // "random" or a shape name
    pub style: String,      // auto | ramp | braille
    pub glyph_size: String, // fine | medium | coarse
    pub accent: String,     // teal | amber | ice | sage | paper
    pub fps_cap: u32,       // 15 | 30 | 60
    pub paused: bool,
    pub start_with_windows: bool, // start at login (Windows Run key / macOS login item); the OS holds the truth
    pub hdr: String,     // auto | on | off
    pub highlight: u32,  // 0..100
    pub peak_nits: f32,  // highlight cap (also capped by the display's MaxLuminance)
    pub effects: String, // off | subtle | full
    pub audio_source: String, // apple | all | off
    pub audio_level: String,  // off | subtle | full
    pub oled_care: bool,      // wander, pixel orbit, rotating field, rest brightness, APL cap, idle dim, auto reroll
    pub idle_dim_min: u32,    // minutes without input before dimming (0 = never)
    pub idle_black_min: u32,  // further minutes before fading to black (0 = never)
    pub auto_reroll_min: u32, // minutes between automatic rerolls (0 = never)
    /// reroll combination weights: render modes, ambient effect, transition
    pub weights: crate::fx::Weights,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shape: "random".into(),
            style: "auto".into(),
            glyph_size: "medium".into(),
            accent: "teal".into(),
            fps_cap: 30,
            paused: false,
            start_with_windows: false,
            hdr: "auto".into(),
            highlight: 60,
            peak_nits: 600.0,
            effects: "full".into(),
            audio_source: "apple".into(),
            audio_level: "subtle".into(),
            oled_care: true,
            idle_dim_min: 10,
            idle_black_min: 30,
            auto_reroll_min: 25,
            weights: crate::fx::Weights::default(),
        }
    }
}

pub const ACCENTS: [(&str, &str); 5] =
    [("teal", "#4fc7bd"), ("amber", "#e0a64a"), ("ice", "#8fb8de"), ("sage", "#9cc49a"), ("paper", "#d9d4c7")];

pub fn accent_hex(name: &str) -> &'static str {
    ACCENTS.iter().find(|(n, _)| *n == name).map(|(_, h)| *h).unwrap_or("#4fc7bd")
}

fn path() -> std::path::PathBuf {
    crate::util::app_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings::default();
        let Ok(text) = std::fs::read_to_string(path()) else { return s };
        let m = parse(&text);
        let st = |k: &str, d: &mut String| {
            if let Some(Value::Str(v)) = m.get(k) {
                *d = v.clone();
            }
        };
        st("shape", &mut s.shape);
        st("style", &mut s.style);
        st("glyph_size", &mut s.glyph_size);
        st("accent", &mut s.accent);
        st("hdr", &mut s.hdr);
        st("effects", &mut s.effects);
        st("audio_source", &mut s.audio_source);
        st("audio_level", &mut s.audio_level);
        if let Some(Value::Num(v)) = m.get("fps_cap") {
            s.fps_cap = (*v as u32).clamp(5, 120);
        }
        if let Some(Value::Num(v)) = m.get("highlight") {
            s.highlight = (*v as u32).min(100);
        }
        if let Some(Value::Num(v)) = m.get("peak_nits") {
            s.peak_nits = (*v as f32).clamp(100.0, 4000.0);
        }
        if let Some(Value::Bool(v)) = m.get("paused") {
            s.paused = *v;
        }
        if let Some(Value::Bool(v)) = m.get("start_with_windows") {
            s.start_with_windows = *v;
        }
        if let Some(Value::Bool(v)) = m.get("oled_care") {
            s.oled_care = *v;
        }
        {
            let w = &mut s.weights;
            for (k, d) in [("w_solid", &mut w.solid), ("w_wire", &mut w.wire), ("w_points", &mut w.points),
                           ("w_plain", &mut w.plain), ("w_sweep", &mut w.sweep), ("w_density", &mut w.density),
                           ("w_dissolve", &mut w.dissolve), ("w_morph", &mut w.morph)] {
                if let Some(Value::Num(v)) = m.get(k) {
                    *d = (*v as f32).clamp(0.0, 100.0);
                }
            }
        }
        for (k, d) in [("idle_dim_min", &mut s.idle_dim_min), ("idle_black_min", &mut s.idle_black_min),
                       ("auto_reroll_min", &mut s.auto_reroll_min)] {
            if let Some(Value::Num(v)) = m.get(k) {
                *d = (*v as u32).min(24 * 60);
            }
        }
        s
    }

    /// Apply one `key=value` (tray/menu commands and `--send set`). False for an unknown key.
    pub fn set(&mut self, key: &str, val: &str) -> bool {
        match key {
            "shape" => self.shape = val.into(),
            "style" => self.style = val.into(),
            "glyph_size" => self.glyph_size = val.into(),
            "accent" => self.accent = val.into(),
            "fps_cap" => self.fps_cap = val.parse().unwrap_or(30),
            "hdr" => self.hdr = val.into(),
            "highlight" => self.highlight = val.parse().unwrap_or(60),
            "peak_nits" => self.peak_nits = val.parse().unwrap_or(600.0),
            "effects" => self.effects = val.into(),
            "audio_source" => self.audio_source = val.into(),
            "audio_level" => self.audio_level = val.into(),
            "paused" => self.paused = val == "true",
            "oled_care" => self.oled_care = val == "true",
            "idle_dim_min" => self.idle_dim_min = val.parse().unwrap_or(10),
            "idle_black_min" => self.idle_black_min = val.parse().unwrap_or(30),
            "auto_reroll_min" => self.auto_reroll_min = val.parse().unwrap_or(25),
            _ => return false,
        }
        true
    }

    pub fn save(&self) {
        let q = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
        let text = format!(
            "{{\n  \"shape\": {},\n  \"style\": {},\n  \"glyph_size\": {},\n  \"accent\": {},\n  \"fps_cap\": {},\n  \"paused\": {},\n  \"start_with_windows\": {},\n  \"hdr\": {},\n  \"highlight\": {},\n  \"peak_nits\": {},\n  \"effects\": {},\n  \"audio_source\": {},\n  \"audio_level\": {},\n  \"oled_care\": {},\n  \"idle_dim_min\": {},\n  \"idle_black_min\": {},\n  \"auto_reroll_min\": {},\n  \"w_solid\": {},\n  \"w_wire\": {},\n  \"w_points\": {},\n  \"w_plain\": {},\n  \"w_sweep\": {},\n  \"w_density\": {},\n  \"w_dissolve\": {},\n  \"w_morph\": {}\n}}\n",
            q(&self.shape), q(&self.style), q(&self.glyph_size), q(&self.accent), self.fps_cap, self.paused,
            self.start_with_windows, q(&self.hdr), self.highlight, self.peak_nits, q(&self.effects),
            q(&self.audio_source), q(&self.audio_level), self.oled_care, self.idle_dim_min, self.idle_black_min,
            self.auto_reroll_min, self.weights.solid, self.weights.wire, self.weights.points, self.weights.plain,
            self.weights.sweep, self.weights.density, self.weights.dissolve, self.weights.morph
        );
        let _ = std::fs::write(path(), text);
    }
}

#[derive(Debug)]
enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

/// Parse a flat JSON object of string / number / bool values. Anything else is skipped.
fn parse(text: &str) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    let c: Vec<char> = text.chars().collect();
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < c.len() && (c[*i].is_whitespace() || c[*i] == ',' || c[*i] == '{' || c[*i] == '}') {
            *i += 1;
        }
    };
    let read_str = |i: &mut usize| -> Option<String> {
        if *i >= c.len() || c[*i] != '"' {
            return None;
        }
        *i += 1;
        let mut s = String::new();
        while *i < c.len() && c[*i] != '"' {
            if c[*i] == '\\' && *i + 1 < c.len() {
                *i += 1;
            }
            s.push(c[*i]);
            *i += 1;
        }
        *i += 1;
        Some(s)
    };
    loop {
        skip_ws(&mut i);
        let Some(key) = read_str(&mut i) else { break };
        while i < c.len() && (c[i].is_whitespace() || c[i] == ':') {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        if c[i] == '"' {
            if let Some(v) = read_str(&mut i) {
                m.insert(key, Value::Str(v));
            }
        } else {
            let start = i;
            while i < c.len() && !matches!(c[i], ',' | '}' | '\n') {
                i += 1;
            }
            let tok: String = c[start..i].iter().collect::<String>().trim().to_string();
            if tok == "true" || tok == "false" {
                m.insert(key, Value::Bool(tok == "true"));
            } else if let Ok(n) = tok.parse::<f64>() {
                m.insert(key, Value::Num(n));
            }
        }
    }
    m
}
