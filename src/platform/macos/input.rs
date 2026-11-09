//! Desktop gestures on macOS, from a global NSEvent monitor. Like the Windows hook it only observes: a global monitor
//! receives copies of events delivered to other apps and cannot block them, so Finder keeps every click, drag and
//! selection. Mouse events need no permission (only key events would need Accessibility). It is installed only
//! while a screen is drawing.
//!
//! Gestures on the bare desktop (the front-most window under the pointer is at a desktop level):
//! - middle-drag, or Option + left-drag (for trackpads and mice without a middle button): rotate; with Shift, turn
//!   4D shapes; a double-click of either rerolls
//! - scroll: zoom (not with Control, which is the system's screen zoom, or Command)
//! - click: a ripple; click and hold still: the exploded view
//!
//! Telling a click on a Finder icon from a click on the bare desktop would need the Accessibility API, so
//! Glyphspin doesn't ask for it: an icon click ripples too.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use std::ptr::NonNull;

pub struct Monitor(Retained<AnyObject>);

impl Monitor {
    pub fn install() -> Option<Monitor> {
        let mask = NSEventMask::LeftMouseDown | NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged
            | NSEventMask::OtherMouseDown | NSEventMask::OtherMouseUp | NSEventMask::OtherMouseDragged
            | NSEventMask::ScrollWheel;
        let block = RcBlock::new(|ev: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a valid event for the duration of the call
            handle(unsafe { ev.as_ref() });
        });
        NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block).map(Monitor)
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe { NSEvent::removeMonitor(&self.0) };
    }
}

/// What the gesture handler in mod.rs gets.
pub enum Gesture {
    /// rotate start: global point, timestamp (s), shift held, click count
    RotateDown { shift: bool, clicks: isize },
    RotateMove,
    RotateUp,
    LeftDown,
    LeftUp,
    Scroll(f32),
}

fn handle(ev: &NSEvent) {
    let flags = ev.modifierFlags();
    let opt = flags.contains(NSEventModifierFlags::Option);
    let shift = flags.contains(NSEventModifierFlags::Shift);
    let t = ev.timestamp();
    let p = NSEvent::mouseLocation();
    let g = match ev.r#type() {
        NSEventType::OtherMouseDown if ev.buttonNumber() == 2 => Gesture::RotateDown { shift, clicks: ev.clickCount() },
        NSEventType::LeftMouseDown if opt => Gesture::RotateDown { shift, clicks: ev.clickCount() },
        NSEventType::LeftMouseDown => Gesture::LeftDown,
        NSEventType::OtherMouseDragged | NSEventType::LeftMouseDragged => Gesture::RotateMove,
        NSEventType::OtherMouseUp if ev.buttonNumber() == 2 => Gesture::RotateUp,
        NSEventType::LeftMouseUp => Gesture::LeftUp,
        NSEventType::ScrollWheel => {
            if flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Command) {
                return;
            }
            // a wheel notch is ~1 line; trackpads send many small pixel deltas
            let dy = ev.scrollingDeltaY() as f32;
            Gesture::Scroll(if ev.hasPreciseScrollingDeltas() { dy / 40.0 } else { dy })
        }
        _ => return,
    };
    super::gesture(g, p, t);
}
