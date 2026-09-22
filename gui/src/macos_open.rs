//! macOS "Open Documents" handling for Finder "Open With", dock-icon drops, and
//! double-clicking an associated file.
//!
//! The windowing backend installs its own `NSApplicationDelegate` but does not
//! implement `application:openURLs:` / `application:openFile:`, so we register
//! our own handler object on the shared `NSAppleEventManager` for the
//! `kCoreEventClass`/`kAEOpenDocuments` (`aevt`/`odoc`) Apple Event. This is
//! last-writer-wins and independent of AppKit's delegate, but must be installed
//! *after* `NSApplication` exists (i.e. once the event loop is running) so it
//! overrides AppKit's default routing. Resolved paths are queued here and
//! drained by the Slint controller's event-loop timer (see app.rs).

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, Sel};
use objc2::{msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

/// Paths delivered by open events (Apple Event *or* a movie dropped on the
/// window), awaiting the UI to pick them up and open them.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Image files dropped on the window, awaiting the UI to set one as the poster.
static PENDING_IMAGES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

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

/// Take any image files dropped on the window since the last call.
pub fn take_pending_images() -> Vec<PathBuf> {
    PENDING_IMAGES
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Native file drag-and-drop
//
// Slint 1.18 only supports in-app drag-and-drop; it does not receive external
// OS file drops (pending upstream in winit). We add the capability on macOS by
// installing a transparent overlay `NSView` over the window's content view that
// is registered for file drags. Its `hitTest:` returns null so normal mouse
// events pass straight through to the Slint view beneath; only drag events are
// delivered to it. Dropped movie files go to `PENDING` (opened), image files to
// `PENDING_IMAGES` (set as the poster) — the same queues the controller drains.
// ---------------------------------------------------------------------------

/// `NSDragOperationCopy`, the operation we advertise for file drops.
const NS_DRAG_OPERATION_COPY: usize = 1;

fn ext_is_movie(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4") | Some("m4v")
    )
}
fn ext_is_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp")
    )
}

/// Read dropped file paths from a dragging-info's pasteboard and queue them.
/// Returns true if at least one usable file was queued.
unsafe fn handle_drop(sender: &AnyObject) -> bool {
    use objc2_app_kit::NSFilenamesPboardType;
    use objc2_foundation::{NSArray, NSString};

    // let pb = [sender draggingPasteboard];
    let pb: *mut AnyObject = msg_send![sender, draggingPasteboard];
    if pb.is_null() {
        return false;
    }
    // let list = [pb propertyListForType: NSFilenamesPboardType]; -> NSArray<NSString>
    let list: *mut NSArray<NSString> = msg_send![pb, propertyListForType: &**NSFilenamesPboardType];
    if list.is_null() {
        return false;
    }
    let list: &NSArray<NSString> = &*list;
    let count = list.count();
    let mut movies: Vec<PathBuf> = Vec::new();
    let mut images: Vec<PathBuf> = Vec::new();
    for i in 0..count {
        let s: Retained<NSString> = list.objectAtIndex(i);
        let path = PathBuf::from(s.to_string());
        if ext_is_movie(&path) {
            movies.push(path);
        } else if ext_is_image(&path) {
            images.push(path);
        }
    }
    // Prefer opening a dropped movie; otherwise treat images as poster art.
    if let Some(movie) = movies.into_iter().next() {
        if let Ok(mut q) = PENDING.lock() {
            q.push(movie);
        }
        return true;
    }
    if !images.is_empty() {
        if let Ok(mut q) = PENDING_IMAGES.lock() {
            q.extend(images);
        }
        return true;
    }
    false
}

// A transparent overlay view that accepts file drags. `hitTest:` returns null
// so it never intercepts mouse events destined for the Slint view below.
objc2::declare_class!(
    struct DropView;

    unsafe impl ClassType for DropView {
        type Super = objc2_app_kit::NSView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "TagTigerDropView";
    }

    impl DeclaredClass for DropView {}

    unsafe impl DropView {
        // Transparent to mouse events.
        #[method(hitTest:)]
        unsafe fn hit_test(&self, _point: objc2_foundation::NSPoint) -> *mut AnyObject {
            std::ptr::null_mut()
        }

        // Advertise a copy operation while a drag hovers.
        #[method(draggingEntered:)]
        unsafe fn dragging_entered(&self, _sender: &AnyObject) -> usize {
            NS_DRAG_OPERATION_COPY
        }

        #[method(draggingUpdated:)]
        unsafe fn dragging_updated(&self, _sender: &AnyObject) -> usize {
            NS_DRAG_OPERATION_COPY
        }

        // Accept the drop before it happens.
        #[method(prepareForDragOperation:)]
        unsafe fn prepare_for_drag_operation(&self, _sender: &AnyObject) -> Bool {
            Bool::YES
        }

        // Read the dropped files and queue them.
        #[method(performDragOperation:)]
        unsafe fn perform_drag_operation(&self, sender: &AnyObject) -> Bool {
            Bool::new(handle_drop(sender))
        }
    }
);

/// Install the native file-drop overlay on the window that owns `ns_view`
/// (the Slint content view, obtained via its raw window handle). Adds a
/// transparent, auto-resizing `DropView` as a sibling covering the content
/// view and registers it for file drags. Safe to call once after the window
/// exists; a no-op if the view/window can't be resolved.
///
/// # Safety
/// `ns_view` must be a valid `NSView*` for the app's window, called on the
/// main thread.
pub unsafe fn install_drag_drop(ns_view: *mut std::ffi::c_void) {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSFilenamesPboardType, NSView};
    use objc2_foundation::MainThreadMarker;

    if ns_view.is_null() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let view: &NSView = &*(ns_view as *const NSView);
    // The content view is the drop target's superview; add the overlay there so
    // it tracks the full window content and sits above the Slint view.
    let content: *mut NSView = msg_send![view, superview];
    let (parent, bounds_src): (&NSView, &NSView) = if content.is_null() {
        (view, view)
    } else {
        (&*content, &*content)
    };

    let frame = bounds_src.bounds();
    let overlay: Retained<DropView> = {
        let alloc = mtm.alloc::<DropView>();
        msg_send_id![alloc, initWithFrame: frame]
    };

    // Auto-resize with the parent (width + height sizable).
    let _: () = msg_send![&overlay, setAutoresizingMask: 2usize | 16usize];

    // Register for filename drags. Build the single-element type array via
    // msg_send to sidestep NSArray::from_slice's element-type constraints on
    // the NSPasteboardType (NSString) constant.
    let filenames_type: &AnyObject = &**NSFilenamesPboardType as &AnyObject;
    let types: *mut AnyObject = msg_send![
        objc2::class!(NSArray),
        arrayWithObject: filenames_type
    ];
    let _: () = msg_send![&overlay, registerForDraggedTypes: types];

    // Add above existing subviews.
    let _: () = msg_send![parent, addSubview: &*overlay];

    // Keep the overlay alive for the process lifetime.
    std::mem::forget(overlay);
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
