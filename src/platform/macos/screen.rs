//! One wallpaper window per screen: borderless, at the desktop window level (above the wallpaper, below Finder's
//! icon layer at kCGDesktopIconWindowLevel), in every Space, ignoring the mouse so Finder keeps every click. Each
//! screen has its own Metal surface and its own Engine (scene, grid, OLED care).

use super::glyphs;
use super::metal::{Gpu, MetalSurface};
use super::super::Surface;
use glyphspin_core::engine::Engine;
use glyphspin_core::logf;
use glyphspin_core::settings::Settings;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSColor, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
                    NSWindowStyleMask};
use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
use objc2_foundation::{NSPoint, NSRect};
use std::rc::Rc;
use std::time::Instant;

pub struct Screen {
    pub name: String,
    /// global points, bottom-left origin
    pub frame: NSRect,
    pub scale: f64,
    pub w: u32,
    pub h: u32,
    pub window: Retained<NSWindow>,
    pub surface: MetalSurface,
    pub eng: Engine,
    /// EDR headroom now (multiples of SDR white) and the most the screen can do
    pub edr_now: f32,
    pub edr_max: f32,
    /// our window's occlusion state says it isn't visible (only trusted once shown)
    pub occluded: bool,
    /// ordinary windows cover the whole screen (window-list sampling)
    pub covered: bool,
    pub active: bool,
    pub shown: bool,
    pub next_due: Option<Instant>,
    pub last_frame: Instant,
}

impl Screen {
    pub fn new(mtm: MainThreadMarker, ns: &NSScreen, gpu: Rc<Gpu>, seed: u64, shape: Option<&'static str>)
               -> Screen {
        let frame = ns.frame();
        let scale = ns.backingScaleFactor();
        let w = (frame.size.width * scale).round().max(1.0) as u32;
        let h = (frame.size.height * scale).round().max(1.0) as u32;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer_screen(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
                Some(ns),
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setFrame_display(frame, false);
        window.setLevel(CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey) as isize);
        window.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle);
        window.setIgnoresMouseEvents(true);
        window.setOpaque(true);
        window.setHasShadow(false);
        window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(11.0 / 255.0, 14.0 / 255.0,
                                                                                   15.0 / 255.0, 1.0)));
        let surface = MetalSurface::new(gpu);
        let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
        view.setLayer(Some(&surface.layer));
        view.setWantsLayer(true);
        window.setContentView(Some(&view));
        let edr_max = ns.maximumPotentialExtendedDynamicRangeColorComponentValue() as f32;
        let edr_now = ns.maximumExtendedDynamicRangeColorComponentValue() as f32;
        let name = ns.localizedName().to_string();
        logf!("screen '{}' {}x{} px @{}x, frame {:?}, EDR now {:.2} max {:.2}", name, w, h, scale,
              (frame.origin.x, frame.origin.y, frame.size.width, frame.size.height), edr_now, edr_max);
        Screen {
            name, frame, scale, w, h, window, surface, eng: Engine::new(seed, shape), edr_now, edr_max,
            occluded: false, covered: false, active: false, shown: false, next_due: None, last_frame: Instant::now(),
        }
    }

    /// Size the surface for the HDR choice and rebuild the atlas + grid for the glyph size.
    pub fn configure(&mut self, s: &Settings, hdr: bool) {
        self.surface.resize(self.w, self.h, self.scale, hdr);
        self.rebuild_grid(s);
    }

    pub fn rebuild_grid(&mut self, s: &Settings) {
        let cw_target = Engine::cw_target(s, self.h);
        let atlas = glyphs::build(cw_target);
        self.eng.set_grid(self.w, self.h, &atlas, cw_target);
        logf!("'{}': grid {}x{} cells of {}x{} px, font '{}', {} glyphs drawn procedurally", self.name, self.eng.cols,
              self.eng.rows, self.eng.cw, self.eng.ch, atlas.face, atlas.missing);
        if let Err(e) = self.surface.set_atlas(&atlas.pixels, atlas.w, atlas.h) {
            logf!("atlas upload failed {e}");
        }
    }

    pub fn contains(&self, p: NSPoint) -> bool {
        let f = self.frame;
        p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y < f.origin.y + f.size.height
    }

    /// Global point (bottom-left origin) -> pixels from this screen's top-left.
    pub fn to_px(&self, p: NSPoint) -> (f32, f32) {
        let f = self.frame;
        (((p.x - f.origin.x) * self.scale) as f32, ((f.origin.y + f.size.height - p.y) * self.scale) as f32)
    }

    /// Global point -> the scene's grid coordinates (pixels from the grid origin).
    pub fn to_grid(&self, p: NSPoint) -> (f32, f32) {
        let (x, y) = self.to_px(p);
        (x - self.eng.origin.0 as f32, y - self.eng.origin.1 as f32)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.window.orderOut(None);
        self.window.close();
    }
}
