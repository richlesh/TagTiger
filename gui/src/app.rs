//! Slint desktop app for TagTiger — window bootstrap + controller.
//!
//! The `.slint` UI (see ui/app.slint) owns the widgets and holds the editable
//! field values / locks as two-way-bound properties. This module hosts the
//! controller: it bridges those properties and the UI callbacks to the
//! background [`Worker`] channel, and ports the editor logic from the former
//! egui frontend — worker-event handling, undo/redo, load/collect metadata,
//! poster clipboard/drag-drop, search, and write.
//!
//! Dialogs (About, Splash, License, Settings, no-credential, lightbox, save
//! progress/complete) are layered on in step 4; platform bits (window icon,
//! macOS open-file drain, CLI install) in step 5.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use slint::{Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};
use tagtiger_core::model::{Definition, MediaMetadata, ProviderId, SearchResult, VideoKind};

use crate::worker::{Event, Request, Worker};

// The Slint-compiled UI (from ui/app.slint). Generates `MainWindow`,
// `MatchItem`, `PosterItem`.
slint::include_modules!();

/// A restorable snapshot of the user-editable state, used for undo/redo.
/// Field text lives in Slint properties; a snapshot copies it out so it can be
/// restored later.
#[derive(Clone, PartialEq)]
struct Snapshot {
    title: String,
    year: String,
    video_kind: Option<VideoKind>,
    definition: Option<Definition>,
    rating: String,
    summary: String,
    overview: String,
    genres: String,
    cast: String,
    directors: String,
    producers: String,
    writers: String,
    studio: String,
    cover_bytes: Option<Vec<u8>>,
    cover_size: Option<(u32, u32)>,
}

/// An empty snapshot for initializing the committed baseline before any file
/// is loaded.
fn empty_snapshot() -> Snapshot {
    Snapshot {
        title: String::new(),
        year: String::new(),
        video_kind: None,
        definition: None,
        rating: String::new(),
        summary: String::new(),
        overview: String::new(),
        genres: String::new(),
        cast: String::new(),
        directors: String::new(),
        producers: String::new(),
        writers: String::new(),
        studio: String::new(),
        cover_bytes: None,
        cover_size: None,
    }
}

/// What the lightbox is currently showing (used in step 4).
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum Lightbox {
    Tmdb(usize),
    CurrentCover,
}

/// Controller state shared between the UI callbacks and the event-drain timer.
/// Mirrors the former egui `App` fields, minus the egui textures (Slint images
/// are pushed straight into properties/models instead).
struct Controller {
    worker: Worker,

    file: Option<PathBuf>,
    file_loaded: bool,
    loading_details: bool,

    /// Working metadata: prefilled from the file, updated when a match loads.
    meta: Option<MediaMetadata>,
    /// TMDB search results backing the Matches list.
    results: Vec<SearchResult>,

    /// Encoded bytes (PNG/JPEG) of the current cover — the single source of
    /// truth for the poster. Written to the file when tagging.
    cover_bytes: Option<Vec<u8>>,
    cover_size: Option<(u32, u32)>,

    /// Undo/redo history of snapshots.
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// A pending "before" snapshot captured when a text edit begins, committed
    /// once the edit settles (coalesces a run of keystrokes into one step).
    text_edit_pending: Option<Snapshot>,
    /// The last stable editable state. Because Slint two-way bindings update a
    /// property *before* its change callback runs, we can't read the "before"
    /// value from the widget; this baseline holds it so dropdown/discrete
    /// changes remain undoable. Refreshed after every committed change.
    committed: Snapshot,

    /// Detected video track dimensions (width, height) of the opened file.
    video_dimensions: Option<(u32, u32)>,
    /// Whether to save the file as fast-start (moov-first).
    edit_fast_start: bool,

    /// Whether a thumbnail fetch has already been requested (lazy loading).
    thumb_requested: Vec<bool>,

    /// Persisted settings (license + tag counter + token).
    settings: crate::license_mgr::Settings,

    /// True while a save is a shift/rewrite (vs in-place), for the message.
    write_is_shift: bool,
    writing: bool,

    /// Backing model for the poster grid (kept so thumbs can be updated in
    /// place as they arrive).
    posters: Rc<VecModel<PosterItem>>,
    /// Lightbox target when the enlarged viewer is open.
    lightbox: Option<Lightbox>,
    /// Full-resolution artwork images for the lightbox, keyed by artwork index.
    full_images: Vec<Option<slint::Image>>,
    /// Whether a full-image fetch has already been requested, keyed by index.
    full_requested: Vec<bool>,
    /// Deferred donation splash: set on an unlicensed every-5th write, shown
    /// once the "Update complete" dialog is dismissed.
    splash_pending: bool,
    /// Theme (dark?) when the Settings dialog was opened, so Cancel can revert a
    /// live theme preview.
    theme_before_settings: bool,
}

/// Build the window, wire up the worker + callbacks, and run the event loop.
pub fn run() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;

    // macOS: (re)install the Open-Documents Apple Event handler now that the
    // backend has created NSApplication, so it wins over AppKit's default
    // routing. (main.rs also calls install() early; this is the effective one.)
    #[cfg(target_os = "macos")]
    crate::macos_open::install();

    let settings = crate::license_mgr::Settings::load();

    // The worker wakes the UI thread from its own thread after each event; a
    // Slint event-loop timer polls the channel (see below), so the repaint hook
    // just nudges the loop awake.
    let worker = Worker::spawn(settings.tmdb_bearer_token.clone(), || {
        let _ = slint::invoke_from_event_loop(|| {});
    });

    // Launch-file argument (Finder "Open With", `open -a`, a path/`file://` URI
    // from the Linux .desktop `%U`).
    if let Some(arg) = std::env::args_os().nth(1) {
        if let Some(path) = arg_to_movie_path(&arg) {
            let _ = worker.tx.send(Request::OpenFile { path });
        }
    }

    // Platform-conditional Help menu item.
    window.set_show_install_cli(cfg!(target_os = "macos"));
    window.set_is_macos(cfg!(target_os = "macos"));

    let posters: Rc<VecModel<PosterItem>> = Rc::new(VecModel::default());
    window.set_posters(ModelRc::from(posters.clone()));

    let ctrl = Rc::new(RefCell::new(Controller {
        worker,
        file: None,
        file_loaded: false,
        loading_details: false,
        meta: None,
        results: Vec::new(),
        cover_bytes: None,
        cover_size: None,
        undo_stack: Vec::new(),
        redo_stack: Vec::new(),
        text_edit_pending: None,
        committed: empty_snapshot(),
        video_dimensions: None,
        edit_fast_start: false,
        thumb_requested: Vec::new(),
        settings,
        write_is_shift: false,
        writing: false,
        posters,
        lightbox: None,
        full_images: Vec::new(),
        full_requested: Vec::new(),
        splash_pending: false,
        theme_before_settings: true,
    }));

    // App version + icon for the dialogs.
    window.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    if let Some(icon) = load_app_icon() {
        window.set_app_icon(icon.clone());
        window.set_window_icon(icon);
    }
    // License state drives the About "thank you" and the startup splash.
    let licensed = ctrl.borrow().settings.is_licensed();
    window.set_about_licensed(licensed);

    // Theme: apply the persisted Light/Dark setting to the window.
    let dark = ctrl.borrow().settings.theme_is_dark();
    window.set_theme_dark(dark);
    window.set_settings_theme_index(if dark { 1 } else { 0 });

    // Match the app's highlight/accent colors to the user's OS setting (macOS:
    // the chosen accent + selection colors). Falls back to Slint's Palette
    // defaults when unavailable.
    apply_system_colors(&window);

    wire_callbacks(&window, &ctrl);

    // Startup splash for unlicensed users (auto-dismisses after 20s via a
    // one-shot timer; also dismissable by clicking the scrim/Close).
    let splash_timer = slint::Timer::default();
    if !licensed {
        window.set_show_splash(true);
        let handle = window.as_weak();
        splash_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(20),
            move || {
                if let Some(w) = handle.upgrade() {
                    w.set_show_splash(false);
                }
            },
        );
    }

    // macOS: install the native file drag-and-drop overlay once the native
    // window (NSView) exists. Slint creates the surface lazily, so defer with a
    // short one-shot timer after the loop starts.
    #[cfg(target_os = "macos")]
    let dnd_timer = slint::Timer::default();
    #[cfg(target_os = "macos")]
    {
        let handle = window.as_weak();
        dnd_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(200),
            move || {
                if let Some(w) = handle.upgrade() {
                    install_native_drag_drop(&w);
                }
            },
        );
    }

    // Poll the worker's event channel on the UI thread.
    let drain_timer = slint::Timer::default();
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        drain_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(30),
            move || {
                if let Some(w) = handle.upgrade() {
                    drain_events(&ctrl, &w);
                }
            },
        );
    }

    window.run()
}

/// Register every UI callback against the controller.
fn wire_callbacks(window: &MainWindow, ctrl: &Rc<RefCell<Controller>>) {
    // --- File / app ---
    {
        let ctrl = ctrl.clone();
        window.on_open_file(move || {
            let c = ctrl.borrow();
            let _ = c.worker.tx.send(Request::PickFile);
        });
    }
    window.on_open_settings({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                open_settings_dialog(&ctrl, &w);
            }
        }
    });
    window.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    window.on_open_license({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                open_license_dialog(&ctrl, &w);
            }
        }
    });
    window.on_install_cli({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                install_cli(&ctrl, &w);
            }
        }
    });
    window.on_open_about({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_about(true);
            }
        }
    });

    // --- Dialog callbacks ---
    window.on_open_url(|url| {
        let _ = webbrowser_open(&url);
    });
    window.on_about_ok({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_about(false);
            }
        }
    });
    window.on_splash_dismiss({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_splash(false);
            }
        }
    });
    window.on_license_cancel({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_license(false);
            }
        }
    });
    window.on_license_key_edited({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Reformat the key and recompute validity live.
                let formatted = crate::license_mgr::format_key(&w.get_license_key());
                if formatted != w.get_license_key().as_str() {
                    w.set_license_key(SharedString::from(formatted.clone()));
                }
                let valid = crate::license_mgr::is_valid(&formatted, &w.get_license_email());
                w.set_license_valid(valid);
            }
        }
    });
    window.on_license_save({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                license_save(&ctrl, &w);
            }
        }
    });
    window.on_settings_cancel({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                // Revert any live theme preview to what it was on open.
                let prev = ctrl.borrow().theme_before_settings;
                w.set_theme_dark(prev);
                w.set_settings_theme_index(if prev { 1 } else { 0 });
                w.set_show_settings(false);
            }
        }
    });
    window.on_settings_save({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                settings_save(&ctrl, &w);
            }
        }
    });
    window.on_theme_changed({
        let handle = window.as_weak();
        move |idx| {
            if let Some(w) = handle.upgrade() {
                // 0 = Light, 1 = Dark. Apply immediately for a live preview.
                w.set_theme_dark(idx != 0);
            }
        }
    });
    window.on_no_cred_ok({
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_no_credential(false);
            }
        }
    });
    window.on_no_cred_open_settings({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_no_credential(false);
                open_settings_dialog(&ctrl, &w);
            }
        }
    });
    window.on_complete_ok({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                w.set_show_complete(false);
                // Show a deferred donation splash now the dialog is dismissed.
                if ctrl.borrow().splash_pending {
                    ctrl.borrow_mut().splash_pending = false;
                    w.set_show_splash(true);
                }
            }
        }
    });
    window.on_lightbox_close({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                ctrl.borrow_mut().lightbox = None;
                w.set_show_lightbox(false);
            }
        }
    });

    // --- Edit menu ---
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_undo(move || {
            if let Some(w) = handle.upgrade() {
                undo(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_redo(move || {
            if let Some(w) = handle.upgrade() {
                redo(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_cut(move || {
            if let Some(w) = handle.upgrade() {
                do_cut(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_copy(move || {
            if let Some(w) = handle.upgrade() {
                do_copy(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_paste(move || {
            if let Some(w) = handle.upgrade() {
                do_paste(&ctrl, &w);
            }
        });
    }

    // --- Search ---
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_do_search(move || {
            if let Some(w) = handle.upgrade() {
                start_search(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_select_match(move |idx| {
            if let Some(w) = handle.upgrade() {
                select_match(&ctrl, &w, idx);
            }
        });
    }

    // --- Field edits (coalesced undo) ---
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_field_edited(move || {
            if let Some(w) = handle.upgrade() {
                on_field_edited(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_field_commit(move || {
            if let Some(w) = handle.upgrade() {
                commit_text_edit(&ctrl, &w);
                update_undo_redo(&ctrl, &w);
            }
        });
    }

    // --- Dropdowns / fast-start (each is a discrete, undoable change) ---
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_video_kind_changed(move |_idx| {
            if let Some(w) = handle.upgrade() {
                discrete_change(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_definition_changed(move |_idx| {
            if let Some(w) = handle.upgrade() {
                discrete_change(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_rating_changed(move |_idx| {
            if let Some(w) = handle.upgrade() {
                discrete_change(&ctrl, &w);
            }
        });
    }
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_fast_start_changed(move |on| {
            ctrl.borrow_mut().edit_fast_start = on;
            if let Some(w) = handle.upgrade() {
                let _ = w; // fast-start isn't part of the undo snapshot
            }
        });
    }

    // --- Current-cover poster interactions ---
    {
        let handle = window.as_weak();
        window.on_poster_clicked(move || {
            if let Some(w) = handle.upgrade() {
                // Toggle selection of the current poster (enables Cut/Copy) and
                // clear any grid-tile selection.
                w.set_poster_selected(!w.get_poster_selected());
                w.set_selected_poster_index(-1);
            }
        });
    }
    {
        // A text field gained focus: drop any poster selection so clipboard
        // focus (and the highlight) moves to the field.
        let handle = window.as_weak();
        window.on_clear_poster_selection(move || {
            if let Some(w) = handle.upgrade() {
                if w.get_poster_selected() {
                    w.set_poster_selected(false);
                }
                if w.get_selected_poster_index() >= 0 {
                    w.set_selected_poster_index(-1);
                }
            }
        });
    }
    window.on_poster_double_clicked({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move || {
            if let Some(w) = handle.upgrade() {
                open_current_cover_lightbox(&ctrl, &w);
            }
        }
    });

    // --- TMDB poster grid ---
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_poster_choice_clicked(move |idx| {
            if let Some(w) = handle.upgrade() {
                poster_choice_clicked(&ctrl, &w, idx);
            }
        });
    }
    window.on_poster_choice_double_clicked({
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        move |idx| {
            if let Some(w) = handle.upgrade() {
                open_tmdb_lightbox(&ctrl, &w, idx);
            }
        }
    });
    window.on_poster_rows_visible({
        let ctrl = ctrl.clone();
        move |first_row, last_row, columns| {
            request_visible_thumbs(&ctrl, first_row, last_row, columns);
        }
    });
    {
        let ctrl = ctrl.clone();
        let handle = window.as_weak();
        window.on_write_tags(move || {
            if let Some(w) = handle.upgrade() {
                write_tags(&ctrl, &w);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Worker event draining
// ---------------------------------------------------------------------------

fn drain_events(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    // macOS: pick up any files delivered via the "Open Documents" Apple Event
    // (Finder "Open With", dock drops, double-click) that aren't surfaced as a
    // launch argument. Drained here since this runs on the UI thread every tick.
    #[cfg(target_os = "macos")]
    {
        // Keep the native app-menu customizations in place (muda rebuilds the
        // menu on property changes): About -> our dialog, plus a Settings item.
        crate::macos_menu::enforce_app_menu();
        if crate::macos_menu::take_about_requested() {
            w.set_show_about(true);
        }
        if crate::macos_menu::take_settings_requested() {
            open_settings_dialog(ctrl, w);
        }
        for path in crate::macos_open::take_pending() {
            if is_movie_path(&path) && path.exists() {
                let c = ctrl.borrow();
                let _ = c.worker.tx.send(Request::OpenFile { path });
                drop(c);
                w.set_status(SharedString::from("Opening file…"));
            }
        }
        // Image files dropped on the window replace the current poster (unless
        // locked / no file open), mirroring the egui drop behavior.
        for path in crate::macos_open::take_pending_images() {
            if !w.get_file_loaded() || w.get_lock_poster() {
                continue;
            }
            if is_image_path(&path) {
                if let Ok(bytes) = std::fs::read(&path) {
                    set_cover_from_bytes_undoable(ctrl, w, bytes);
                    w.set_status(SharedString::from("Poster replaced from dropped image."));
                }
            }
        }
    }

    loop {
        let evt = {
            let c = ctrl.borrow();
            match c.worker.rx.try_recv() {
                Ok(e) => e,
                Err(_) => break,
            }
        };
        match evt {
            Event::FileLoaded {
                file,
                meta,
                suggested_query,
                cover,
                cover_size,
                cover_bytes,
                video_dimensions,
                fast_start,
            } => {
                {
                    let mut c = ctrl.borrow_mut();
                    c.file = Some(file);
                    c.file_loaded = true;
                    c.loading_details = false;
                    c.results = Vec::new();
                    c.cover_bytes = cover_bytes;
                    c.cover_size = cover_size;
                    c.video_dimensions = video_dimensions;
                    c.edit_fast_start = fast_start;
                    c.undo_stack.clear();
                    c.redo_stack.clear();
                    c.text_edit_pending = None;
                }
                w.set_file_loaded(true);
                w.set_loading_details(false);
                w.set_poster_selected(false);
                w.set_selected_poster_index(-1);
                w.set_selected_match_index(-1);
                w.set_search_query(SharedString::from(suggested_query));
                w.set_fast_start(fast_start);
                set_matches(ctrl, w);
                // Current cover display + size caption.
                match cover {
                    Some((cw, ch, rgba)) => {
                        w.set_cover_image(rgba_to_image(cw, ch, &rgba));
                        w.set_has_cover(true);
                    }
                    None => w.set_has_cover(false),
                }
                w.set_video_dimensions(SharedString::from(dims_caption(video_dimensions)));
                w.set_cover_size_caption(SharedString::from(size_caption(cover_size)));
                // Prefill editable fields (all fields — respect_locks=false).
                load_meta(ctrl, w, *meta, false);
                w.set_status(SharedString::from(
                    "Edit fields, or search TMDB to fetch metadata.",
                ));
            }
            Event::CoverSet {
                width,
                height,
                rgba,
                orig_size,
                bytes,
            } => {
                // Setting a poster is an undoable action.
                let before = snapshot(ctrl, w);
                push_undo(ctrl, w, before);
                {
                    let mut c = ctrl.borrow_mut();
                    c.cover_size = Some(orig_size);
                    c.cover_bytes = Some(bytes);
                }
                w.set_cover_image(rgba_to_image(width, height, &rgba));
                w.set_has_cover(true);
                w.set_cover_size_caption(SharedString::from(size_caption(Some(orig_size))));
                w.set_status(SharedString::from("Poster set as current."));
                update_undo_redo(ctrl, w);
            }
            Event::CopyToClipboard { bytes } => {
                let msg = if write_clipboard_image(&bytes).is_ok() {
                    "Poster copied to clipboard."
                } else {
                    "Failed to copy poster."
                };
                w.set_status(SharedString::from(msg));
            }
            Event::SearchDone { results } => {
                {
                    let mut c = ctrl.borrow_mut();
                    c.results = results;
                }
                let n = ctrl.borrow().results.len();
                w.set_status(SharedString::from(format!("{n} match(es).")));
                w.set_selected_match_index(-1);
                set_matches(ctrl, w);
            }
            Event::DetailsDone { meta } => {
                ctrl.borrow_mut().loading_details = false;
                w.set_loading_details(false);
                load_meta(ctrl, w, *meta, true);
                w.set_status(SharedString::from(
                    "Details loaded. Edit fields and pick a poster.",
                ));
            }
            Event::ThumbDone {
                index,
                width,
                height,
                rgba,
            } => {
                let c = ctrl.borrow();
                if index < c.posters.row_count() {
                    // Preserve the size caption set at placeholder creation.
                    let size = c
                        .posters
                        .row_data(index)
                        .map(|p| p.size)
                        .unwrap_or_default();
                    c.posters.set_row_data(
                        index,
                        PosterItem {
                            image: rgba_to_image(width, height, &rgba),
                            loaded: true,
                            size,
                        },
                    );
                }
            }
            Event::FullImageDone {
                index,
                width,
                height,
                rgba,
            } => {
                let img = rgba_to_image(width, height, &rgba);
                {
                    let mut c = ctrl.borrow_mut();
                    if index < c.full_images.len() {
                        c.full_images[index] = Some(img.clone());
                    }
                }
                // If the lightbox is currently showing this TMDB image, update it.
                if ctrl.borrow().lightbox == Some(Lightbox::Tmdb(index)) {
                    w.set_lightbox_image(img);
                    w.set_lightbox_loaded(true);
                }
            }
            Event::WriteDone { file } => {
                let (name, how, show_splash) = {
                    let mut c = ctrl.borrow_mut();
                    c.writing = false;
                    let name = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    let how = if c.write_is_shift {
                        "rewritten (media shifted)"
                    } else {
                        "updated in place"
                    };
                    c.write_is_shift = false;
                    // Count each successful write and persist it. Unlicensed
                    // users see the donation splash every 5th write, deferred
                    // until the completion dialog is dismissed.
                    c.settings.tag_count = c.settings.tag_count.wrapping_add(1);
                    let _ = c.settings.save();
                    let show_splash = !c.settings.is_licensed() && c.settings.tag_count % 5 == 0;
                    c.splash_pending = show_splash;
                    (name, how, show_splash)
                };
                let _ = show_splash;
                w.set_writing(false);
                w.set_show_progress(false);
                w.set_status(SharedString::from(format!("Saved: {name}")));
                w.set_complete_msg(SharedString::from(format!("“{name}” was {how}.")));
                w.set_show_complete(true);
            }
            Event::WriteStarted => {
                ctrl.borrow_mut().write_is_shift = true;
                w.set_progress_phase(SharedString::from("Copying"));
                w.set_progress_fraction(0.0);
                w.set_progress_caption(SharedString::from(""));
                w.set_show_progress(true);
            }
            Event::WriteProgress(phase, done, total) => {
                w.set_progress_phase(SharedString::from(phase));
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                w.set_progress_fraction(frac);
                if total > 0 {
                    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
                    w.set_progress_caption(SharedString::from(format!(
                        "{:.0} / {:.0} MiB",
                        mb(done),
                        mb(total)
                    )));
                }
            }
            Event::Error(e) => {
                {
                    let mut c = ctrl.borrow_mut();
                    c.loading_details = false;
                    c.write_is_shift = false;
                    c.writing = false;
                }
                w.set_loading_details(false);
                w.set_writing(false);
                w.set_show_progress(false);
                w.set_status(SharedString::from(format!("Error: {e}")));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Matches list
// ---------------------------------------------------------------------------

fn set_matches(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let items: Vec<MatchItem> = ctrl
        .borrow()
        .results
        .iter()
        .map(|r| MatchItem {
            title: SharedString::from(r.title.clone()),
            year: SharedString::from(r.year.map(|y| y.to_string()).unwrap_or_default()),
        })
        .collect();
    w.set_matches(ModelRc::new(VecModel::from(items)));
}

fn select_match(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, idx: i32) {
    if idx < 0 {
        return;
    }
    let idx = idx as usize;
    let (id, file) = {
        let c = ctrl.borrow();
        if c.loading_details {
            return;
        }
        let Some(r) = c.results.get(idx) else {
            return;
        };
        let Some(file) = c.file.clone() else {
            return;
        };
        (
            ProviderId {
                provider: r.id.provider.clone(),
                id: r.id.id.clone(),
                kind: r.id.kind,
            },
            file,
        )
    };
    ctrl.borrow_mut().loading_details = true;
    w.set_loading_details(true);
    w.set_selected_match_index(idx as i32);
    w.set_status(SharedString::from("Loading details…"));
    let c = ctrl.borrow();
    let _ = c.worker.tx.send(Request::FetchDetails { id, file });
}

// ---------------------------------------------------------------------------
// Undo / redo + snapshots
// ---------------------------------------------------------------------------

/// Capture the current editable state (read from the window properties).
fn snapshot(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) -> Snapshot {
    let c = ctrl.borrow();
    Snapshot {
        title: w.get_title_text().to_string(),
        year: w.get_year_text().to_string(),
        video_kind: index_to_video_kind(w.get_video_kind_index()),
        definition: index_to_definition(w.get_definition_index()),
        rating: index_to_rating(w.get_rating_index()),
        summary: w.get_summary_text().to_string(),
        overview: w.get_overview_text().to_string(),
        genres: w.get_genres_text().to_string(),
        cast: w.get_cast_text().to_string(),
        directors: w.get_directors_text().to_string(),
        producers: w.get_producers_text().to_string(),
        writers: w.get_writers_text().to_string(),
        studio: w.get_studio_text().to_string(),
        cover_bytes: c.cover_bytes.clone(),
        cover_size: c.cover_size,
    }
}

/// Push a "before" snapshot onto the undo stack and clear redo. Flushes any
/// pending coalesced text edit first so ordering is correct. Refreshes the
/// committed baseline to the current state.
fn push_undo(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, before: Snapshot) {
    commit_text_edit(ctrl, w);
    {
        let mut c = ctrl.borrow_mut();
        c.undo_stack.push(before);
        c.redo_stack.clear();
    }
    refresh_committed(ctrl, w);
}

/// Refresh the committed baseline to the current live state.
fn refresh_committed(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let s = snapshot(ctrl, w);
    ctrl.borrow_mut().committed = s;
}

/// Commit a pending coalesced text edit if the state changed since it began.
fn commit_text_edit(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let pending = ctrl.borrow_mut().text_edit_pending.take();
    if let Some(before) = pending {
        let now = snapshot(ctrl, w);
        if before != now {
            let mut c = ctrl.borrow_mut();
            c.undo_stack.push(before);
            c.redo_stack.clear();
            c.committed = now;
        }
    }
}

/// Any field edit: capture a pre-edit snapshot on the first change of a run.
fn on_field_edited(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    // Editing a text field deselects the current poster (matches egui).
    w.set_poster_selected(false);
    // Enforce the 255-char summary limit as the user types.
    let s = w.get_summary_text();
    if s.chars().count() > 255 {
        let clipped: String = s.chars().take(255).collect();
        w.set_summary_text(SharedString::from(clipped));
    }
    // On the first keystroke of a run, remember the committed baseline as the
    // "before" state for a single coalesced undo step.
    if ctrl.borrow().text_edit_pending.is_none() {
        let before = ctrl.borrow().committed.clone();
        ctrl.borrow_mut().text_edit_pending = Some(before);
    }
    update_undo_redo(ctrl, w);
}

/// A discrete (non-text) change such as a dropdown selection. The two-way bind
/// has already applied the new value to the property, so the pre-change state
/// is taken from the committed baseline (see `Controller::committed`). Any
/// in-flight text edit is flushed first so ordering is correct.
fn discrete_change(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    commit_text_edit(ctrl, w);
    let before = ctrl.borrow().committed.clone();
    let now = snapshot(ctrl, w);
    if before != now {
        let mut c = ctrl.borrow_mut();
        c.undo_stack.push(before);
        c.redo_stack.clear();
        c.committed = now;
    }
    update_undo_redo(ctrl, w);
}

fn undo(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    commit_text_edit(ctrl, w);
    let prev = ctrl.borrow_mut().undo_stack.pop();
    if let Some(prev) = prev {
        let current = snapshot(ctrl, w);
        ctrl.borrow_mut().redo_stack.push(current);
        apply_snapshot(ctrl, w, prev);
        refresh_committed(ctrl, w);
        w.set_status(SharedString::from("Undo."));
    }
    update_undo_redo(ctrl, w);
}

fn redo(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    commit_text_edit(ctrl, w);
    let next = ctrl.borrow_mut().redo_stack.pop();
    if let Some(next) = next {
        let current = snapshot(ctrl, w);
        ctrl.borrow_mut().undo_stack.push(current);
        apply_snapshot(ctrl, w, next);
        refresh_committed(ctrl, w);
        w.set_status(SharedString::from("Redo."));
    }
    update_undo_redo(ctrl, w);
}

/// Restore a snapshot into the live properties, regenerating the cover image.
fn apply_snapshot(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, s: Snapshot) {
    w.set_title_text(SharedString::from(s.title));
    w.set_year_text(SharedString::from(s.year));
    w.set_video_kind_index(video_kind_to_index(s.video_kind));
    w.set_definition_index(definition_to_index(s.definition));
    w.set_rating_index(rating_to_index(&s.rating));
    w.set_summary_text(SharedString::from(s.summary));
    w.set_overview_text(SharedString::from(s.overview));
    w.set_genres_text(SharedString::from(s.genres));
    w.set_cast_text(SharedString::from(s.cast));
    w.set_directors_text(SharedString::from(s.directors));
    w.set_producers_text(SharedString::from(s.producers));
    w.set_writers_text(SharedString::from(s.writers));
    w.set_studio_text(SharedString::from(s.studio));
    ctrl.borrow_mut().cover_size = s.cover_size;
    set_cover_bytes(ctrl, w, s.cover_bytes);
    w.set_cover_size_caption(SharedString::from(size_caption(s.cover_size)));
}

/// Set the cover to the given encoded bytes (or clear it) and regenerate the
/// display image. Does not touch undo history.
fn set_cover_bytes(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, bytes: Option<Vec<u8>>) {
    match &bytes {
        Some(b) => {
            if let Ok(img) = tagtiger_core::artwork::thumbnail(b, 1000) {
                w.set_cover_image(rgba_to_image(img.width, img.height, &img.rgba));
                w.set_has_cover(true);
            }
        }
        None => {
            w.set_cover_image(slint::Image::default());
            w.set_has_cover(false);
        }
    }
    ctrl.borrow_mut().cover_bytes = bytes;
}

/// Push the OS accent / selection colors into the `Theme` global so the app's
/// highlights and progress bar match the user's chosen OS colors. Falls back to
/// Slint's Palette defaults (`has-sys-colors` false) when the OS colors can't
/// be read (e.g. a Linux desktop without an accent-color portal setting).
fn apply_system_colors(w: &MainWindow) {
    if let Some(c) = crate::sys_colors::system_colors() {
        let theme = w.global::<Theme>();
        let col =
            |c: crate::sys_colors::Rgb| slint::Brush::SolidColor(slint::Color::from_rgb_u8(c.r, c.g, c.b));
        theme.set_sys_highlight(col(c.highlight));
        theme.set_sys_highlight_text(col(c.highlight_text));
        theme.set_sys_accent(col(c.accent));
        theme.set_has_sys_colors(true);
    }
}

fn update_undo_redo(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let c = ctrl.borrow();
    w.set_can_undo(!c.undo_stack.is_empty() || c.text_edit_pending.is_some());
    w.set_can_redo(!c.redo_stack.is_empty());
}

// ---------------------------------------------------------------------------
// Poster clipboard / grid
// ---------------------------------------------------------------------------

/// Dispatch a synthetic Ctrl/Cmd + `key` shortcut to the window so the focused
/// TextInput performs the corresponding standard text op (Slint maps Cmd→control
/// on macOS internally, so `Control` is correct on every platform). Used to
/// route the Edit-menu Cut/Copy/Paste to the focused field.
fn dispatch_text_shortcut(w: &MainWindow, key: char) {
    use slint::platform::{Key, WindowEvent};
    let win = w.window();
    win.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
    win.dispatch_event(WindowEvent::KeyPressed { text: SharedString::from(key.to_string()) });
    win.dispatch_event(WindowEvent::KeyReleased { text: SharedString::from(key.to_string()) });
    win.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
}

/// Route Cut to the focused text field, else the selected current poster.
fn do_cut(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let clip = w.global::<ClipCtx>();
    if clip.get_text_focused() {
        dispatch_text_shortcut(w, 'x');
    } else if clip.get_current_poster() {
        cut_poster(ctrl, w);
    }
}

/// Route Copy to the focused text field, the selected current poster, or a
/// selected TMDB grid poster (grid → copy that poster's image).
fn do_copy(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let clip = w.global::<ClipCtx>();
    if clip.get_text_focused() {
        dispatch_text_shortcut(w, 'c');
    } else if clip.get_current_poster() {
        copy_poster(ctrl, w);
    } else if clip.get_grid_poster() {
        copy_grid_poster(ctrl, w);
    }
}

/// Route Paste to the focused text field, else the selected current poster.
fn do_paste(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let clip = w.global::<ClipCtx>();
    if clip.get_text_focused() {
        dispatch_text_shortcut(w, 'v');
    } else if clip.get_current_poster() {
        paste_poster(ctrl, w);
    }
}

/// Copy the currently selected TMDB grid poster's image to the clipboard.
fn copy_grid_poster(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let idx = w.get_selected_poster_index();
    if idx < 0 {
        return;
    }
    // Prefer an already-fetched full image's bytes; fall back to the thumbnail
    // source is not byte-addressable, so we re-use the cover pipeline by asking
    // the worker to download the poster URL and place it on the clipboard.
    let url = {
        let c = ctrl.borrow();
        c.meta
            .as_ref()
            .and_then(|m| m.artwork.get(idx as usize))
            .map(|a| a.url.clone())
    };
    if let Some(url) = url {
        w.set_status(SharedString::from("Copying poster…"));
        let c = ctrl.borrow();
        let _ = c.worker.tx.send(Request::CopyPosterToClipboard { url });
    }
}

fn copy_poster(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let bytes = ctrl.borrow().cover_bytes.clone();
    let msg = match bytes {
        Some(b) if write_clipboard_image(&b).is_ok() => "Poster copied to clipboard.",
        Some(_) => "Failed to copy poster.",
        None => "No poster to copy.",
    };
    w.set_status(SharedString::from(msg));
}

fn cut_poster(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let (has, locked, bytes) = {
        let c = ctrl.borrow();
        (c.cover_bytes.is_some(), w.get_lock_poster(), c.cover_bytes.clone())
    };
    if !has {
        w.set_status(SharedString::from("No poster to cut."));
        return;
    }
    if locked {
        w.set_status(SharedString::from("Poster is locked."));
        return;
    }
    if let Some(b) = &bytes {
        let _ = write_clipboard_image(b);
    }
    let before = snapshot(ctrl, w);
    push_undo(ctrl, w, before);
    set_cover_bytes(ctrl, w, None);
    ctrl.borrow_mut().cover_size = None;
    w.set_cover_size_caption(SharedString::from(size_caption(None)));
    w.set_status(SharedString::from("Poster cut."));
    update_undo_redo(ctrl, w);
}

fn paste_poster(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    if w.get_lock_poster() {
        w.set_status(SharedString::from("Poster is locked."));
        return;
    }
    if let Some(bytes) = read_clipboard_image_png() {
        set_cover_from_bytes_undoable(ctrl, w, bytes);
        w.set_status(SharedString::from("Poster pasted."));
    } else {
        w.set_status(SharedString::from("No image on clipboard."));
    }
}

fn set_cover_from_bytes_undoable(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, bytes: Vec<u8>) {
    let before = snapshot(ctrl, w);
    push_undo(ctrl, w, before);
    let size = tagtiger_core::artwork::dimensions(&bytes).ok();
    ctrl.borrow_mut().cover_size = size;
    set_cover_bytes(ctrl, w, Some(bytes));
    w.set_cover_size_caption(SharedString::from(size_caption(size)));
    update_undo_redo(ctrl, w);
}

fn poster_choice_clicked(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, idx: i32) {
    if idx < 0 || w.get_lock_poster() {
        return;
    }
    let idx = idx as usize;
    let url = {
        let c = ctrl.borrow();
        c.meta
            .as_ref()
            .and_then(|m| m.artwork.get(idx))
            .map(|a| a.url.clone())
    };
    if let Some(url) = url {
        // Select this grid tile (highlight) and clear the current-cover
        // selection; the chosen poster becomes the current cover.
        w.set_selected_poster_index(idx as i32);
        w.set_poster_selected(false);
        w.set_status(SharedString::from("Setting poster…"));
        let c = ctrl.borrow();
        let _ = c.worker.tx.send(Request::SetCoverFromUrl { url });
    }
}

// ---------------------------------------------------------------------------
// load_meta / collect_edited
// ---------------------------------------------------------------------------

/// Populate the editable properties from `meta`. When `respect_locks` is true
/// (a new match's details), locked fields are left untouched; when false
/// (initial file load) all fields are set.
fn load_meta(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, meta: MediaMetadata, respect_locks: bool) {
    if !(respect_locks && w.get_lock_title()) {
        w.set_title_text(SharedString::from(meta.title.clone()));
    }
    if !(respect_locks && w.get_lock_video_kind()) {
        w.set_video_kind_index(video_kind_to_index(meta.video_kind));
    }
    // Definition is only set on the initial file load; selecting a TMDB match
    // never changes it.
    if !respect_locks {
        w.set_definition_index(definition_to_index(meta.definition));
    }
    if !(respect_locks && w.get_lock_year()) {
        let y = meta
            .release_date
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        w.set_year_text(SharedString::from(y));
    }
    if !(respect_locks && w.get_lock_rating()) {
        let r = meta.content_rating.clone().unwrap_or_default();
        w.set_rating_index(rating_to_index(&r));
    }
    if !(respect_locks && w.get_lock_summary()) {
        w.set_summary_text(SharedString::from(meta.summary.clone().unwrap_or_default()));
    }
    if !(respect_locks && w.get_lock_overview()) {
        w.set_overview_text(SharedString::from(meta.overview.clone().unwrap_or_default()));
    }
    if !(respect_locks && w.get_lock_genres()) {
        w.set_genres_text(SharedString::from(meta.genres.join(", ")));
    }
    if !(respect_locks && w.get_lock_cast()) {
        w.set_cast_text(SharedString::from(join_people(&meta.cast)));
    }
    if !(respect_locks && w.get_lock_directors()) {
        w.set_directors_text(SharedString::from(join_people(&meta.directors)));
    }
    if !(respect_locks && w.get_lock_producers()) {
        w.set_producers_text(SharedString::from(join_people(&meta.producers)));
    }
    if !(respect_locks && w.get_lock_writers()) {
        w.set_writers_text(SharedString::from(join_people(&meta.writers)));
    }
    if !(respect_locks && w.get_lock_studio()) {
        w.set_studio_text(SharedString::from(meta.studio.clone().unwrap_or_default()));
    }

    // Rebuild the poster grid model (all placeholders; thumbs fetched below).
    let n = meta.artwork.len();
    {
        let c = ctrl.borrow();
        c.posters.set_vec(
            meta.artwork
                .iter()
                .map(|art| PosterItem {
                    image: slint::Image::default(),
                    loaded: false,
                    size: SharedString::from(size_caption(art.width.zip(art.height))),
                })
                .collect::<Vec<_>>(),
        );
    }
    ctrl.borrow_mut().thumb_requested = vec![false; n];
    {
        let mut c = ctrl.borrow_mut();
        c.full_images = vec![None; n];
        c.full_requested = vec![false; n];
        c.lightbox = None;
    }
    w.set_show_lightbox(false);

    // Thumbnails load lazily as their grid rows scroll into view — the
    // poster-rows-visible callback drives fetching (see request_visible_thumbs).
    // Request the initially-visible rows now (row 0 onward for the ~2 rows the
    // 240px viewport shows). The grid also fires the callback once it lays out.
    ctrl.borrow_mut().meta = Some(meta);
    request_visible_thumbs(ctrl, 0, 1, 4);
    refresh_committed(ctrl, w);
    update_undo_redo(ctrl, w);
}

/// Compute the inclusive artwork-index range `[first, last]` covered by grid
/// rows `[first_row, last_row]` at `columns` per row, clamped to `count` items.
/// Returns `None` when there's nothing to cover (no columns or no items).
fn visible_index_range(
    first_row: i32,
    last_row: i32,
    columns: i32,
    count: usize,
) -> Option<(usize, usize)> {
    if columns < 1 || count == 0 {
        return None;
    }
    let columns = columns as usize;
    let first = (first_row.max(0) as usize) * columns;
    if first >= count {
        return None;
    }
    // last_row is inclusive, so include the whole last row.
    let last = (((last_row.max(0) as usize) + 1) * columns - 1).min(count - 1);
    Some((first, last))
}

/// Fetch thumbnails for the poster grid rows in the inclusive range
/// `[first_row, last_row]` (with `columns` per row) that haven't been requested
/// yet. Called from the `poster-rows-visible` callback as the user scrolls, and
/// once when a match's posters first load.
fn request_visible_thumbs(ctrl: &Rc<RefCell<Controller>>, first_row: i32, last_row: i32, columns: i32) {
    let mut to_fetch: Vec<(usize, String)> = Vec::new();
    {
        let mut c = ctrl.borrow_mut();
        let count = c.meta.as_ref().map(|m| m.artwork.len()).unwrap_or(0);
        let Some((first, last)) = visible_index_range(first_row, last_row, columns, count) else {
            return;
        };
        let meta = c.meta.clone();
        let Some(meta) = meta else {
            return;
        };
        // Collect (index, url) for visible, not-yet-requested tiles.
        for i in first..=last {
            if c.thumb_requested.get(i).copied().unwrap_or(true) {
                continue;
            }
            let art = &meta.artwork[i];
            let url = art.thumb_url.clone().unwrap_or_else(|| art.url.clone());
            to_fetch.push((i, url));
        }
        // Mark as requested before sending so a rapid re-scroll doesn't double-fetch.
        for (i, _) in &to_fetch {
            if let Some(flag) = c.thumb_requested.get_mut(*i) {
                *flag = true;
            }
        }
    }
    let c = ctrl.borrow();
    for (i, url) in to_fetch {
        let _ = c.worker.tx.send(Request::FetchThumb { index: i, url });
    }
}

/// Rebuild a MediaMetadata from the edited properties before writing.
fn collect_edited(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) -> Option<MediaMetadata> {
    use tagtiger_core::model::Person;
    let base = ctrl.borrow().meta.clone()?;
    let mut m = base;
    m.title = w.get_title_text().to_string();
    m.video_kind = index_to_video_kind(w.get_video_kind_index());
    m.definition = index_to_definition(w.get_definition_index());
    m.release_date =
        chrono::NaiveDate::parse_from_str(w.get_year_text().trim(), "%Y-%m-%d").ok();
    let rating = index_to_rating(w.get_rating_index());
    m.content_rating = if rating.trim().is_empty() {
        None
    } else {
        Some(rating)
    };
    let summary = w.get_summary_text().to_string();
    m.summary = if summary.trim().is_empty() {
        None
    } else {
        Some(summary.chars().take(255).collect())
    };
    let overview = w.get_overview_text().to_string();
    m.overview = if overview.trim().is_empty() {
        None
    } else {
        Some(overview)
    };
    m.genres = split_csv(&w.get_genres_text());
    m.cast = split_csv(&w.get_cast_text())
        .into_iter()
        .map(Person::new)
        .collect();
    m.directors = split_csv(&w.get_directors_text())
        .into_iter()
        .map(Person::new)
        .collect();
    m.producers = split_csv(&w.get_producers_text())
        .into_iter()
        .map(Person::new)
        .collect();
    m.writers = split_csv(&w.get_writers_text())
        .into_iter()
        .map(Person::new)
        .collect();
    let studio = w.get_studio_text().to_string();
    m.studio = if studio.trim().is_empty() {
        None
    } else {
        Some(studio.trim().to_string())
    };
    Some(m)
}

// ---------------------------------------------------------------------------
// Dialogs (License / Settings) + lightbox + CLI install + icon
// ---------------------------------------------------------------------------

/// Open the License Key dialog, prefilling saved values.
fn open_license_dialog(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let (email, key) = {
        let c = ctrl.borrow();
        (
            c.settings.license_email.clone(),
            crate::license_mgr::format_key(&c.settings.license_key),
        )
    };
    let valid = crate::license_mgr::is_valid(&key, &email);
    w.set_license_email(SharedString::from(email));
    w.set_license_key(SharedString::from(key));
    w.set_license_valid(valid);
    w.set_license_msg(SharedString::from(""));
    w.set_show_license(true);
}

/// Save the license entered in the dialog (validated live in the UI).
fn license_save(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let email = w.get_license_email().trim().to_string();
    let key = crate::license_mgr::normalize_key(&w.get_license_key());
    let result = {
        let mut c = ctrl.borrow_mut();
        c.settings.license_email = email;
        c.settings.license_key = key;
        c.settings.save()
    };
    match result {
        Ok(()) => {
            w.set_show_license(false);
            w.set_status(SharedString::from("License saved. Thank you!"));
            // A valid license suppresses the splash and updates About.
            let licensed = ctrl.borrow().settings.is_licensed();
            w.set_about_licensed(licensed);
            if licensed {
                w.set_show_splash(false);
            }
        }
        Err(e) => {
            w.set_license_msg(SharedString::from(format!("Couldn't save settings: {e}")));
        }
    }
}

/// Open the Settings dialog, prefilling the saved TMDB Bearer token + theme.
fn open_settings_dialog(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let (token, dark) = {
        let c = ctrl.borrow();
        (c.settings.tmdb_bearer_token.clone(), c.settings.theme_is_dark())
    };
    w.set_settings_token(SharedString::from(token));
    // Reflect the current theme in the selector; remember it so Cancel can
    // revert a live preview.
    w.set_settings_theme_index(if dark { 1 } else { 0 });
    w.set_theme_dark(dark);
    ctrl.borrow_mut().theme_before_settings = dark;
    w.set_show_settings(true);
}

/// Save the TMDB token + theme from the Settings dialog and hand the token to
/// the worker.
fn settings_save(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let token = w.get_settings_token().trim().to_string();
    let dark = w.get_settings_theme_index() != 0;
    let result = {
        let mut c = ctrl.borrow_mut();
        c.settings.tmdb_bearer_token = token.clone();
        c.settings.theme = if dark { "Dark".into() } else { "Light".into() };
        c.settings.save()
    };
    match result {
        Ok(()) => w.set_status(SharedString::from("TMDB token saved.")),
        Err(e) => w.set_status(SharedString::from(format!("Failed to save settings: {e}"))),
    }
    // Empty clears it, falling back to environment variables.
    let c = ctrl.borrow();
    let _ = c.worker.tx.send(Request::SetBearerToken(token));
    w.set_show_settings(false);
}

/// Open the lightbox on the current file cover (already-decoded image).
fn open_current_cover_lightbox(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    if !w.get_has_cover() {
        return;
    }
    ctrl.borrow_mut().lightbox = Some(Lightbox::CurrentCover);
    // Reuse the cover image already set on the window.
    w.set_lightbox_image(w.get_cover_image());
    w.set_lightbox_loaded(true);
    w.set_show_lightbox(true);
}

/// Open the lightbox for a TMDB artwork `index`, requesting a larger image if
/// not already loaded.
fn open_tmdb_lightbox(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow, idx: i32) {
    if idx < 0 {
        return;
    }
    let index = idx as usize;
    let (cached, url, needs_fetch) = {
        let c = ctrl.borrow();
        let cached = c.full_images.get(index).and_then(|s| s.clone());
        let url = c
            .meta
            .as_ref()
            .and_then(|m| m.artwork.get(index))
            .map(|a| a.url.clone());
        let needs_fetch = cached.is_none()
            && !c.full_requested.get(index).copied().unwrap_or(true);
        (cached, url, needs_fetch)
    };
    ctrl.borrow_mut().lightbox = Some(Lightbox::Tmdb(index));
    match cached {
        Some(img) => {
            w.set_lightbox_image(img);
            w.set_lightbox_loaded(true);
        }
        None => {
            w.set_lightbox_loaded(false);
        }
    }
    w.set_show_lightbox(true);
    if needs_fetch {
        if let Some(url) = url {
            if let Some(flag) = ctrl.borrow_mut().full_requested.get_mut(index) {
                *flag = true;
            }
            let c = ctrl.borrow();
            let _ = c.worker.tx.send(Request::FetchFullImage { index, url });
        }
    }
}

/// macOS: resolve the window's `NSView` via its raw window handle and install
/// the native file drag-and-drop overlay (see `macos_open::install_drag_drop`).
#[cfg(target_os = "macos")]
fn install_native_drag_drop(w: &MainWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let win = w.window();
    let sh = win.window_handle();
    let wh = match sh.window_handle() {
        Ok(h) => h,
        Err(_) => return,
    };
    if let RawWindowHandle::AppKit(appkit) = wh.as_raw() {
        // SAFETY: the handle is valid for the live window; called on the UI
        // (main) thread from a Slint event-loop timer.
        unsafe {
            crate::macos_open::install_drag_drop(appkit.ns_view.as_ptr());
        }
    }
}

/// Decode the embedded 256px PNG icon into a `slint::Image` for dialogs and the
/// window icon.
fn load_app_icon() -> Option<slint::Image> {
    let bytes = include_bytes!("resources/app_icon_256.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(rgba_to_image(w, h, &img.into_raw()))
}

/// Open a URL in the user's default browser.
fn webbrowser_open(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

/// Install the `tagtiger` CLI onto the user's PATH.
#[cfg(target_os = "macos")]
fn install_cli(_ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    use std::path::PathBuf;

    let cli = match std::env::current_exe() {
        Ok(exe) => exe
            .parent()
            .map(|dir| dir.join("tagtiger-cli"))
            .unwrap_or_else(|| PathBuf::from("tagtiger-cli")),
        Err(e) => {
            w.set_status(SharedString::from(format!(
                "Couldn't locate the app executable: {e}"
            )));
            return;
        }
    };
    if !cli.exists() {
        w.set_status(SharedString::from(
            "Couldn't find the bundled CLI (tagtiger-cli) next to the app.",
        ));
        return;
    }

    let dest = "/usr/local/bin/tagtiger";
    let src = cli.to_string_lossy().to_string();

    let bindir = std::path::Path::new("/usr/local/bin");
    let writable_no_sudo = bindir.exists()
        && std::fs::metadata(bindir)
            .map(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o200 != 0
            })
            .unwrap_or(false)
        && std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(bindir.join(".tagtiger-write-probe"))
            .map(|_| {
                let _ = std::fs::remove_file(bindir.join(".tagtiger-write-probe"));
                true
            })
            .unwrap_or(false);

    if writable_no_sudo {
        let _ = std::fs::remove_file(dest);
        match std::os::unix::fs::symlink(&src, dest) {
            Ok(()) => {
                w.set_status(SharedString::from(format!(
                    "Installed CLI: run `tagtiger --help`. ({dest})"
                )));
            }
            Err(e) => w.set_status(SharedString::from(format!("Failed to install CLI: {e}"))),
        }
        return;
    }

    // Privileged path: prompt for admin rights via osascript.
    let esc = |s: &str| s.replace('\'', r"'\''");
    let shell_cmd = format!(
        "mkdir -p /usr/local/bin && ln -sf '{}' '{}'",
        esc(&src),
        esc(dest)
    );
    let as_literal = shell_cmd.replace('\\', r"\\").replace('"', r#"\""#);
    let script = format!("do shell script \"{as_literal}\" with administrator privileges");

    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(out) if out.status.success() => {
            w.set_status(SharedString::from(format!(
                "Installed CLI: run `tagtiger --help`. ({dest})"
            )));
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("User canceled") || err.contains("(-128)") {
                w.set_status(SharedString::from("CLI install cancelled."));
            } else {
                w.set_status(SharedString::from(format!(
                    "Failed to install CLI: {}",
                    err.trim()
                )));
            }
        }
        Err(e) => w.set_status(SharedString::from(format!("Failed to run installer: {e}"))),
    }
}

/// CLI install is a macOS-only feature; a no-op elsewhere.
#[cfg(not(target_os = "macos"))]
fn install_cli(_ctrl: &Rc<RefCell<Controller>>, _w: &MainWindow) {}

// ---------------------------------------------------------------------------
// Search / write
// ---------------------------------------------------------------------------

fn start_search(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    let query = w.get_search_query().trim().to_string();
    if query.is_empty() {
        w.set_status(SharedString::from("Enter a title to search."));
        return;
    }
    if !has_tmdb_credential(ctrl) {
        w.set_show_no_credential(true);
        return;
    }
    w.set_status(SharedString::from(format!("Searching for “{query}”…")));
    let c = ctrl.borrow();
    let _ = c.worker.tx.send(Request::Search { query });
}

/// Whether a TMDB credential is available: a saved Bearer token or one of the
/// `TMDB_BEARER_TOKEN` / `TMDB_API_KEY` environment variables.
fn has_tmdb_credential(ctrl: &Rc<RefCell<Controller>>) -> bool {
    if !ctrl.borrow().settings.tmdb_bearer_token.trim().is_empty() {
        return true;
    }
    let env_set = |k: &str| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    env_set("TMDB_BEARER_TOKEN") || env_set("TMDB_API_KEY")
}

fn write_tags(ctrl: &Rc<RefCell<Controller>>, w: &MainWindow) {
    if ctrl.borrow().writing {
        return;
    }
    let file = ctrl.borrow().file.clone();
    let meta = collect_edited(ctrl, w);
    let (Some(file), Some(meta)) = (file, meta) else {
        w.set_status(SharedString::from("Nothing to write."));
        return;
    };
    let (cover_override, fast_start) = {
        let c = ctrl.borrow();
        (c.cover_bytes.clone(), c.edit_fast_start)
    };
    ctrl.borrow_mut().writing = true;
    w.set_writing(true);
    w.set_status(SharedString::from("Writing…"));
    let c = ctrl.borrow();
    let _ = c.worker.tx.send(Request::WriteTags {
        file,
        meta: Box::new(meta),
        artwork_url: None,
        cover_override,
        fast_start,
    });
}

// ---------------------------------------------------------------------------
// Enum <-> combo index mapping
// ---------------------------------------------------------------------------

/// video-kind combo: index 0 = "(none)"; index i>0 -> VideoKind::all()[i-1].
fn index_to_video_kind(i: i32) -> Option<VideoKind> {
    if i <= 0 {
        None
    } else {
        VideoKind::all().get((i - 1) as usize).copied()
    }
}
fn video_kind_to_index(k: Option<VideoKind>) -> i32 {
    match k {
        None => 0,
        Some(k) => VideoKind::all()
            .iter()
            .position(|x| *x == k)
            .map(|p| (p + 1) as i32)
            .unwrap_or(0),
    }
}

fn index_to_definition(i: i32) -> Option<Definition> {
    if i <= 0 {
        None
    } else {
        Definition::all().get((i - 1) as usize).copied()
    }
}
fn definition_to_index(d: Option<Definition>) -> i32 {
    match d {
        None => 0,
        Some(d) => Definition::all()
            .iter()
            .position(|x| *x == d)
            .map(|p| (p + 1) as i32)
            .unwrap_or(0),
    }
}

/// rating combo options: index 0 = "(none)", then MOVIE_RATINGS, then
/// TV_RATINGS — matching ui/app.slint's rating-options.
fn rating_options() -> Vec<&'static str> {
    let mut v = vec!["(none)"];
    v.extend_from_slice(MOVIE_RATINGS);
    v.extend_from_slice(TV_RATINGS);
    v
}
fn index_to_rating(i: i32) -> String {
    if i <= 0 {
        String::new()
    } else {
        rating_options()
            .get(i as usize)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}
fn rating_to_index(r: &str) -> i32 {
    if r.trim().is_empty() {
        return 0;
    }
    rating_options()
        .iter()
        .position(|s| *s == r)
        .map(|p| p as i32)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Image + formatting helpers
// ---------------------------------------------------------------------------

/// Build a `slint::Image` from an RGBA8 buffer.
fn rgba_to_image(width: u32, height: u32, rgba: &[u8]) -> slint::Image {
    let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
    buf.make_mut_bytes().copy_from_slice(rgba);
    slint::Image::from_rgba8(buf)
}

fn join_people(people: &[tagtiger_core::model::Person]) -> String {
    people
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn dims_caption(size: Option<(u32, u32)>) -> String {
    match size {
        Some((w, h)) if w > 0 && h > 0 => format!("{w} x {h}"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (no UI dependency) — reused from the egui implementation.
// ---------------------------------------------------------------------------

/// US movie (MPAA) content ratings.
const MOVIE_RATINGS: &[&str] = &["G", "PG", "PG-13", "R", "NC-17", "Not Rated", "Unrated"];
/// US TV content ratings.
const TV_RATINGS: &[&str] = &["TV-Y", "TV-Y7", "TV-G", "TV-PG", "TV-14", "TV-MA"];

/// Split a comma-separated list into trimmed, non-empty entries.
fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Lowercased file extension, if any.
fn ext_lower(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Whether a path looks like a taggable movie file.
fn is_movie_path(path: &std::path::Path) -> bool {
    matches!(ext_lower(path).as_deref(), Some("mp4") | Some("m4v"))
}

/// Convert a launch argument — a plain path or a `file://` URI — into a movie
/// path that exists on disk. Returns `None` if it isn't a real movie file.
fn arg_to_movie_path(arg: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    let s = arg.to_string_lossy();
    let path = if let Some(rest) = s.strip_prefix("file://") {
        let rest = match rest.find('/') {
            Some(i) => &rest[i..],
            None => rest,
        };
        std::path::PathBuf::from(percent_decode(rest))
    } else {
        std::path::PathBuf::from(arg)
    };
    (is_movie_path(&path) && path.exists()).then_some(path)
}

/// Minimal percent-decoding for `file://` URIs (e.g. `%20` -> space).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a path looks like an image file (for poster replacement).
#[allow(dead_code)]
fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        ext_lower(path).as_deref(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp")
    )
}

/// Format an image size caption like "1000 x 1500", or a placeholder.
fn size_caption(size: Option<(u32, u32)>) -> String {
    match size {
        Some((w, h)) if w > 0 && h > 0 => format!("{w} x {h}"),
        _ => "— x —".to_string(),
    }
}

/// Write an encoded image (PNG/JPEG bytes) to the system clipboard as an image.
fn write_clipboard_image(bytes: &[u8]) -> Result<(), ()> {
    let img = image::load_from_memory(bytes).map_err(|_| ())?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);

    // On macOS, also write a standard PNG pasteboard type (arboard writes only
    // TIFF, which some apps don't surface).
    #[cfg(target_os = "macos")]
    {
        let mut png = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut png, image::ImageFormat::Png)
            .is_ok()
            && crate::macos_open::write_pasteboard_png(png.get_ref())
        {
            return Ok(());
        }
    }

    let data = arboard::ImageData {
        width: w,
        height: h,
        bytes: std::borrow::Cow::Owned(img.into_raw()),
    };
    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
    clipboard.set_image(data).map_err(|_| ())
}

/// Read an image from the system clipboard and return it encoded as PNG.
fn read_clipboard_image_png() -> Option<Vec<u8>> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    let width = img.width as u32;
    let height = img.height as u32;
    let buf = image::RgbaImage::from_raw(width, height, img.bytes.into_owned())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod map_tests {
    use super::*;
    use tagtiger_core::model::{Definition, VideoKind};

    #[test]
    fn video_kind_index_roundtrip() {
        // None maps to index 0 and back.
        assert_eq!(video_kind_to_index(None), 0);
        assert_eq!(index_to_video_kind(0), None);
        // Every kind round-trips through its index.
        for k in VideoKind::all() {
            let i = video_kind_to_index(Some(*k));
            assert!(i >= 1);
            assert_eq!(index_to_video_kind(i), Some(*k));
        }
        // Index 1 is the first of VideoKind::all() (Movie) per ui/app.slint.
        assert_eq!(index_to_video_kind(1), Some(VideoKind::all()[0]));
    }

    #[test]
    fn definition_index_roundtrip() {
        assert_eq!(definition_to_index(None), 0);
        assert_eq!(index_to_definition(0), None);
        for d in Definition::all() {
            let i = definition_to_index(Some(*d));
            assert!(i >= 1);
            assert_eq!(index_to_definition(i), Some(*d));
        }
    }

    #[test]
    fn rating_index_roundtrip() {
        // Empty rating -> "(none)" index 0.
        assert_eq!(rating_to_index(""), 0);
        assert_eq!(index_to_rating(0), "");
        // Each concrete rating round-trips.
        for r in MOVIE_RATINGS.iter().chain(TV_RATINGS.iter()) {
            let i = rating_to_index(r);
            assert!(i >= 1, "rating {r} should have a positive index");
            assert_eq!(index_to_rating(i), *r);
        }
        // A specific known mapping: options[3] == "PG-13".
        assert_eq!(index_to_rating(3), "PG-13");
        assert_eq!(rating_to_index("PG-13"), 3);
        // Unknown rating falls back to 0.
        assert_eq!(rating_to_index("BOGUS"), 0);
    }

    #[test]
    fn split_csv_trims_and_drops_empties() {
        assert_eq!(split_csv("a, b ,,c"), vec!["a", "b", "c"]);
        assert_eq!(split_csv("  "), Vec::<String>::new());
    }

    #[test]
    fn visible_index_range_maps_rows_to_indices() {
        // 4 columns, 20 items. Rows 0..=1 -> indices 0..=7.
        assert_eq!(visible_index_range(0, 1, 4, 20), Some((0, 7)));
        // Rows 2..=3 -> indices 8..=15.
        assert_eq!(visible_index_range(2, 3, 4, 20), Some((8, 15)));
        // Last row clamps to the item count (20 items -> max index 19).
        assert_eq!(visible_index_range(4, 6, 4, 20), Some((16, 19)));
        // Negative rows clamp to 0.
        assert_eq!(visible_index_range(-2, 0, 4, 20), Some((0, 3)));
        // First row beyond the data -> nothing.
        assert_eq!(visible_index_range(10, 12, 4, 20), None);
        // No columns or no items -> nothing.
        assert_eq!(visible_index_range(0, 1, 0, 20), None);
        assert_eq!(visible_index_range(0, 1, 4, 0), None);
    }
}

#[cfg(test)]
mod arg_tests {
    use super::{arg_to_movie_path, percent_decode};
    use std::ffi::OsStr;

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%done"), "100%done");
        assert_eq!(percent_decode("%2Fetc%2Ffile"), "/etc/file");
    }

    #[test]
    fn non_movie_args_rejected() {
        assert!(arg_to_movie_path(OsStr::new("--some-flag")).is_none());
        assert!(arg_to_movie_path(OsStr::new("/tmp/notes.txt")).is_none());
    }

    #[test]
    fn movie_arg_and_uri_resolve_to_existing_file() {
        let dir = std::env::temp_dir();
        let file = dir.join("tag tiger test.mp4");
        std::fs::write(&file, b"x").unwrap();

        let plain = arg_to_movie_path(file.as_os_str());
        assert_eq!(plain.as_deref(), Some(file.as_path()));

        let uri = format!("file://{}", file.to_string_lossy().replace(' ', "%20"));
        let from_uri = arg_to_movie_path(OsStr::new(&uri));
        assert_eq!(from_uri.as_deref(), Some(file.as_path()));

        let _ = std::fs::remove_file(&file);
    }
}
