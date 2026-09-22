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
use objc2_app_kit::{NSApplication, NSMenu};
use objc2_foundation::MainThreadMarker;

/// Set by our "About" action; polled/cleared by the controller which then shows
/// the About dialog.
static ABOUT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// True if the native About item has been chosen since the last check (clears
/// the flag). Called from the UI event loop (see app.rs `drain_events`).
pub fn take_about_requested() -> bool {
    ABOUT_REQUESTED.swap(false, Ordering::Relaxed)
}

// A process-lifetime Objective-C object that receives the About action.
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
    }
);

thread_local! {
    /// Our shared action target, created lazily on the main thread.
    static TARGET: Retained<MenuTarget> =
        unsafe { msg_send_id![MenuTarget::alloc(), init] };
}

/// Re-point the app-menu "About" item at our dialog, if muda has (re)built the
/// menu with its own About action. Idempotent and safe to call every UI tick;
/// a no-op once our override is in place, until muda rebuilds the menu again.
pub fn enforce_about_override() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    unsafe {
        let app = NSApplication::sharedApplication(mtm);
        let Some(main_menu): Option<Retained<NSMenu>> = app.mainMenu() else {
            return;
        };
        // The application menu is the first submenu of the main menu.
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
        // muda lays the app menu out as [About, sep, Services, …]; the About
        // item is index 0.
        let Some(about_item) = app_menu.itemAtIndex(0) else {
            return;
        };

        let our_sel: Sel = sel!(tigerAbout:);
        let current: Sel = msg_send![&about_item, action];
        // Already ours? Nothing to do (muda hasn't rebuilt since we patched).
        if current == our_sel {
            return;
        }

        TARGET.with(|target| {
            let target_obj: &AnyObject = target;
            let _: () = msg_send![&about_item, setTarget: target_obj];
            let _: () = msg_send![&about_item, setAction: our_sel];
        });
    }
}
