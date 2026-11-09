//! The menu-bar extra: a template image of the dotted ring and the same menu as the Windows tray
//! (glyphspin_core::menu), rebuilt from the settings each time it opens.

use glyphspin_core::menu::{Check, Item};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuDelegate, NSMenuItem,
                    NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace};
use objc2_foundation::{NSObjectProtocol, NSSize, NSString};

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Target doesn't implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "GlyphspinMenuTarget"]
    #[ivars = ()]
    pub struct Target;

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            super::menu_opening(menu);
        }
    }

    impl Target {
        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            super::command(sender.tag() as u32);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Target> {
        let this = Target::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

pub struct MenuBar {
    item: Retained<NSStatusItem>,
    _menu: Retained<NSMenu>,
    target: Retained<Target>,
    mtm: MainThreadMarker,
}

/// The brand ring as an 18 pt template image (drawn at 2x): a torus seen at an angle, as dots.
fn ring_image() -> Option<Retained<NSImage>> {
    let n = 36usize;
    let mut px = vec![0u8; n * n * 4];
    let (cx, cy) = (17.5f32, 17.5f32);
    for y in 0..n {
        for x in 0..n {
            let dx = (x as f32 - cx) / 16.0;
            let dy = (y as f32 - cy) / 10.0;
            let r = (dx * dx + dy * dy).sqrt();
            let ring = (1.0 - ((r - 0.72) / 0.22).abs()).clamp(0.0, 1.0);
            let a = ((ring * 1.6).min(1.0) * 255.0) as u8;
            px[(y * n + x) * 4 + 3] = a; // black + alpha: a template image
        }
    }
    let space = CGColorSpace::new_device_rgb();
    let ctx = unsafe {
        CGBitmapContextCreate(px.as_mut_ptr() as *mut _, n, n, 8, n * 4, space.as_deref(),
                              1 /* kCGImageAlphaPremultipliedLast */)
    }?;
    let img = CGBitmapContextCreateImage(Some(&ctx))?;
    let ns = NSImage::initWithCGImage_size(NSImage::alloc(), &img, NSSize::new(18.0, 18.0));
    ns.setTemplate(true);
    Some(ns)
}

impl MenuBar {
    pub fn new(mtm: MainThreadMarker) -> MenuBar {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let target = Target::new(mtm);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        if let Some(b) = item.button(mtm) {
            match ring_image() {
                Some(img) => b.setImage(Some(&img)),
                None => b.setTitle(&NSString::from_str("Glyphspin")),
            }
        }
        item.setMenu(Some(&menu));
        MenuBar { item, _menu: menu, target, mtm }
    }

    pub fn set_tip(&self, tip: &str) {
        if let Some(b) = self.item.button(self.mtm) {
            b.setToolTip(Some(&NSString::from_str(tip)));
        }
    }

    /// Replace `menu`'s items with `items`.
    pub fn fill(&self, menu: &NSMenu, items: &[Item]) {
        menu.removeAllItems();
        for it in items {
            match it {
                Item::Cmd { id, label, check, .. } => {
                    let mi = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.mtm),
                                                                       &NSString::from_str(label), Some(sel!(pick:)),
                                                                       &NSString::from_str(""))
                    };
                    unsafe { mi.setTarget(Some(&self.target)) };
                    mi.setTag(*id as isize);
                    let on = match check {
                        Check::Box(on) | Check::Radio(on) => *on,
                    };
                    mi.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
                    menu.addItem(&mi);
                }
                Item::Note(text) => {
                    let mi = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.mtm),
                                                                       &NSString::from_str(text), None,
                                                                       &NSString::from_str(""))
                    };
                    mi.setEnabled(false);
                    menu.addItem(&mi);
                }
                Item::Sep => menu.addItem(&NSMenuItem::separatorItem(self.mtm)),
                Item::Sub(text, children) => {
                    let sub = NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &NSString::from_str(text));
                    sub.setAutoenablesItems(false);
                    self.fill(&sub, children);
                    let mi = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.mtm),
                                                                       &NSString::from_str(text), None,
                                                                       &NSString::from_str(""))
                    };
                    mi.setSubmenu(Some(&sub));
                    menu.addItem(&mi);
                }
            }
        }
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
