//! macOS "Open Documents" handling for Finder "Open With", dock-icon drops, and
//! double-clicking an associated file.
//!
//! winit (0.30) installs its own `NSApplicationDelegate` but implements neither
//! `application:openURLs:` nor `application:openFile:`, and adding those methods
//! to winit's delegate class after the fact is not picked up by AppKit. So we
//! register our own handler object on the shared `NSAppleEventManager` for the
//! `kCoreEventClass`/`kAEOpenDocuments` (`aevt`/`odoc`) Apple Event. This is
//! last-writer-wins and independent of AppKit's delegate, but must be installed
//! *after* `NSApplication` exists (i.e. once the winit event loop is running) so
//! it overrides AppKit's default routing. Resolved paths are queued and drained
//! by the egui update loop each frame.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

/// Paths delivered by open events, awaiting the UI to pick them up.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Four-char-code helpers for the open-documents Apple Event.
const fn fourcc(t: &[u8; 4]) -> u32 {
    ((t[0] as u32) << 24) | ((t[1] as u32) << 16) | ((t[2] as u32) << 8) | (t[3] as u32)
}
const K_CORE_EVENT_CLASS: u32 = fourcc(b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = fourcc(b"odoc");
const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");

// A small Objective-C object carrying the Apple Event handler method.
objc2::declare_class!(
    struct OpenDocHandler;

    unsafe impl ClassType for OpenDocHandler {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "TagTigerOpenDocHandler";
    }

    impl DeclaredClass for OpenDocHandler {}

    unsafe impl OpenDocHandler {
        #[method(handleOpenDocuments:withReplyEvent:)]
        unsafe fn handle_open_documents(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            handle_event(event);
        }
    }
);

/// Extract file paths from the `----` direct object (an AEList of file URLs).
fn handle_event(event: &NSAppleEventDescriptor) {
    unsafe {
        let list: Option<Retained<NSAppleEventDescriptor>> =
            msg_send_id![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        let Some(list) = list else {
            return;
        };
        let count = list.numberOfItems();
        let mut paths = Vec::new();
        for i in 1..=count {
            let Some(item) = list.descriptorAtIndex(i) else {
                continue;
            };
            // fileURLValue is the documented accessor for a file-URL item.
            if let Some(url) = item.fileURLValue() {
                if let Some(path) = url.path() {
                    paths.push(PathBuf::from(path.to_string()));
                    continue;
                }
            }
            // Fallback: some senders provide the path/URL as a string.
            if let Some(s) = item.stringValue() {
                if let Some(p) = uri_or_path(&s.to_string()) {
                    paths.push(p);
                }
            }
        }
        if !paths.is_empty() {
            if let Ok(mut q) = PENDING.lock() {
                q.extend(paths);
            }
        }
    }
}

/// Convert a `file://` URL or plain path into a `PathBuf`.
fn uri_or_path(s: &str) -> Option<PathBuf> {
    if let Some(rest) = s.strip_prefix("file://") {
        let rest = match rest.find('/') {
            Some(i) => &rest[i..],
            None => rest,
        };
        Some(PathBuf::from(percent_decode(rest)))
    } else if s.starts_with('/') {
        Some(PathBuf::from(s))
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Install the Apple Event handler. Registers (or re-registers) our
/// open-documents handler on the shared NSAppleEventManager.
pub fn install() {
    unsafe {
        let handler: Retained<OpenDocHandler> = msg_send_id![OpenDocHandler::alloc(), init];
        let manager = NSAppleEventManager::sharedAppleEventManager();
        let sel: Sel = sel!(handleOpenDocuments:withReplyEvent:);
        let target: &AnyObject = &handler;
        let _: () = msg_send![
            &manager,
            setEventHandler: target,
            andSelector: sel,
            forEventClass: K_CORE_EVENT_CLASS,
            andEventID: K_AE_OPEN_DOCUMENTS,
        ];
        std::mem::forget(handler);
    }
}

/// Take any file paths delivered since the last call (drains the queue).
pub fn take_pending() -> Vec<PathBuf> {
    PENDING
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Write PNG-encoded image bytes to the general pasteboard as a standard
/// `public.png` (`NSPasteboardTypePNG`) item, so other macOS apps recognize it.
/// `arboard` writes only TIFF, which some apps don't surface.
pub fn write_pasteboard_png(png_bytes: &[u8]) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::NSData;
    unsafe {
        let data = NSData::with_bytes(png_bytes);
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ok: bool = msg_send![&pb, setData: &*data, forType: NSPasteboardTypePNG];
        ok
    }
}
