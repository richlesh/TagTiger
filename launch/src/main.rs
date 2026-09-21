//! macOS launcher shim for TagTiger.
//!
//! winit (used by the GUI) does not forward Finder "Open With" / dock-drop /
//! double-click open events on cold launch. This tiny launcher is the bundle's
//! executable instead: it owns a normal `NSApplication` + delegate that
//! implements `application:openURLs:` / `application:openFile:`, collects the
//! file path(s) the app was launched with, then `exec`s the real GUI
//! (`tagtiger-gui`, sibling in `Contents/MacOS/`) passing those paths as
//! command-line arguments — which the GUI already handles.
//!
//! On non-macOS platforms this is a trivial passthrough so the workspace still
//! builds everywhere (the launcher is only used inside the macOS .app bundle).

#[cfg(not(target_os = "macos"))]
fn main() {
    // Not used off macOS; exec the GUI beside us, forwarding any arguments.
    exec_gui(std::env::args().skip(1).collect());
}

/// Resolve the sibling `tagtiger-gui` and launch it, passing `files` as
/// arguments, then exit. The GUI ships beside this launcher in the bundle's
/// `Contents/MacOS/`.
fn exec_gui(files: Vec<String>) -> ! {
    let gui = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("tagtiger-gui")))
        .unwrap_or_else(|| std::path::PathBuf::from("tagtiger-gui"));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replace this process with the GUI so it inherits the bundle's app
        // identity and dock slot. exec only returns on failure.
        let err = std::process::Command::new(&gui).args(&files).exec();
        eprintln!("tagtiger-launch: failed to exec {}: {err}", gui.display());
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(&gui)
            .args(&files)
            .status()
            .map(|s| s.code().unwrap_or(0))
            .unwrap_or(1);
        std::process::exit(status);
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, ProtocolObject};
    use objc2::{declare_class, mutability, ClassType, DeclaredClass};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSNotification, NSObjectProtocol, NSString, NSURL,
    };

    thread_local! {
        /// File paths collected from open events during launch.
        static COLLECTED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn push_path(p: String) {
        COLLECTED.with(|c| c.borrow_mut().push(p));
    }

    declare_class!(
        struct Delegate;

        unsafe impl ClassType for Delegate {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "TagTigerLaunchDelegate";
        }

        impl DeclaredClass for Delegate {}

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl NSApplicationDelegate for Delegate {
            // - (void)application:(NSApplication*)app openURLs:(NSArray<NSURL*>*)urls
            #[method(application:openURLs:)]
            unsafe fn application_open_urls(
                &self,
                _app: &NSApplication,
                urls: &NSArray<NSURL>,
            ) {
                for i in 0..urls.count() {
                    let url = urls.objectAtIndex(i);
                    if let Some(path) = url.path() {
                        push_path(path.to_string());
                    }
                }
                stop_after_collect();
            }

            // - (BOOL)application:(NSApplication*)app openFile:(NSString*)filename
            #[method(application:openFile:)]
            unsafe fn application_open_file(
                &self,
                _app: &NSApplication,
                filename: &NSString,
            ) -> bool {
                push_path(filename.to_string());
                stop_after_collect();
                true
            }

            // - (void)applicationDidFinishLaunching:(NSNotification*)note
            #[method(applicationDidFinishLaunching:)]
            unsafe fn did_finish_launching(&self, _note: &NSNotification) {
                // If no open event arrived by the time launching finishes, stop
                // the run loop on the next tick so we exec the GUI with no file.
                stop_soon();
            }
        }
    );

    /// Stop the app run loop very soon so `run()` returns and we exec the GUI.
    fn stop_soon() {
        if let Some(mtm) = MainThreadMarker::new() {
            let app = NSApplication::sharedApplication(mtm);
            app.stop(None);
        }
    }

    /// Called from an open-event handler: we already have the file, so stop.
    fn stop_after_collect() {
        stop_soon();
    }

    pub fn run() -> ! {
        let mtm = MainThreadMarker::new().expect("launcher must run on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // Regular app so it can receive open events and own the dock slot.
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

        let delegate: Retained<Delegate> = {
            let this = mtm.alloc::<Delegate>();
            unsafe { objc2::msg_send_id![this, init] }
        };
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        // Run the app; the delegate stops it once launch open events (if any)
        // have been delivered.
        unsafe { app.run() };

        // Collect any files and exec the GUI, forwarding them as arguments.
        let files: Vec<String> = COLLECTED.with(|c| c.borrow().clone());
        // Also forward any command-line file arguments (e.g. `open --args`).
        let mut all = files;
        all.extend(std::env::args().skip(1));
        super::exec_gui(all);
    }
}
