//! Glyphspin's platform-free core: shapes, the cell rasteriser, the scene (motion, interaction, field, effects),
//! render modes, OLED care, settings, audio analysis, the per-screen frame controller and the headless pipeline.
//! Nothing in here talks to an OS API; the Windows and macOS backends (src/platform/) supply the window, the GPU
//! surface, input, visibility, the tray / menu bar, audio capture, the settings folder and the glyph atlas.

pub mod atlas;
pub mod audio;
pub mod dump;
pub mod engine;
pub mod fx;
pub mod menu;
pub mod offline;
pub mod oled;
pub mod raster;
pub mod scene;
pub mod settings;
pub mod shapes;
pub mod shapes_x;
pub mod util;
