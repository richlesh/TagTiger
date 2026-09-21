//! macOS "Open Documents" Apple Event handling.
//!
//! winit (0.30) does not forward the `application:openURLs:` /
//! `application:openFile:` delegate callbacks that fire when a file is opened
//! via Finder's "Open With", a dock-icon drop, or a double-click on an
//! associated file. Those arrive as a `kCoreEventClass` / `kAEOpenDocuments`
//! (`aevt`/`odoc`) Apple Event, not as command-line arguments.
//!
//! We install our own handler on the shared `NSAppleEventManager` for that
//! event *before* the winit event loop starts, so AppKit delivers both the
//! cold-launch event (queued until a handler exists) and later events while the
//! app is running. Resolved file paths are pushed into a global queue that the
//! egui update loop drains each frame.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

/// Paths delivered by "Open Documents" Apple Events, awaiting the UI to pick
/// them up. Drained by [`take_pending`] each frame.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Apple Event constants (four-char codes) for the open-documents event.
const K_CORE_EVENT_CLASS: u32 = fourcc(b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = fourcc(b"odoc");
/// The `keyDirectObject` ('----') keyword holding the event's direct object.
const KEY_DIRECT_OBJECT: u32 = fourcc(b"----");

/// Compute a big-endian four-character code from a 4-byte tag.
const fn fourcc(tag: &[u8; 4]) -> u32 {
    ((tag[0] as u32) << 24) | ((tag[1] as u32) << 16) | ((tag[2] as u32) << 8) | (tag[3] as u32)
}

// A minimal Objective-C object whose sole job is to carry the Apple Event
// handler method. `NSAppleEventManager` invokes it via target+selector.
objc2::declare_class!(
    struct OpenDocHandler;

    unsafe impl ClassType for OpenDocHandler {
        type Super = objc2::runtime::NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "TagTigerOpenDocHandler";
    }

    impl DeclaredClass for OpenDocHandler {}

    unsafe impl OpenDocHandler {
        // - (void)handleOpenDocuments:(NSAppleEventDescriptor*)event
        //                withReplyEvent:(NSAppleEventDescriptor*)reply;
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

/// Extract file paths from the direct object of an `odoc` Apple Event and queue
/// them. The direct object is an AEList of file URLs / paths.
fn handle_event(event: &NSAppleEventDescriptor) {
    unsafe {
        // Fetch the '----' direct-object parameter (an AEList of items). This
        // method isn't in objc2-foundation 0.2.2's bindings, so call it raw.
        let list: Option<Retained<NSAppleEventDescriptor>> =
            msg_send_id![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
        let Some(list) = list else {
            return;
        };
        let count = list.numberOfItems();
        let mut paths = Vec::new();
        // AE lists are 1-indexed.
        for i in 1..=count {
            let Some(item) = list.descriptorAtIndex(i) else {
                continue;
            };
            // Coerce each item to a file-URL descriptor and read its string.
            let furl: Option<Retained<NSAppleEventDescriptor>> =
                msg_send_id![&item, coerceToDescriptorType: fourcc(b"furl")];
            if let Some(url_desc) = furl {
                if let Some(s) = url_desc.stringValue() {
                    if let Some(p) = uri_or_path_to_pathbuf(&s.to_string()) {
                        paths.push(p);
                    }
                }
            } else if let Some(s) = item.stringValue() {
                if let Some(p) = uri_or_path_to_pathbuf(&s.to_string()) {
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

/// Turn a `file://` URL or a plain path string into a `PathBuf`.
fn uri_or_path_to_pathbuf(s: &str) -> Option<PathBuf> {
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

/// Minimal percent-decoding (e.g. `%20` -> space) for `file://` URLs.
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

/// Install the Apple Event handler. Call once, early, before the winit loop.
pub fn install() {
    unsafe {
        let handler: Retained<OpenDocHandler> = msg_send_id![OpenDocHandler::alloc(), init];
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // Register handleOpenDocuments:withReplyEvent: for aevt/odoc.
        let sel: Sel = sel!(handleOpenDocuments:withReplyEvent:);
        let target: &AnyObject = &handler;
        let _: () = msg_send![
            &manager,
            setEventHandler: target,
            andSelector: sel,
            forEventClass: K_CORE_EVENT_CLASS,
            andEventID: K_AE_OPEN_DOCUMENTS,
        ];
        // The manager holds only a weak reference to the handler; keep it alive
        // for the process lifetime by leaking our strong reference. (Retained
        // isn't Send/Sync, so it can't live in a static; leaking is simplest.)
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
/// `public.png` (`NSPasteboardTypePNG`) item, so other macOS apps (Preview,
/// Finder, browsers) recognize it. `arboard` writes only TIFF, which some apps
/// don't surface; this writes the widely-recognized PNG type.
///
/// Returns `false` if the pasteboard write failed.
pub fn write_pasteboard_png(png_bytes: &[u8]) -> bool {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG};
    use objc2_foundation::NSData;
    unsafe {
        let data = NSData::with_bytes(png_bytes);
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        // setData:forType: expects the raw bytes for the given UTI type.
        let ok: bool = msg_send![&pb, setData: &*data, forType: NSPasteboardTypePNG];
        ok
    }
}
