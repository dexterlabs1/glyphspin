//! The tray / menu-bar menu as data, so Windows (tray) and macOS (menu-bar extra) show the same menu and map
//! the same command ids to the same settings.

use crate::settings::Settings;
use crate::theme::{ROTATE, THEMES};
use crate::shapes::SHAPES;

pub const CMD_REROLL: u32 = 100;
pub const CMD_SHAPE: u32 = 200; // +0 random, +1.. SHAPES
pub const CMD_STYLE: u32 = 300;
pub const CMD_SIZE: u32 = 310;
pub const CMD_THEME: u32 = 320; // +0..8 THEMES
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
pub const CMD_BLACK_BG: u32 = 450;
pub const CMD_ROTATE: u32 = 460; // +0..5 ROTATE
pub const CMD_MOTION: u32 = 470; // +0..2 MOTIONS
pub const CMD_PAUSE: u32 = 400;
pub const CMD_STARTUP: u32 = 401;
pub const CMD_QUIT: u32 = 499;

pub const STYLES: [&str; 3] = ["auto", "ramp", "braille"];
pub const SIZES: [&str; 3] = ["fine", "medium", "coarse"];
pub const MOTIONS: [&str; 2] = ["bounce", "wander"];
pub const FPS: [u32; 3] = [15, 30, 60];
pub const HDRS: [&str; 3] = ["auto", "on", "off"];
pub const HIGHLIGHTS: [u32; 5] = [0, 25, 50, 75, 100];
pub const LEVELS: [&str; 3] = ["off", "subtle", "full"];
pub const SOURCES: [&str; 3] = ["apple", "all", "off"];
pub const DIM_MIN: [u32; 4] = [5, 10, 20, 0];
pub const BLACK_MIN: [u32; 4] = [15, 30, 60, 0];
pub const REROLL_MIN: [u32; 4] = [15, 25, 45, 0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Check {
    /// a check box (shown checked or not)
    Box(bool),
    /// one of a radio group
    Radio(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Cmd { id: u32, label: String, check: Check, default: bool },
    /// greyed-out text: a group heading or an information line
    Note(String),
    Sep,
    Sub(String, Vec<Item>),
}

/// Platform bits of the menu: the start-at-login label and state, display information lines, and extra notes
/// under Music (macOS: version / permission hints).
pub struct Extras<'a> {
    pub startup_label: &'a str,
    pub startup_on: bool,
    pub display_info: &'a [String],
    pub music_notes: &'a [String],
}

fn cmd(id: u32, label: &str, check: Check) -> Item {
    Item::Cmd { id, label: label.to_string(), check, default: false }
}

fn radio(id: u32, label: &str, on: bool) -> Item {
    cmd(id, label, Check::Radio(on))
}

fn note(text: &str) -> Item {
    Item::Note(text.to_string())
}

pub fn build(s: &Settings, x: &Extras) -> Vec<Item> {
    let mut m = Vec::new();
    m.push(Item::Cmd { id: CMD_REROLL, label: "Reroll".into(), check: Check::Box(false), default: true });

    let mut shapes = vec![radio(CMD_SHAPE, "Random", s.shape == "random"), Item::Sep];
    for (i, name) in SHAPES.iter().enumerate() {
        shapes.push(radio(CMD_SHAPE + 1 + i as u32, &name.replace('_', " "), s.shape == *name));
    }
    m.push(Item::Sub("Shape".into(), shapes));

    let mut look = Vec::new();
    for (i, v) in STYLES.iter().enumerate() {
        look.push(radio(CMD_STYLE + i as u32, &format!("Style: {v}"), s.style == *v));
    }
    look.push(Item::Sep);
    for (i, v) in SIZES.iter().enumerate() {
        look.push(radio(CMD_SIZE + i as u32, &format!("Glyphs: {v}"), s.glyph_size == *v));
    }
    look.push(Item::Sep);
    for (i, (v, label)) in MOTIONS.iter().zip(["Motion: bounce", "Motion: wander"]).enumerate() {
        look.push(radio(CMD_MOTION + i as u32, label, s.motion == *v));
    }
    m.push(Item::Sub("Look".into(), look));

    let mut th: Vec<Item> = THEMES.iter().enumerate()
        .map(|(i, t)| radio(CMD_THEME + i as u32, t.label, s.theme == t.name)).collect();
    th.push(Item::Sep);
    th.push(cmd(CMD_BLACK_BG, "OLED black background", Check::Box(s.black_bg)));
    th.push(Item::Sep);
    th.push(note("Rotate themes"));
    let labels = ["Off", "With each new shape", "Every 15 min", "Every 30 min", "Every 60 min"];
    for (i, (v, label)) in ROTATE.iter().zip(labels).enumerate() {
        th.push(radio(CMD_ROTATE + i as u32, label, s.theme_rotate == *v));
    }
    m.push(Item::Sub("Theme".into(), th));

    let fx = LEVELS.iter().enumerate().map(|(i, v)| radio(CMD_EFFECTS + i as u32, v, s.effects == *v)).collect();
    m.push(Item::Sub("Effects".into(), fx));

    let mut au = vec![note("React to")];
    for (i, (v, label)) in SOURCES.iter().zip(["Apple Music", "All audio", "Off"]).enumerate() {
        au.push(radio(CMD_AUDIO_SRC + i as u32, label, s.audio_source == *v));
    }
    au.push(Item::Sep);
    au.push(note("Intensity"));
    for (i, v) in LEVELS.iter().enumerate() {
        au.push(radio(CMD_AUDIO_LVL + i as u32, v, s.audio_level == *v));
    }
    if !x.music_notes.is_empty() {
        au.push(Item::Sep);
        au.extend(x.music_notes.iter().map(|t| note(t)));
    }
    m.push(Item::Sub("Music".into(), au));

    let mut hd = Vec::new();
    for (i, v) in HDRS.iter().enumerate() {
        hd.push(radio(CMD_HDR + i as u32, &format!("HDR: {v}"), s.hdr == *v));
    }
    hd.push(Item::Sep);
    hd.push(note("Highlights"));
    for (i, v) in HIGHLIGHTS.iter().enumerate() {
        hd.push(radio(CMD_HIGHLIGHT + i as u32, &format!("{v}%"), s.highlight == *v));
    }
    hd.push(Item::Sep);
    hd.extend(x.display_info.iter().map(|t| note(t)));
    m.push(Item::Sub("Display".into(), hd));

    let mins = |v: u32| if v == 0 { "never".to_string() } else { format!("{v} min") };
    let mut ol = vec![cmd(CMD_OLED, "OLED care", Check::Box(s.oled_care)), Item::Sep, note("Dim when idle")];
    for (i, v) in DIM_MIN.iter().enumerate() {
        ol.push(radio(CMD_DIM + i as u32, &mins(*v), s.idle_dim_min == *v));
    }
    ol.push(Item::Sep);
    ol.push(note("Then black after"));
    for (i, v) in BLACK_MIN.iter().enumerate() {
        ol.push(radio(CMD_BLACK + i as u32, &mins(*v), s.idle_black_min == *v));
    }
    ol.push(Item::Sep);
    ol.push(note("New shape every"));
    for (i, v) in REROLL_MIN.iter().enumerate() {
        ol.push(radio(CMD_AUTOREROLL + i as u32, &mins(*v), s.auto_reroll_min == *v));
    }
    m.push(Item::Sub("OLED care".into(), ol));

    let fp = FPS.iter().enumerate().map(|(i, v)| radio(CMD_FPS + i as u32, &format!("{v} fps max"), s.fps_cap == *v))
        .collect();
    m.push(Item::Sub("Frame rate".into(), fp));

    m.push(Item::Sep);
    m.push(cmd(CMD_PAUSE, "Pause", Check::Box(s.paused)));
    m.push(cmd(CMD_STARTUP, x.startup_label, Check::Box(x.startup_on)));
    m.push(Item::Sep);
    m.push(cmd(CMD_QUIT, "Quit", Check::Box(false)));
    m
}

/// What a menu command does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Reroll,
    /// a settings key and its new value (as `Settings::set` takes them)
    Set(&'static str, String),
    ToggleStartup,
    Quit,
    None,
}

pub fn action(cmd: u32, s: &Settings) -> Action {
    let set = |k: &'static str, v: &str| Action::Set(k, v.to_string());
    match cmd {
        CMD_REROLL => Action::Reroll,
        c if (CMD_SHAPE..CMD_SHAPE + 1 + SHAPES.len() as u32).contains(&c) => {
            set("shape", if c == CMD_SHAPE { "random" } else { SHAPES[(c - CMD_SHAPE - 1) as usize] })
        }
        c if (CMD_STYLE..CMD_STYLE + 3).contains(&c) => set("style", STYLES[(c - CMD_STYLE) as usize]),
        c if (CMD_SIZE..CMD_SIZE + 3).contains(&c) => set("glyph_size", SIZES[(c - CMD_SIZE) as usize]),
        c if (CMD_THEME..CMD_THEME + THEMES.len() as u32).contains(&c) => {
            set("theme", THEMES[(c - CMD_THEME) as usize].name)
        }
        c if (CMD_MOTION..CMD_MOTION + MOTIONS.len() as u32).contains(&c) => {
            set("motion", MOTIONS[(c - CMD_MOTION) as usize])
        }
        CMD_BLACK_BG => set("black_bg", if s.black_bg { "false" } else { "true" }),
        c if (CMD_ROTATE..CMD_ROTATE + ROTATE.len() as u32).contains(&c) => {
            set("theme_rotate", ROTATE[(c - CMD_ROTATE) as usize])
        }
        c if (CMD_FPS..CMD_FPS + 3).contains(&c) => set("fps_cap", &FPS[(c - CMD_FPS) as usize].to_string()),
        c if (CMD_HDR..CMD_HDR + 3).contains(&c) => set("hdr", HDRS[(c - CMD_HDR) as usize]),
        c if (CMD_HIGHLIGHT..CMD_HIGHLIGHT + 5).contains(&c) => {
            set("highlight", &HIGHLIGHTS[(c - CMD_HIGHLIGHT) as usize].to_string())
        }
        c if (CMD_EFFECTS..CMD_EFFECTS + 3).contains(&c) => set("effects", LEVELS[(c - CMD_EFFECTS) as usize]),
        c if (CMD_AUDIO_SRC..CMD_AUDIO_SRC + 3).contains(&c) => {
            set("audio_source", SOURCES[(c - CMD_AUDIO_SRC) as usize])
        }
        c if (CMD_AUDIO_LVL..CMD_AUDIO_LVL + 3).contains(&c) => {
            set("audio_level", LEVELS[(c - CMD_AUDIO_LVL) as usize])
        }
        CMD_PAUSE => set("paused", if s.paused { "false" } else { "true" }),
        CMD_OLED => set("oled_care", if s.oled_care { "false" } else { "true" }),
        c if (CMD_DIM..CMD_DIM + 4).contains(&c) => set("idle_dim_min", &DIM_MIN[(c - CMD_DIM) as usize].to_string()),
        c if (CMD_BLACK..CMD_BLACK + 4).contains(&c) => {
            set("idle_black_min", &BLACK_MIN[(c - CMD_BLACK) as usize].to_string())
        }
        c if (CMD_AUTOREROLL..CMD_AUTOREROLL + 4).contains(&c) => {
            set("auto_reroll_min", &REROLL_MIN[(c - CMD_AUTOREROLL) as usize].to_string())
        }
        CMD_STARTUP => Action::ToggleStartup,
        CMD_QUIT => Action::Quit,
        _ => Action::None,
    }
}
