//! The platform seam. Each backend implements `Platform` (settings folder, glyph atlas, single instance + the
//! command channel the CLI talks to, and the wallpaper itself) and `Surface` (the GPU target the cell grid is
//! drawn into). Inside `run`, a backend supplies the window behind the desktop icons, observe-only input, the
//! visibility / pause signals, the tray or menu bar and audio capture, and drives the shared core with them:
//! `glyphspin_core::engine::Engine` per screen, `menu` for the menu, `audio::Analyzer` for music, `dump` for
//! readbacks. Neither backend reimplements anything the core has.

use glyphspin_core::atlas::Atlas;
use glyphspin_core::engine::Consts;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::Windows as Os;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::MacOs as Os;

/// Kinds of message the CLI sends to the running instance.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Msg {
    /// a text command: `reroll [shape]`, `ripple X Y`, `set key=value`, `stats PATH`, `fx ...`, ...
    Cmd,
    /// read back the next frame into this BMP path (stats go to the .txt next to it)
    Dump,
}

pub trait Platform {
    /// Settings and log folder (created if missing).
    fn app_dir() -> std::path::PathBuf;
    /// Glyph atlas from the platform's monospace font for a target cell width in pixels.
    fn build_atlas(cw_target: u32) -> Atlas;
    /// Send a message to the running instance; false if none is running.
    fn send(kind: Msg, text: &str) -> bool;
    /// Ask the running instance to quit and wait for it to exit; returns the process exit code for the CLI.
    fn quit_running() -> i32;
    /// Platform-only test commands (e.g. Windows' `--meter`); Some(exit code) when one ran.
    fn test_cli(args: &[String]) -> Option<i32>;
    /// Become the single instance and run the wallpaper until quit. A second launch rerolls the running one.
    fn run(args: &[String]);
}

/// The GPU target: one fullscreen pass turns the cell grid into glyph pixels (shaders/glyph.hlsl, shaders/glyph.metal).
pub trait Surface {
    type Error;
    /// Upload the glyph atlas (R8 coverage, `w`x`h`).
    fn set_atlas(&mut self, pixels: &[u8], w: u32, h: u32) -> Result<(), Self::Error>;
    /// Upload the cell grid: 4 halfs per cell (glyph, level, tint, glow).
    fn upload_cells(&mut self, data: &[u16], cols: u32, rows: u32) -> Result<(), Self::Error>;
    /// Draw and present one frame. With `readback`, the frame is also copied to the CPU before presenting and
    /// returned as raw texels (8 bytes per pixel for FP16, 4 for BGRA8) with the row pitch.
    fn draw(&mut self, consts: &Consts, readback: bool) -> Result<Option<(Vec<u8>, usize)>, Self::Error>;
    /// A target exists and frames can be drawn.
    fn ready(&self) -> bool;
}
