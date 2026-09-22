//! macOS application-menu integration.
//!
//! On macOS the bold, leftmost menu — the *application menu* — is created and
//! owned by Slint's winit backend via the `muda` crate, not by our Slint
//! `MenuBar`. Two consequences we care about:
//!
//! 1. **Title.** muda titles the app menu (and its "About …"/"Hide …"/"Quit …"
//!    items) from `NSRunningApplication.localizedName`, i.e. the bundle's
//!    `CFBundleName`. Inside the shipped, notarized `TagTiger.app` that is
//!    already "TagTiger" (see `packaging/macos_bundle.sh`), so the menu reads
//!    "TagTiger". Only an *unbundled* `cargo run` shows the raw executable name
//!    ("tagtiger-gui"); that is a dev-only cosmetic and not settable without
//!    private APIs, so we leave it.
//!
//! 2. **About action.** muda wires the app-menu "About" item to AppKit's
//!    generic `orderFrontStandardAboutPanel:`. We want it to open *our* About
//!    dialog instead. muda rebuilds the whole menu whenever a menu property
//!    changes (e.g. our Undo/Redo `enabled` bindings), so a one-shot patch is
//!    wiped out. Instead `enforce_about_override` runs each UI tick and, only
//!    when needed, re-points the app-menu About item's target/action at our
//!    handler. It's idempotent and cheap (a couple of ObjC message sends, and
//!    only a mutation when muda has just rebuilt the menu).
//!
//! The click sets an atomic flag drained by the controller (`take_about_requested`),
//! which keeps this module free of any `!Send` Slint handles — the menu action
//! and the drain both run on the main thread.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};

/// Set by our "About" action; polled/cleared by the controller which then shows
/// the About dialog.
static ABOUT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set by our "Settings…" app-menu item (macOS puts Settings in the app menu).
static SETTINGS_REQUESTED: AtomicBool = AtomicBool::new(false);

/// True if the native About item has been chosen since the last check (clears
/// the flag). Called from the UI event loop (see app.rs `drain_events`).
pub fn take_about_requested() -> bool {
    ABOUT_REQUESTED.swap(false, Ordering::Relaxed)
}

/// True if the app-menu Settings item has been chosen since the last check.
pub fn take_settings_requested() -> bool {
    SETTINGS_REQUESTED.swap(false, Ordering::Relaxed)
}

// A process-lifetime Objective-C object that receives the About/Settings actions.
objc2::declare_class!(
    struct MenuTarget;

    unsafe impl ClassType for MenuTarget {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "TagTigerMenuTarget";
    }

    impl DeclaredClass for MenuTarget {}

    unsafe impl MenuTarget {
        #[method(tigerAbout:)]
        unsafe fn tiger_about(&self, _sender: *mut AnyObject) {
            ABOUT_REQUESTED.store(true, Ordering::Relaxed);
        }

        #[method(tigerSettings:)]
        unsafe fn tiger_settings(&self, _sender: *mut AnyObject) {
            SETTINGS_REQUESTED.store(true, Ordering::Relaxed);
        }
    }
);

thread_local! {
    /// Our shared action target, created lazily on the main thread.
    static TARGET: Retained<MenuTarget> =
        unsafe { msg_send_id![MenuTarget::alloc(), init] };
}

/// Keep the app-menu customizations in place (idempotent, safe every UI tick):
///   1. Re-point the "About" item at our dialog.
///   2. Insert a "Settings…" item (Cmd+,) right after About, per macOS
///      convention. muda rebuilds the menu on property changes, so we re-apply
///      whenever our items are missing.
pub fn enforce_app_menu() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let Some(main_menu): Option<Retained<NSMenu>> = app.mainMenu() else {
            return;
        };
        if main_menu.numberOfItems() == 0 {
            return;
        }
        let Some(app_item) = main_menu.itemAtIndex(0) else {
            return;
        };
        let Some(app_menu): Option<Retained<NSMenu>> = app_item.submenu() else {
            return;
        };
        if app_menu.numberOfItems() == 0 {
            return;
        }
        // muda lays the app menu out as [About, sep, Services, …].
        let Some(about_item) = app_menu.itemAtIndex(0) else {
            return;
        };

        let about_sel: Sel = sel!(tigerAbout:);
        let settings_sel: Sel = sel!(tigerSettings:);

        let about_action: Sel = msg_send![&about_item, action];
        let about_ok = about_action == about_sel;

        // Is our Settings item already present at index 1?
        let settings_present = if app_menu.numberOfItems() > 1 {
            if let Some(it) = app_menu.itemAtIndex(1) {
                let a: Sel = msg_send![&it, action];
                a == settings_sel
            } else {
                false
            }
        } else {
            false
        };

        if about_ok && settings_present {
            return; // Nothing to do since muda's last rebuild.
        }

        TARGET.with(|target| {
            let target_obj: &AnyObject = target;

            if !about_ok {
                let _: () = msg_send![&about_item, setTarget: target_obj];
                let _: () = msg_send![&about_item, setAction: about_sel];
            }

            if !settings_present {
                // Build "Settings…" (Cmd+,) and insert it right after About,
                // followed by a separator, matching macOS conventions.
                let title = NSString::from_str("Settings…");
                let key = NSString::from_str(",");
                let item: Retained<NSMenuItem> = {
                    let alloc = mtm.alloc::<NSMenuItem>();
                    msg_send_id![
                        alloc,
                        initWithTitle: &*title,
                        action: settings_sel,
                        keyEquivalent: &*key,
                    ]
                };
                let _: () = msg_send![&item, setTarget: target_obj];
                app_menu.insertItem_atIndex(&item, 1);
                let sep = NSMenuItem::separatorItem(mtm);
                app_menu.insertItem_atIndex(&sep, 2);
            }
        });
    }
}
