//! egui application: the poster-selection and field-editing UI.

use crate::worker::{Event, Request, Worker};
use eframe::egui;
use std::path::PathBuf;
use tagtiger_core::model::{Definition, MediaMetadata, ProviderId, SearchResult, VideoKind};

/// What the lightbox is currently showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lightbox {
    /// A TMDB candidate poster by artwork index.
    Tmdb(usize),
    /// The poster currently embedded in / pasted into the file.
    CurrentCover,
}

/// A restorable snapshot of the user-editable state, used for undo/redo.
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
    /// Encoded bytes of the current cover, if any.
    cover_bytes: Option<Vec<u8>>,
    cover_size: Option<(u32, u32)>,
}

pub struct App {
    worker: Worker,

    file: Option<PathBuf>,
    status: String,
    /// True once a file has been opened (so the editing panel is shown even
    /// before any TMDB search).
    file_loaded: bool,
    /// True while a match's details are being fetched; locks match selection
    /// and shows a busy cursor.
    loading_details: bool,

    /// Search box contents (prefilled with the file's title or filename stem).
    search_query: String,

    results: Vec<SearchResult>,
    /// Working metadata: prefilled from the file, updated when a match's
    /// details are fetched.
    meta: Option<MediaMetadata>,

    /// Thumbnail of the poster currently embedded in (or pasted into) the file.
    cover_thumb: Option<egui::TextureHandle>,
    /// Full-resolution texture of the current cover, for the lightbox.
    cover_full: Option<egui::TextureHandle>,
    /// Original pixel dimensions of the current cover, for the size caption.
    cover_size: Option<(u32, u32)>,
    /// Encoded bytes (PNG/JPEG) of the current cover — the single source of
    /// truth for the poster; textures are derived from it. Written to the file
    /// when tagging.
    cover_bytes: Option<Vec<u8>>,
    /// Whether the current poster is selected (shows a highlight border and
    /// enables Cut/Copy).
    poster_selected: bool,

    /// Loaded poster thumbnails keyed by artwork index.
    thumbs: Vec<Option<egui::TextureHandle>>,
    /// Whether a thumbnail fetch has already been requested (lazy loading).
    thumb_requested: Vec<bool>,
    selected_artwork: Option<usize>,

    /// What the lightbox is currently showing, if open.
    lightbox: Option<Lightbox>,
    /// Full-resolution textures for TMDB posters, keyed by artwork index.
    full_images: Vec<Option<egui::TextureHandle>>,

    // Editable string buffers bound to the fields.
    edit_title: String,
    edit_year: String,
    edit_rating: String,
    edit_summary: String,
    edit_overview: String,
    edit_genres: String,
    edit_cast: String,
    edit_directors: String,
    edit_producers: String,
    edit_writers: String,
    edit_studio: String,

    // Per-field locks: when set, the field is read-only and is not overwritten
    // when a new match's details load.
    lock_title: bool,
    lock_year: bool,
    lock_rating: bool,
    lock_summary: bool,
    lock_overview: bool,
    lock_genres: bool,
    lock_cast: bool,
    lock_directors: bool,
    lock_producers: bool,
    lock_writers: bool,
    lock_studio: bool,
    /// Lock for the poster/cover: when set, selecting a match won't change the
    /// chosen poster and a pasted/dropped image is ignored.
    lock_poster: bool,

    // Undo/redo history of snapshots.
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// A pending "before" snapshot captured when a text edit begins, pushed to
    /// the undo stack once the edit is committed (focus lost / value settled).
    /// This coalesces a run of keystrokes into a single undo step.
    text_edit_pending: Option<Snapshot>,
    /// The text widget that held focus as of the start of this frame. Captured
    /// before menus are drawn, because opening a menu clears live focus.
    last_text_focus: Option<egui::Id>,

    /// Selected media/video kind (`stik`) and its lock.
    edit_video_kind: Option<VideoKind>,
    lock_video_kind: bool,
    /// Whether to save the file as fast-start (moov-first). Initialized from
    /// the opened file's current layout; the user can toggle it.
    edit_fast_start: bool,
    /// Selected video definition (`hdvd`) and its lock.
    edit_definition: Option<Definition>,
    lock_definition: bool,
    /// Detected video track dimensions (width, height) of the opened file.
    video_dimensions: Option<(u32, u32)>,
    /// Progress of an ongoing shift-save: (bytes_done, bytes_total).
    write_progress: Option<(u64, u64)>,
    /// When set, show a "save complete" dialog with this message.
    write_done_msg: Option<String>,
    /// True while the current save is a shift/rewrite (vs in-place).
    write_is_shift: bool,
    /// True from when the user clicks "Write tags" until the write completes
    /// (or errors). Disables the button so a write can't be launched twice.
    writing: bool,

    /// The app icon texture (96×96), lazily created for the About/Splash
    /// dialogs. `None` until first shown.
    icon_tex: Option<egui::TextureHandle>,
    /// True while the About dialog is open.
    about_open: bool,
    /// When the splash screen should stop showing. `Some(instant)` while the
    /// splash is visible on startup; cleared when it closes (after 20s or on
    /// click).
    splash_until: Option<std::time::Instant>,
    /// Set when a tag-write hits the every-10th unlicensed nag. The donation
    /// splash is deferred until the user dismisses the "Update complete"
    /// dialog, so it isn't rendered underneath (and hidden by) that dialog.
    splash_pending: bool,

    /// Persisted settings (license + tag counter), loaded at startup.
    settings: crate::license_mgr::Settings,
    /// True while the License Key dialog is open.
    license_open: bool,
    /// Email input in the License Key dialog.
    license_email_input: String,
    /// License-key input in the License Key dialog (may contain dashes).
    license_key_input: String,
    /// Transient status message shown in the License Key dialog.
    license_msg: String,
    /// True while the Settings dialog is open.
    settings_open: bool,
    /// TMDB Bearer token input in the Settings dialog.
    settings_token_input: String,
    /// True while the "TMDB credential required" message dialog is open
    /// (shown when a search is attempted without any TMDB credential).
    no_credential_open: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();

        // Load persisted settings. When a valid license is present the startup
        // splash is suppressed; otherwise it shows for 20 seconds.
        let settings = crate::license_mgr::Settings::load();
        let worker = Worker::spawn(settings.tmdb_bearer_token.clone(), move || {
            ctx.request_repaint()
        });

        let splash_until = if settings.is_licensed() {
            None
        } else {
            Some(std::time::Instant::now() + std::time::Duration::from_secs(20))
        };

        // Prefill the Settings dialog's token field with the saved value.
        let token_input = settings.tmdb_bearer_token.clone();

        let app = Self {
            worker,
            file: None,
            status: "Open an MP4/M4V file to begin.".into(),
            file_loaded: false,
            loading_details: false,
            search_query: String::new(),
            results: Vec::new(),
            meta: None,
            cover_thumb: None,
            cover_full: None,
            cover_size: None,
            cover_bytes: None,
            poster_selected: false,
            thumbs: Vec::new(),
            thumb_requested: Vec::new(),
            selected_artwork: None,
            lightbox: None,
            full_images: Vec::new(),
            edit_title: String::new(),
            edit_year: String::new(),
            edit_rating: String::new(),
            edit_summary: String::new(),
            edit_overview: String::new(),
            edit_genres: String::new(),
            edit_cast: String::new(),
            edit_directors: String::new(),
            edit_producers: String::new(),
            edit_writers: String::new(),
            edit_studio: String::new(),
            lock_title: false,
            lock_year: false,
            lock_rating: false,
            lock_summary: false,
            lock_overview: false,
            lock_genres: false,
            lock_cast: false,
            lock_directors: false,
            lock_producers: false,
            lock_writers: false,
            lock_studio: false,
            lock_poster: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            text_edit_pending: None,
            last_text_focus: None,
            edit_video_kind: None,
            lock_video_kind: false,
            edit_fast_start: false,
            edit_definition: None,
            lock_definition: false,
            video_dimensions: None,
            write_progress: None,
            write_done_msg: None,
            write_is_shift: false,
            writing: false,
            icon_tex: None,
            about_open: false,
            // Suppressed at startup when licensed (computed above).
            splash_until,
            splash_pending: false,
            settings,
            license_open: false,
            license_email_input: String::new(),
            license_key_input: String::new(),
            license_msg: String::new(),
            settings_open: false,
            settings_token_input: token_input,
            no_credential_open: false,
        };

        // If launched with a movie file argument (e.g. Finder "Open With" or
        // `open -a TagTiger movie.mp4`), open it immediately.
        if let Some(path) = std::env::args_os().nth(1).map(std::path::PathBuf::from) {
            if is_movie_path(&path) && path.exists() {
                let _ = app.worker.tx.send(Request::OpenFile { path });
            }
        }

        app
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(evt) = self.worker.rx.try_recv() {
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
                    self.file = Some(file);
                    self.file_loaded = true;
                    self.loading_details = false;
                    self.search_query = suggested_query;
                    self.results = Vec::new();
                    self.cover_bytes = cover_bytes;
                    self.cover_size = cover_size;
                    self.video_dimensions = video_dimensions;
                    self.edit_fast_start = fast_start;
                    self.poster_selected = false;
                    // Fresh file: reset undo history.
                    self.undo_stack.clear();
                    self.redo_stack.clear();
                    self.text_edit_pending = None;
                    // Prefill the editable fields from the file's existing tags.
                    self.load_meta(*meta, false);
                    // Current-file poster (one texture serves both the
                    // thumbnail and the enlarged lightbox view).
                    let cover_tex = cover.map(|(w, h, rgba)| {
                        let color = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &rgba,
                        );
                        ctx.load_texture("cover", color, Default::default())
                    });
                    self.cover_thumb = cover_tex.clone();
                    self.cover_full = cover_tex;
                    self.status = "Edit fields, or search TMDB to fetch metadata.".into();
                }
                Event::CoverSet {
                    width,
                    height,
                    rgba,
                    orig_size,
                    bytes,
                } => {
                    // Setting a poster is an undoable action.
                    let before = self.snapshot();
                    self.push_undo(before);
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let tex = ctx.load_texture("cover", color, Default::default());
                    self.cover_thumb = Some(tex.clone());
                    self.cover_full = Some(tex);
                    self.cover_size = Some(orig_size);
                    self.cover_bytes = Some(bytes);
                    self.status = "Poster set as current.".into();
                }
                Event::SearchDone { results } => {
                    self.status = format!("{} match(es).", results.len());
                    self.results = results;
                }
                Event::DetailsDone { meta } => {
                    // Merge fetched details into the editable fields and load
                    // candidate posters. Thumbnails are fetched lazily as their
                    // grid cells scroll into view (see poster_grid).
                    self.loading_details = false;
                    self.load_meta(*meta, true);
                    self.status = "Details loaded. Edit fields and pick a poster.".into();
                }
                Event::ThumbDone {
                    index,
                    width,
                    height,
                    rgba,
                } => {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let handle =
                        ctx.load_texture(format!("poster{index}"), color, Default::default());
                    if index < self.thumbs.len() {
                        self.thumbs[index] = Some(handle);
                    }
                }
                Event::FullImageDone {
                    index,
                    width,
                    height,
                    rgba,
                } => {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let handle =
                        ctx.load_texture(format!("full{index}"), color, Default::default());
                    if index < self.full_images.len() {
                        self.full_images[index] = Some(handle);
                    }
                }
                Event::WriteDone { file } => {
                    self.write_progress = None;
                    self.writing = false;
                    let name = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_string();
                    let how = if self.write_is_shift {
                        "rewritten (media shifted)"
                    } else {
                        "updated in place"
                    };
                    self.write_done_msg = Some(format!("“{name}” was {how}."));
                    self.status = format!("Saved: {name}");
                    self.write_is_shift = false;

                    // Count each successful tag-write and persist it. For
                    // unlicensed users, show the donation splash for 20s every
                    // 5th write. Licensed users are never nagged.
                    self.settings.tag_count = self.settings.tag_count.wrapping_add(1);
                    let _ = self.settings.save();
                    if !self.settings.is_licensed() && self.settings.tag_count % 5 == 0 {
                        // Defer the splash until the user dismisses the
                        // "Update complete" dialog; otherwise the splash renders
                        // underneath that dialog and can't be seen.
                        self.splash_pending = true;
                    }
                }
                Event::WriteStarted => {
                    self.write_progress = Some((0, 0));
                    self.write_is_shift = true;
                    self.status = "Saving (shifting media)…".into();
                }
                Event::WriteProgress(done, total) => {
                    self.write_progress = Some((done, total));
                }
                Event::Error(e) => {
                    self.loading_details = false;
                    self.write_progress = None;
                    self.write_is_shift = false;
                    self.writing = false;
                    self.status = format!("Error: {e}");
                }
            }
        }
    }

    /// Capture the current editable state as a snapshot.
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            title: self.edit_title.clone(),
            year: self.edit_year.clone(),
            video_kind: self.edit_video_kind,
            definition: self.edit_definition,
            rating: self.edit_rating.clone(),
            summary: self.edit_summary.clone(),
            overview: self.edit_overview.clone(),
            genres: self.edit_genres.clone(),
            cast: self.edit_cast.clone(),
            directors: self.edit_directors.clone(),
            producers: self.edit_producers.clone(),
            writers: self.edit_writers.clone(),
            studio: self.edit_studio.clone(),
            cover_bytes: self.cover_bytes.clone(),
            cover_size: self.cover_size,
        }
    }

    /// Push the given "before" snapshot onto the undo stack and clear redo.
    /// Call this right before applying a discrete change (poster set, cut,
    /// paste). No-op if the snapshot equals the current state.
    fn push_undo(&mut self, before: Snapshot) {
        // Flush any pending coalesced text edit first so ordering is correct.
        self.commit_text_edit();
        self.undo_stack.push(before);
        self.redo_stack.clear();
    }

    /// Commit a pending coalesced text edit to the undo stack if the state
    /// actually changed since the edit began.
    fn commit_text_edit(&mut self) {
        if let Some(before) = self.text_edit_pending.take() {
            if before != self.snapshot() {
                self.undo_stack.push(before);
                self.redo_stack.clear();
            }
        }
    }

    fn undo(&mut self, ctx: &egui::Context) {
        self.commit_text_edit();
        if let Some(prev) = self.undo_stack.pop() {
            let current = self.snapshot();
            self.redo_stack.push(current);
            self.apply_snapshot(ctx, prev);
            self.status = "Undo.".into();
        }
    }

    fn redo(&mut self, ctx: &egui::Context) {
        self.commit_text_edit();
        if let Some(next) = self.redo_stack.pop() {
            let current = self.snapshot();
            self.undo_stack.push(current);
            self.apply_snapshot(ctx, next);
            self.status = "Redo.".into();
        }
    }

    /// Restore a snapshot into the live fields, regenerating the cover texture.
    fn apply_snapshot(&mut self, ctx: &egui::Context, s: Snapshot) {
        self.edit_title = s.title;
        self.edit_year = s.year;
        self.edit_video_kind = s.video_kind;
        self.edit_definition = s.definition;
        self.edit_rating = s.rating;
        self.edit_summary = s.summary;
        self.edit_overview = s.overview;
        self.edit_genres = s.genres;
        self.edit_cast = s.cast;
        self.edit_directors = s.directors;
        self.edit_producers = s.producers;
        self.edit_writers = s.writers;
        self.edit_studio = s.studio;
        self.cover_size = s.cover_size;
        self.set_cover_bytes(ctx, s.cover_bytes);
    }

    /// Set the cover to the given encoded bytes (or clear it) and regenerate
    /// the display textures. Does not touch undo history.
    fn set_cover_bytes(&mut self, ctx: &egui::Context, bytes: Option<Vec<u8>>) {
        match &bytes {
            Some(b) => {
                if let Ok(img) = tagtiger_core::artwork::thumbnail(b, 1000) {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.rgba,
                    );
                    let tex = ctx.load_texture("cover", color, Default::default());
                    self.cover_thumb = Some(tex.clone());
                    self.cover_full = Some(tex);
                }
            }
            None => {
                self.cover_thumb = None;
                self.cover_full = None;
            }
        }
        self.cover_bytes = bytes;
    }

    /// Menu "Cut": route to the given focused text field (re-focus it and
    /// inject an egui Cut event), otherwise cut the poster.
    fn menu_cut(&mut self, ctx: &egui::Context, focus: Option<egui::Id>) {
        if let Some(id) = focus {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.input_mut(|i| i.events.push(egui::Event::Cut));
            ctx.request_repaint();
        } else if self.poster_selected {
            self.cut_poster(ctx);
        }
    }

    /// Menu "Copy": route to the focused text field, otherwise copy the poster.
    fn menu_copy(&mut self, ctx: &egui::Context, focus: Option<egui::Id>) {
        if let Some(id) = focus {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.input_mut(|i| i.events.push(egui::Event::Copy));
            ctx.request_repaint();
        } else if self.poster_selected {
            self.copy_poster();
        }
    }

    /// Menu "Paste": into the focused text field (inject clipboard text as an
    /// egui Paste event), otherwise paste an image as the poster.
    fn menu_paste(&mut self, ctx: &egui::Context, focus: Option<egui::Id>) {
        if let Some(id) = focus {
            if let Some(text) = read_clipboard_text() {
                ctx.memory_mut(|m| m.request_focus(id));
                ctx.input_mut(|i| i.events.push(egui::Event::Paste(text)));
                ctx.request_repaint();
            }
        } else {
            self.paste_poster(ctx);
        }
    }

    /// Copy the current poster to the system clipboard as an image.
    fn copy_poster(&mut self) {
        if let Some(bytes) = &self.cover_bytes {
            if write_clipboard_image(bytes).is_ok() {
                self.status = "Poster copied to clipboard.".into();
            } else {
                self.status = "Failed to copy poster.".into();
            }
        } else {
            self.status = "No poster to copy.".into();
        }
    }

    /// Cut the current poster: copy to clipboard, then clear it (undoable).
    fn cut_poster(&mut self, ctx: &egui::Context) {
        if self.cover_bytes.is_none() {
            self.status = "No poster to cut.".into();
            return;
        }
        if self.lock_poster {
            self.status = "Poster is locked.".into();
            return;
        }
        if let Some(bytes) = &self.cover_bytes {
            let _ = write_clipboard_image(bytes);
        }
        let before = self.snapshot();
        self.push_undo(before);
        self.set_cover_bytes(ctx, None);
        self.cover_size = None;
        self.status = "Poster cut.".into();
    }

    /// Delete the current poster without touching the clipboard (undoable).
    /// Used by the Delete/Backspace key when the poster is selected.
    fn delete_poster(&mut self, ctx: &egui::Context) {
        if self.cover_bytes.is_none() {
            self.status = "No poster to delete.".into();
            return;
        }
        if self.lock_poster {
            self.status = "Poster is locked.".into();
            return;
        }
        let before = self.snapshot();
        self.push_undo(before);
        self.set_cover_bytes(ctx, None);
        self.cover_size = None;
        self.poster_selected = false;
        self.status = "Poster deleted.".into();
    }

    /// Paste an image from the clipboard as the current poster (undoable).
    fn paste_poster(&mut self, ctx: &egui::Context) {
        if self.lock_poster {
            self.status = "Poster is locked.".into();
            return;
        }
        if let Some(bytes) = read_clipboard_image_png() {
            self.set_cover_from_bytes_undoable(ctx, bytes);
            self.status = "Poster pasted.".into();
        } else {
            self.status = "No image on clipboard.".into();
        }
    }

    /// Replace the current poster with the given encoded bytes, pushing an undo
    /// step and updating the size caption.
    fn set_cover_from_bytes_undoable(&mut self, ctx: &egui::Context, bytes: Vec<u8>) {
        let before = self.snapshot();
        self.push_undo(before);
        self.cover_size = tagtiger_core::artwork::dimensions(&bytes).ok();
        self.set_cover_bytes(ctx, Some(bytes));
    }

    /// Populate the editable buffers from `meta`. When `respect_locks` is true
    /// (a new match's details), locked fields are left untouched; when false
    /// (initial file load) all fields are set.
    fn load_meta(&mut self, meta: MediaMetadata, respect_locks: bool) {
        if !(respect_locks && self.lock_title) {
            self.edit_title = meta.title.clone();
        }
        if !(respect_locks && self.lock_video_kind) {
            self.edit_video_kind = meta.video_kind;
        }
        if !(respect_locks && self.lock_definition) {
            self.edit_definition = meta.definition;
        }
        if !(respect_locks && self.lock_year) {
            self.edit_year = meta
                .release_date
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
        }
        if !(respect_locks && self.lock_rating) {
            self.edit_rating = meta.content_rating.clone().unwrap_or_default();
        }
        if !(respect_locks && self.lock_summary) {
            self.edit_summary = meta.summary.clone().unwrap_or_default();
        }
        if !(respect_locks && self.lock_overview) {
            self.edit_overview = meta.overview.clone().unwrap_or_default();
        }
        if !(respect_locks && self.lock_genres) {
            self.edit_genres = meta.genres.join(", ");
        }
        if !(respect_locks && self.lock_cast) {
            self.edit_cast = meta
                .cast
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if !(respect_locks && self.lock_directors) {
            self.edit_directors = meta
                .directors
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if !(respect_locks && self.lock_producers) {
            self.edit_producers = meta
                .producers
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if !(respect_locks && self.lock_writers) {
            self.edit_writers = meta
                .writers
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
        }
        if !(respect_locks && self.lock_studio) {
            self.edit_studio = meta.studio.clone().unwrap_or_default();
        }

        self.thumbs = vec![None; meta.artwork.len()];
        self.thumb_requested = vec![false; meta.artwork.len()];
        self.full_images = vec![None; meta.artwork.len()];
        self.lightbox = None;
        // Never auto-select a poster. If the poster is locked, keep any prior
        // selection/custom cover; otherwise clear the selection.
        if !(respect_locks && self.lock_poster) {
            self.selected_artwork = None;
        }
        self.meta = Some(meta);
    }

    /// Rebuild a MediaMetadata from the edited buffers before writing.
    fn collect_edited(&self) -> Option<MediaMetadata> {
        let base = self.meta.clone()?;
        use tagtiger_core::model::Person;
        let mut m = base;
        m.title = self.edit_title.clone();
        m.video_kind = self.edit_video_kind;
        m.definition = self.edit_definition;
        m.release_date = chrono::NaiveDate::parse_from_str(self.edit_year.trim(), "%Y-%m-%d").ok();
        m.content_rating = if self.edit_rating.trim().is_empty() {
            None
        } else {
            Some(self.edit_rating.clone())
        };
        m.summary = if self.edit_summary.trim().is_empty() {
            None
        } else {
            // Enforce the 255-character limit on the short summary.
            Some(self.edit_summary.chars().take(255).collect())
        };
        m.overview = if self.edit_overview.trim().is_empty() {
            None
        } else {
            Some(self.edit_overview.clone())
        };
        m.genres = split_csv(&self.edit_genres);
        m.cast = split_csv(&self.edit_cast)
            .into_iter()
            .map(Person::new)
            .collect();
        m.directors = split_csv(&self.edit_directors)
            .into_iter()
            .map(Person::new)
            .collect();
        m.producers = split_csv(&self.edit_producers)
            .into_iter()
            .map(Person::new)
            .collect();
        m.writers = split_csv(&self.edit_writers)
            .into_iter()
            .map(Person::new)
            .collect();
        m.studio = if self.edit_studio.trim().is_empty() {
            None
        } else {
            Some(self.edit_studio.trim().to_string())
        };
        Some(m)
    }
}

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

/// Whether a path looks like an image file (for poster replacement).
fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        ext_lower(path).as_deref(),
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp")
    )
}

/// US movie (MPAA) content ratings, shown at the top of the Rating menu.
const MOVIE_RATINGS: &[&str] = &["G", "PG", "PG-13", "R", "NC-17", "Not Rated", "Unrated"];
/// US TV content ratings, shown at the bottom of the Rating menu.
const TV_RATINGS: &[&str] = &["TV-Y", "TV-Y7", "TV-G", "TV-PG", "TV-14", "TV-MA"];

/// Format an image size caption like "1000 x 1500", or a placeholder when the
/// dimensions are unknown.
fn size_caption(size: Option<(u32, u32)>) -> String {
    match size {
        Some((w, h)) if w > 0 && h > 0 => format!("{w} x {h}"),
        _ => "— x —".to_string(),
    }
}

/// Render a right-aligned label occupying a fixed-width cell, so all field
/// labels line up on their right edge next to the inputs.
fn right_label(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.label(text);
        },
    );
}

/// Render one field row (no grid, so the input can fill available width):
/// fixed-width label | input that fills the remaining space | lock checkbox.
/// When `lock` is set, the input is non-interactive (read-only). Returns the
/// text input's `Response` for focus/edit tracking.
fn field_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    lock: &mut bool,
    label_w: f32,
) -> egui::Response {
    // Space reserved on the right for the "Lock" checkbox + spacing.
    const LOCK_W: f32 = 64.0;
    ui.horizontal(|ui| {
        right_label(ui, label, label_w);
        let input_w = (ui.available_width() - LOCK_W).max(120.0);
        let resp = ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(input_w)
                .interactive(!*lock),
        );
        ui.checkbox(lock, "Lock");
        resp
    })
    .inner
}

/// Render a "people" row (Cast/Directors/Producers/Screenwriters): a right
/// label, a multiline input that wraps and grows vertically as needed, and a
/// lock checkbox. Returns the input `Response` for focus/edit tracking.
fn people_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    lock: &mut bool,
    label_w: f32,
) -> egui::Response {
    const LOCK_W: f32 = 64.0;
    ui.horizontal(|ui| {
        right_label(ui, label, label_w);
        let input_w = (ui.available_width() - LOCK_W).max(120.0);
        // `desired_rows(1)` with multiline: starts one line tall and grows as
        // text wraps to more lines.
        let resp = ui.add(
            egui::TextEdit::multiline(value)
                .desired_width(input_w)
                .desired_rows(1)
                .interactive(!*lock),
        );
        ui.checkbox(lock, "Lock");
        resp
    })
    .inner
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);

        // Focus for menu routing: the live focus this frame, or the focus from
        // the previous frame as a fallback (opening a menu clears live focus,
        // so the just-focused field is remembered for one frame).
        let live_focus = ctx.memory(|m| m.focused());
        let menu_focus = live_focus.or(self.last_text_focus);
        // Remember this frame's live focus for next frame's fallback.
        self.last_text_focus = live_focus;

        // Busy cursor while details are loading.
        if self.loading_details {
            ctx.set_cursor_icon(egui::CursorIcon::Wait);
        }

        // Handle a pasted image (Cmd/Ctrl+V) or a dropped image file: either
        // replaces the current poster.
        self.handle_image_input(&ctx);

        // Global keyboard shortcuts for edit actions.
        let (do_undo, do_redo, do_cut, do_copy, do_delete) = ctx.input(|i| {
            let cmd = i.modifiers.command || i.modifiers.ctrl;
            let shift = i.modifiers.shift;
            (
                cmd && !shift && i.key_pressed(egui::Key::Z),
                cmd && ((shift && i.key_pressed(egui::Key::Z)) || i.key_pressed(egui::Key::Y)),
                cmd && i.key_pressed(egui::Key::X),
                cmd && i.key_pressed(egui::Key::C),
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
            )
        });
        if do_undo {
            self.undo(&ctx);
        }
        if do_redo {
            self.redo(&ctx);
        }
        // Cut/Copy act on the poster only when it's the selected element.
        if do_cut && self.poster_selected {
            self.cut_poster(&ctx);
        }
        if do_copy && self.poster_selected {
            self.copy_poster();
        }
        // Delete/Backspace removes the poster when it's the selected element
        // and no text field has focus (so it won't disrupt text editing).
        if do_delete && self.poster_selected && live_focus.is_none() {
            self.delete_poster(&ctx);
        }

        egui::Panel::top("menubar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open…").clicked() {
                        self.status = "Choose a file…".into();
                        let _ = self.worker.tx.send(Request::PickFile);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Settings…").clicked() {
                        self.open_settings_dialog();
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let can_undo = !self.undo_stack.is_empty() || self.text_edit_pending.is_some();
                    let can_redo = !self.redo_stack.is_empty();
                    let has_poster = self.cover_bytes.is_some();

                    if ui
                        .add_enabled(can_undo, egui::Button::new("Undo"))
                        .clicked()
                    {
                        self.undo(&ctx);
                        ui.close();
                    }
                    if ui
                        .add_enabled(can_redo, egui::Button::new("Redo"))
                        .clicked()
                    {
                        self.redo(&ctx);
                        ui.close();
                    }
                    ui.separator();
                    let text_focused = menu_focus.is_some();
                    let cut_copy_enabled =
                        text_focused || (self.poster_selected && has_poster && !self.lock_poster);
                    let copy_enabled = text_focused || (self.poster_selected && has_poster);
                    let paste_enabled = text_focused || (self.file_loaded && !self.lock_poster);

                    if ui
                        .add_enabled(cut_copy_enabled, egui::Button::new("Cut"))
                        .clicked()
                    {
                        self.menu_cut(&ctx, menu_focus);
                        ui.close();
                    }
                    if ui
                        .add_enabled(copy_enabled, egui::Button::new("Copy"))
                        .clicked()
                    {
                        self.menu_copy(&ctx, menu_focus);
                        ui.close();
                    }
                    if ui
                        .add_enabled(paste_enabled, egui::Button::new("Paste"))
                        .clicked()
                    {
                        self.menu_paste(&ctx, menu_focus);
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("License Key…").clicked() {
                        self.open_license_dialog();
                        ui.close();
                    }
                    #[cfg(target_os = "macos")]
                    {
                        ui.separator();
                        if ui.button("Install Command-Line Tool…").clicked() {
                            self.install_cli();
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("About TagTiger").clicked() {
                        self.about_open = true;
                        ui.close();
                    }
                });
            });
        });

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open file…").clicked() {
                    self.status = "Choose a file…".into();
                    let _ = self.worker.tx.send(Request::PickFile);
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        // Matches panel on the far right.
        egui::Panel::right("results")
            .default_size(300.0)
            .show(ui, |ui| {
                ui.heading("Matches");
                if self.results.is_empty() {
                    ui.label("No matches yet. Type a title and press search.");
                }
                let file = self.file.clone();
                let locked = self.loading_details;
                if locked {
                    ui.label("Loading details…");
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for r in &self.results {
                        let label = format!(
                            "{} ({})",
                            r.title,
                            r.year.map(|y| y.to_string()).unwrap_or_else(|| "?".into())
                        );
                        // Disabled while a selection is loading, so the user
                        // can't pick another match until data has loaded.
                        let clicked = ui.add_enabled(!locked, egui::Button::new(label)).clicked();
                        if clicked {
                            if let Some(f) = &file {
                                self.loading_details = true;
                                self.status = "Loading details…".into();
                                let _ = self.worker.tx.send(Request::FetchDetails {
                                    id: ProviderId {
                                        provider: r.id.provider.clone(),
                                        id: r.id.id.clone(),
                                        kind: r.id.kind,
                                    },
                                    file: f.clone(),
                                });
                            }
                        }
                    }
                });
            });

        // Fields + posters on the left (central area).
        egui::CentralPanel::default().show(ui, |ui| {
            if !self.file_loaded {
                ui.label("Open a file to begin.");
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                // Search row: label, input, magnifying-glass button.
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .desired_width(240.0)
                            .hint_text("Movie title"),
                    );
                    if resp.gained_focus() {
                        self.poster_selected = false;
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    // Magnifying-glass button.
                    if ui
                        .button("\u{1F50D}")
                        .on_hover_text("Search TMDB")
                        .clicked()
                        || enter
                    {
                        self.start_search();
                    }
                });

                ui.separator();

                // Two explicit columns inside the scroll area: a fields column
                // on the left and a fixed-width poster column pinned right.
                const POSTER_COL_W: f32 = 170.0;
                const LABEL_W: f32 = 190.0;
                let total_w = ui.available_width();
                let fields_w = (total_w - POSTER_COL_W).max(320.0);
                // Accumulate text-field focus/change to drive coalesced undo.
                let mut f_gained = false;
                let mut f_lost = false;
                let mut f_changed = false;
                // Snapshot taken before this frame's field edits are applied,
                // so a coalesced text edit can be undone back to this state.
                let pre_edit = self.snapshot();
                ui.horizontal_top(|ui| {
                    // Left: fields column (bounded width). No Grid — each row is
                    // a horizontal with a filling input so it uses all width.
                    ui.allocate_ui_with_layout(
                        egui::vec2(fields_w, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.heading("Fields");
                            let r = field_row(
                                ui,
                                "Title",
                                &mut self.edit_title,
                                &mut self.lock_title,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            // Video Kind: popup of legal video types.
                            ui.horizontal(|ui| {
                                right_label(ui, "Video Kind", LABEL_W);
                                ui.add_enabled_ui(!self.lock_video_kind, |ui| {
                                    let current = self
                                        .edit_video_kind
                                        .map(|k| k.label())
                                        .unwrap_or("(none)");
                                    egui::ComboBox::from_id_salt("video_kind")
                                        .selected_text(current)
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(
                                                    self.edit_video_kind.is_none(),
                                                    "(none)",
                                                )
                                                .clicked()
                                            {
                                                self.edit_video_kind = None;
                                            }
                                            for k in VideoKind::all() {
                                                if ui
                                                    .selectable_label(
                                                        self.edit_video_kind == Some(*k),
                                                        k.label(),
                                                    )
                                                    .clicked()
                                                {
                                                    self.edit_video_kind = Some(*k);
                                                }
                                            }
                                        });
                                });
                                ui.checkbox(&mut self.lock_video_kind, "Lock");
                                ui.checkbox(&mut self.edit_fast_start, "Fast-start")
                                    .on_hover_text(
                                        "When checked, save a web-optimized file with moov \
                                         before mdat. When unchecked, moov is placed after \
                                         mdat.",
                                    );
                            });
                            // Definition: SD / HD 720p / HD 1080p / 4K.
                            ui.horizontal(|ui| {
                                right_label(ui, "Definition", LABEL_W);
                                ui.add_enabled_ui(!self.lock_definition, |ui| {
                                    let current = self
                                        .edit_definition
                                        .map(|d| d.label())
                                        .unwrap_or("(none)");
                                    egui::ComboBox::from_id_salt("definition")
                                        .selected_text(current)
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(
                                                    self.edit_definition.is_none(),
                                                    "(none)",
                                                )
                                                .clicked()
                                            {
                                                self.edit_definition = None;
                                            }
                                            for d in Definition::all() {
                                                if ui
                                                    .selectable_label(
                                                        self.edit_definition == Some(*d),
                                                        d.label(),
                                                    )
                                                    .clicked()
                                                {
                                                    self.edit_definition = Some(*d);
                                                }
                                            }
                                        });
                                });
                                ui.checkbox(&mut self.lock_definition, "Lock");
                                if let Some((w, h)) = self.video_dimensions {
                                    ui.label(format!("{w} x {h}"));
                                }
                            });
                            let r = field_row(
                                ui,
                                "Release date (YYYY-MM-DD)",
                                &mut self.edit_year,
                                &mut self.lock_year,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            // Rating: label | dropdown | lock. Movie ratings on
                            // top, TV ratings below a separator.
                            ui.horizontal(|ui| {
                                right_label(ui, "Rating", LABEL_W);
                                ui.add_enabled_ui(!self.lock_rating, |ui| {
                                    let combo = egui::ComboBox::from_id_salt("rating")
                                        .selected_text(if self.edit_rating.is_empty() {
                                            "(none)".to_string()
                                        } else {
                                            self.edit_rating.clone()
                                        })
                                        .show_ui(ui, |ui| {
                                            let mut changed = false;
                                            changed |= ui
                                                .selectable_value(
                                                    &mut self.edit_rating,
                                                    String::new(),
                                                    "(none)",
                                                )
                                                .changed();
                                            ui.label("Movie");
                                            for r in MOVIE_RATINGS {
                                                changed |= ui
                                                    .selectable_value(
                                                        &mut self.edit_rating,
                                                        (*r).to_string(),
                                                        *r,
                                                    )
                                                    .changed();
                                            }
                                            ui.separator();
                                            ui.label("TV");
                                            for r in TV_RATINGS {
                                                changed |= ui
                                                    .selectable_value(
                                                        &mut self.edit_rating,
                                                        (*r).to_string(),
                                                        *r,
                                                    )
                                                    .changed();
                                            }
                                            changed
                                        });
                                    if combo.inner == Some(true) {
                                        f_changed = true;
                                        f_lost = true;
                                    }
                                });
                                ui.checkbox(&mut self.lock_rating, "Lock");
                            });
                            let r = field_row(
                                ui,
                                "Genres (comma-sep)",
                                &mut self.edit_genres,
                                &mut self.lock_genres,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            let r = people_row(
                                ui,
                                "Directors (comma-sep)",
                                &mut self.edit_directors,
                                &mut self.lock_directors,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            let r = people_row(
                                ui,
                                "Cast (comma-sep)",
                                &mut self.edit_cast,
                                &mut self.lock_cast,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            let r = people_row(
                                ui,
                                "Producers (comma-sep)",
                                &mut self.edit_producers,
                                &mut self.lock_producers,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            let r = people_row(
                                ui,
                                "Screenwriters (comma-sep)",
                                &mut self.edit_writers,
                                &mut self.lock_writers,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();
                            let r = field_row(
                                ui,
                                "Studio",
                                &mut self.edit_studio,
                                &mut self.lock_studio,
                                LABEL_W,
                            );
                            f_gained |= r.gained_focus();
                            f_lost |= r.lost_focus();
                            f_changed |= r.changed();

                            // Summary: 2-line input, 255-character limit.
                            ui.horizontal(|ui| {
                                right_label(
                                    ui,
                                    &format!(
                                        "Summary ({}/255)",
                                        self.edit_summary.chars().count()
                                    ),
                                    LABEL_W,
                                );
                                let input_w = (ui.available_width() - 64.0).max(120.0);
                                let resp = ui.add(
                                    egui::TextEdit::multiline(&mut self.edit_summary)
                                        .desired_width(input_w)
                                        .desired_rows(2)
                                        .interactive(!self.lock_summary),
                                );
                                if resp.changed() && self.edit_summary.chars().count() > 255 {
                                    // Enforce the 255-character limit as the
                                    // user types.
                                    self.edit_summary =
                                        self.edit_summary.chars().take(255).collect();
                                }
                                f_gained |= resp.gained_focus();
                                f_lost |= resp.lost_focus();
                                f_changed |= resp.changed();
                                ui.checkbox(&mut self.lock_summary, "Lock");
                            });

                            // Long Description: multiline input, then lock.
                            ui.horizontal(|ui| {
                                right_label(ui, "Long Description", LABEL_W);
                                let input_w = (ui.available_width() - 64.0).max(120.0);
                                let resp = ui.add(
                                    egui::TextEdit::multiline(&mut self.edit_overview)
                                        .desired_width(input_w)
                                        .desired_rows(4)
                                        .interactive(!self.lock_overview),
                                );
                                f_gained |= resp.gained_focus();
                                f_lost |= resp.lost_focus();
                                f_changed |= resp.changed();
                                ui.checkbox(&mut self.lock_overview, "Lock");
                            });
                        },
                    );

                    // Right: current poster pinned to the far right.
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 0.0),
                        egui::Layout::top_down(egui::Align::RIGHT),
                        |ui| {
                            // Fixed-width centered column so the caption and
                            // lock line up under the poster.
                            ui.allocate_ui_with_layout(
                                egui::vec2(150.0, 0.0),
                                egui::Layout::top_down(egui::Align::Center),
                                |ui| {
                                    if let Some(tex) = &self.cover_thumb {
                                        let resp = ui
                                            .add(
                                                egui::Image::new(tex)
                                                    .max_width(150.0)
                                                    .sense(egui::Sense::click()),
                                            )
                                            .on_hover_text(
                                                "Click to select · double-click to enlarge · paste/drop to replace",
                                            );
                                        if resp.double_clicked() {
                                            self.lightbox = Some(Lightbox::CurrentCover);
                                        } else if resp.clicked() {
                                            // Toggle selection of the current
                                            // poster (enables Cut/Copy).
                                            self.poster_selected = !self.poster_selected;
                                            self.selected_artwork = None;
                                        }
                                        // Highlight border when selected.
                                        if self.poster_selected {
                                            ui.painter().rect_stroke(
                                                resp.rect.expand(3.0),
                                                4.0,
                                                egui::Stroke::new(
                                                    3.0,
                                                    egui::Color32::LIGHT_BLUE,
                                                ),
                                                egui::StrokeKind::Outside,
                                            );
                                        }
                                    } else {
                                        ui.add_sized(
                                            [150.0, 210.0],
                                            egui::Label::new("(no poster in file)\npaste/drop to add"),
                                        );
                                    }
                                    // Actual size caption, centered under image.
                                    ui.label(size_caption(self.cover_size));
                                    ui.checkbox(&mut self.lock_poster, "Lock");
                                },
                            );
                        },
                    );
                });

                // Clicking/focusing a text field deselects the current poster.
                if f_gained {
                    self.poster_selected = false;
                }

                // Coalesced undo for text edits: capture the pre-edit snapshot
                // when a field first gains focus or changes, and commit it as a
                // single undo step when focus leaves the field.
                if (f_gained || f_changed) && self.text_edit_pending.is_none() {
                    self.text_edit_pending = Some(pre_edit);
                }
                if f_lost {
                    self.commit_text_edit();
                }

                ui.separator();
                if ui
                    .add_enabled(!self.writing, egui::Button::new("Write tags"))
                    .clicked()
                {
                    self.write_tags();
                }

                // Poster choices from TMDB at the bottom.
                ui.separator();
                ui.heading("Poster choices (TMDB)");
                ui.label("Click to set as current poster · double-click to enlarge");
                self.poster_grid(ui);
            });
        });

        // Lightbox overlay (rendered last so it sits on top).
        self.lightbox_window(&ctx);

        // Progress overlay during a shift-save (streaming media to temp file).
        if let Some((done, total)) = self.write_progress {
            egui::Window::new("Saving")
                .id(egui::Id::new("save_progress"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label("Rewriting file (moving media)…");
                    let frac = if total > 0 {
                        done as f32 / total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(320.0)
                            .show_percentage(),
                    );
                    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
                    if total > 0 {
                        ui.label(format!("{:.0} / {:.0} MiB", mb(done), mb(total)));
                    }
                });
        }

        // Completion dialog after a save (in-place or rewrite).
        if let Some(msg) = self.write_done_msg.clone() {
            egui::Window::new("Update complete")
                .id(egui::Id::new("save_complete"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(msg);
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("OK").clicked() {
                            self.write_done_msg = None;
                            // Now that the completion dialog is dismissed, show
                            // the deferred donation splash (unlicensed, every
                            // 5th write) so it's actually visible on top.
                            if self.splash_pending {
                                self.splash_pending = false;
                                self.splash_until = Some(
                                    std::time::Instant::now()
                                        + std::time::Duration::from_secs(20),
                                );
                            }
                        }
                    });
                });
        }

        // About dialog (Help ▸ About) and the startup splash. Rendered last so
        // they sit on top of everything else.
        self.about_window(&ctx);
        self.license_window(&ctx);
        self.settings_window(&ctx);
        self.no_credential_window(&ctx);
        self.splash_window(&ctx);
    }
}

impl App {
    fn poster_grid(&mut self, ui: &mut egui::Ui) {
        // Collect what we need up front to avoid borrowing `self.meta` while
        // handling clicks that mutate `self`.
        let Some(meta) = &self.meta else { return };
        let count = meta.artwork.len();
        if count == 0 {
            ui.label("No artwork candidates.");
            return;
        }
        // (full_url, thumb_url, size)
        struct PosterInfo {
            url: String,
            thumb_url: String,
            size: Option<(u32, u32)>,
        }
        let infos: Vec<PosterInfo> = meta
            .artwork
            .iter()
            .map(|a| PosterInfo {
                url: a.url.clone(),
                thumb_url: a.thumb_url.clone().unwrap_or_else(|| a.url.clone()),
                size: a.width.zip(a.height),
            })
            .collect();

        // Actions to apply after rendering (deferred to satisfy the borrow
        // checker): (set_cover_url, enlarge_index, select_index).
        let mut set_cover: Option<String> = None;
        let mut enlarge: Option<usize> = None;
        let mut select: Option<usize> = None;
        // Thumbnails whose cells are visible and not yet requested.
        let mut to_fetch: Vec<(usize, String)> = Vec::new();

        // Uniform cell size so the posters form a rectangular grid instead of
        // stair-stepping on varying image heights.
        const IMG_W: f32 = 140.0;
        const IMG_H: f32 = 200.0;
        const CAP_H: f32 = 18.0;
        const CELL_PAD: f32 = 12.0;
        let cell_w = IMG_W + CELL_PAD;
        let cell_h = IMG_H + CAP_H + 6.0;

        // Number of columns that fit the available width (at least one).
        let avail = ui.available_width();
        let cols = ((avail / cell_w).floor() as usize).max(1);

        let mut i = 0usize;
        while i < infos.len() {
            ui.horizontal(|ui| {
                for _ in 0..cols {
                    if i >= infos.len() {
                        break;
                    }
                    let info = &infos[i];
                    // Each cell occupies a fixed-size box, so rows line up.
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(cell_w, cell_h), egui::Sense::hover());

                    // Lazy load: only fetch a thumbnail once its cell is visible
                    // (scrolled into view) and hasn't been requested yet.
                    let loaded = matches!(self.thumbs.get(i), Some(Some(_)));
                    let requested = self.thumb_requested.get(i).copied().unwrap_or(true);
                    if ui.is_rect_visible(rect) && !loaded && !requested {
                        to_fetch.push((i, info.thumb_url.clone()));
                    }

                    let mut cell = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect)
                            .layout(egui::Layout::top_down(egui::Align::Center)),
                    );
                    let response = if let Some(Some(tex)) = self.thumbs.get(i) {
                        cell.add_sized(
                            [IMG_W, IMG_H],
                            egui::Image::new(tex)
                                .max_size(egui::vec2(IMG_W, IMG_H))
                                .sense(egui::Sense::click()),
                        )
                    } else {
                        cell.add_sized([IMG_W, IMG_H], egui::Button::new("loading…"))
                    };
                    cell.label(size_caption(info.size));

                    // Highlight rectangle around the selected grid poster.
                    if self.selected_artwork == Some(i) {
                        cell.painter().rect_stroke(
                            response.rect.expand(3.0),
                            4.0,
                            egui::Stroke::new(3.0, egui::Color32::LIGHT_BLUE),
                            egui::StrokeKind::Outside,
                        );
                    }

                    if response.double_clicked() {
                        enlarge = Some(i);
                    } else if response.clicked() && !self.lock_poster {
                        // Select (highlight) and set as the current poster.
                        select = Some(i);
                        set_cover = Some(info.url.clone());
                    }
                    i += 1;
                }
            });
        }

        // Kick off lazy thumbnail fetches for the cells that scrolled into view.
        for (idx, thumb_url) in to_fetch {
            if let Some(flag) = self.thumb_requested.get_mut(idx) {
                *flag = true;
            }
            let _ = self.worker.tx.send(Request::FetchThumb {
                index: idx,
                url: thumb_url,
            });
        }

        if let Some(idx) = select {
            self.selected_artwork = Some(idx);
            // Selecting a grid poster deselects the current-cover box.
            self.poster_selected = false;
        }
        if let Some(i) = enlarge {
            self.open_tmdb_lightbox(i);
        }
        if let Some(url) = set_cover {
            self.status = "Setting poster…".into();
            let _ = self.worker.tx.send(Request::SetCoverFromUrl { url });
        }
    }

    /// Open the lightbox for a TMDB artwork `index`, requesting a larger image
    /// if not already loaded.
    fn open_tmdb_lightbox(&mut self, index: usize) {
        self.lightbox = Some(Lightbox::Tmdb(index));
        let needs_fetch = self
            .full_images
            .get(index)
            .map(|slot| slot.is_none())
            .unwrap_or(false);
        if needs_fetch {
            if let Some(meta) = &self.meta {
                if let Some(art) = meta.artwork.get(index) {
                    let _ = self.worker.tx.send(Request::FetchFullImage {
                        index,
                        url: art.url.clone(),
                    });
                }
            }
        }
    }

    /// Render the enlarged lightbox viewer as a modal-style window.
    fn lightbox_window(&mut self, ctx: &egui::Context) {
        let Some(target) = self.lightbox else { return };

        // Resolve the texture to show for the current target.
        let tex: Option<egui::TextureHandle> = match target {
            Lightbox::Tmdb(index) => self.full_images.get(index).and_then(|s| s.clone()),
            Lightbox::CurrentCover => self.cover_full.clone(),
        };

        let mut open = true;
        egui::Window::new("Poster preview")
            .id(egui::Id::new("lightbox"))
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| match &tex {
                Some(tex) => {
                    ui.add(egui::Image::new(tex).max_height(760.0).max_width(760.0));
                }
                None => {
                    ui.add_sized([360.0, 480.0], egui::Label::new("Loading…"));
                }
            });

        // Close via the window's X button or Escape.
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.lightbox = None;
        }
    }

    /// Detect a pasted (Cmd/Ctrl+V) or drag-and-dropped image and use it to
    /// replace the current poster.
    fn handle_image_input(&mut self, ctx: &egui::Context) {
        // Drag-and-drop: route by file type. A movie (mp4/m4v) opens in the
        // window; an image replaces the current poster.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            // Prefer a movie file if one was dropped.
            if let Some(movie) = dropped.iter().find(|p| is_movie_path(p)) {
                let _ = self.worker.tx.send(Request::OpenFile {
                    path: movie.clone(),
                });
                self.status = "Opening dropped file…".into();
                return;
            }
            // Otherwise, treat the first image as a new poster.
            if self.file_loaded && !self.lock_poster {
                if let Some(img) = dropped.iter().find(|p| is_image_path(p)) {
                    if let Ok(bytes) = std::fs::read(img) {
                        self.set_cover_from_bytes_undoable(ctx, bytes);
                        self.status = "Poster replaced from dropped image.".into();
                    }
                }
            }
            return;
        }

        // Clipboard paste via Cmd/Ctrl+V replaces the poster (image only).
        if !self.file_loaded || self.lock_poster {
            return;
        }
        let paste = ctx.input(|i| {
            let cmd = i.modifiers.command || i.modifiers.ctrl;
            cmd && i.key_pressed(egui::Key::V)
        });
        if paste {
            if let Some(bytes) = read_clipboard_image_png() {
                self.set_cover_from_bytes_undoable(ctx, bytes);
                self.status = "Poster pasted.".into();
            }
        }
    }

    fn start_search(&mut self) {
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.status = "Enter a title to search.".into();
            return;
        }
        // TMDB searches need a credential. If neither a saved token nor an
        // environment variable is configured, prompt the user to set one up
        // rather than firing a request that would just error.
        if !self.has_tmdb_credential() {
            self.no_credential_open = true;
            return;
        }
        self.status = format!("Searching for “{query}”…");
        let _ = self.worker.tx.send(Request::Search { query });
    }

    /// Whether a TMDB credential is available: a saved Bearer token, or one of
    /// the `TMDB_BEARER_TOKEN` / `TMDB_API_KEY` environment variables. This
    /// mirrors the worker's provider-construction logic.
    fn has_tmdb_credential(&self) -> bool {
        if !self.settings.tmdb_bearer_token.trim().is_empty() {
            return true;
        }
        let env_set = |k: &str| {
            std::env::var(k)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        };
        env_set("TMDB_BEARER_TOKEN") || env_set("TMDB_API_KEY")
    }

    fn write_tags(&mut self) {
        // Guard against re-entry (e.g. Enter key) while a write is in flight.
        if self.writing {
            return;
        }
        let (Some(file), Some(meta)) = (self.file.clone(), self.collect_edited()) else {
            self.status = "Nothing to write.".into();
            return;
        };
        let cover_override = self.cover_bytes.clone();
        // Mark as in-flight so the button is disabled until WriteDone/Error.
        self.writing = true;
        self.status = "Writing…".into();
        let _ = self.worker.tx.send(Request::WriteTags {
            file,
            meta: Box::new(meta),
            artwork_url: None,
            cover_override,
            fast_start: self.edit_fast_start,
        });
    }

    /// Lazily create the 96×96 app-icon texture used by the About/Splash
    /// dialogs, decoding the embedded PNG on first use.
    fn ensure_icon(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.icon_tex.is_none() {
            let bytes = include_bytes!("resources/app_icon_256.png");
            if let Ok(img) = image::load_from_memory(bytes) {
                let img = img.to_rgba8();
                let (w, h) = img.dimensions();
                let color =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &img);
                self.icon_tex =
                    Some(ctx.load_texture("app_icon", color, egui::TextureOptions::LINEAR));
            }
        }
        self.icon_tex.clone()
    }

    /// The About dialog — a native rendering of TagTiger's `about.html`:
    /// dark panel, rounded 96px icon, title, version/copyright/build lines,
    /// links, and an OK button.
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let icon = self.ensure_icon(ctx);
        let mut open = true;
        egui::Window::new("About TagTiger")
            .id(egui::Id::new("about"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    if let Some(tex) = &icon {
                        ui.add(
                            egui::Image::new(tex)
                                .fit_to_exact_size(egui::vec2(96.0, 96.0))
                                .corner_radius(20.0),
                        );
                    }
                    ui.add_space(8.0);
                    ui.label(title_text("TagTiger"));
                    ui.add_space(6.0);
                    ui.label(muted_text(&format!(
                        "Version {}",
                        env!("CARGO_PKG_VERSION")
                    )));
                    ui.label(muted_text("©2026 Richard Lesh"));
                    ui.label(muted_text(&format!("Built with egui v{EGUI_VERSION}")));
                    ui.add_space(2.0);
                    ui.hyperlink_to("Glowing Cat Software", GLOWING_CAT_URL);
                    ui.hyperlink_to("Report issues on GitHub", ISSUES_URL);
                    ui.add_space(12.0);
                    if ui.button("OK").clicked() {
                        self.about_open = false;
                    }
                    // Donation thank-you shown only for licensed users.
                    if self.settings.is_licensed() {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("Thank you for donating to")
                                .size(14.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0xe0, 0xe0, 0xe0)),
                        );
                        ui.label(
                            egui::RichText::new("Glowing Cat Software!")
                                .size(14.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0xe0, 0xe0, 0xe0)),
                        );
                    }
                    ui.add_space(8.0);
                });
            });
        // Close via the window's X button or Escape.
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.about_open = false;
        }
    }

    /// The startup splash — a native rendering of TagTiger's `splash.html`:
    /// dark panel, icon, title, version, and a donate message with a link.
    /// Auto-closes after 20 seconds; also closes on a click anywhere except the
    /// donate link.
    fn splash_window(&mut self, ctx: &egui::Context) {
        let Some(until) = self.splash_until else {
            return;
        };
        // Auto-close after the timeout.
        if std::time::Instant::now() >= until {
            self.splash_until = None;
            return;
        }
        // Keep repainting so the timeout fires even without user input.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        let icon = self.ensure_icon(ctx);
        let mut link_clicked = false;
        egui::Window::new("TagTiger")
            .id(egui::Id::new("splash"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(12.0);
                    if let Some(tex) = &icon {
                        ui.add(
                            egui::Image::new(tex)
                                .fit_to_exact_size(egui::vec2(96.0, 96.0))
                                .corner_radius(20.0),
                        );
                    }
                    ui.add_space(8.0);
                    ui.label(title_text("TagTiger"));
                    ui.add_space(4.0);
                    ui.label(muted_text(&format!(
                        "Version {}",
                        env!("CARGO_PKG_VERSION")
                    )));
                    ui.add_space(8.0);
                    ui.label(body_text("If you enjoy using this product"));
                    ui.label(body_text("please consider donating to help"));
                    ui.label(body_text("fund this and other open source"));
                    // Final line: "projects at <link>." — rendered as one line
                    // with no inter-widget spacing, centered by measuring the
                    // actual text width (the pieces are separate widgets because
                    // only the middle one is a clickable link).
                    let pre = "projects at ";
                    let link = "Glowing Cat Software";
                    let post = ".";
                    let font = egui::FontId::proportional(14.0);
                    let text_w = |s: &str| {
                        ui.ctx().fonts_mut(|f| {
                            f.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                                .size()
                                .x
                        })
                    };
                    let total_w = text_w(pre) + text_w(link) + text_w(post);
                    ui.horizontal(|ui| {
                        // Remove the default gaps between the three pieces so the
                        // link sits flush against the surrounding text.
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let offset = ((ui.available_width() - total_w) / 2.0).max(0.0);
                        ui.add_space(offset);
                        ui.label(body_text(pre));
                        if ui.link(egui::RichText::new(link).size(14.0)).clicked() {
                            link_clicked = true;
                            let _ = webbrowser_open(GLOWING_CAT_URL);
                        }
                        ui.label(body_text(post));
                    });
                    ui.add_space(12.0);
                });
            });

        // Close on a click anywhere except the donate link.
        let clicked_anywhere = ctx.input(|i| i.pointer.any_click());
        if clicked_anywhere && !link_clicked {
            self.splash_until = None;
        }
    }

    /// The License Key dialog — a native rendering of TagTiger's
    /// `license_dialog.html`: email + key inputs with live validation, a donate
    /// link, and Cancel/Save. Save is enabled only when the key is valid for
    /// the entered email; saving persists the license to settings.
    fn license_window(&mut self, ctx: &egui::Context) {
        if !self.license_open {
            return;
        }
        let mut open = true;
        let mut do_save = false;
        let mut do_cancel = false;

        egui::Window::new("License Key")
            .id(egui::Id::new("license"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(title_text("License Key"));
                    ui.add_space(4.0);
                    ui.label(muted_text("Enter your email address and license key"));
                    ui.add_space(8.0);
                });

                // Email input.
                ui.add(
                    egui::TextEdit::singleline(&mut self.license_email_input)
                        .hint_text("Your email address")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);

                // Key input: reformat to XXXX-XXXX-XXXX-XXXX as the user types.
                let key_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.license_key_input)
                        .hint_text("XXXX-XXXX-XXXX-XXXX")
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
                if key_resp.changed() {
                    self.license_key_input =
                        crate::license_mgr::format_key(&self.license_key_input);
                }

                let valid = crate::license_mgr::is_valid(
                    &self.license_key_input,
                    &self.license_email_input,
                );

                ui.add_space(8.0);
                ui.hyperlink_to(
                    "Donate at Glowing Cat Software to get a license key.",
                    GLOWING_CAT_URL,
                );
                if !self.license_msg.is_empty() {
                    ui.add_space(4.0);
                    ui.label(muted_text(&self.license_msg));
                }
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                    if ui.add_enabled(valid, egui::Button::new("Save")).clicked() {
                        do_save = true;
                    }
                });
            });

        // Enter saves (when valid); Escape cancels.
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if enter && crate::license_mgr::is_valid(&self.license_key_input, &self.license_email_input)
        {
            do_save = true;
        }
        if escape {
            do_cancel = true;
        }

        if do_save {
            self.settings.license_email = self.license_email_input.trim().to_string();
            self.settings.license_key = crate::license_mgr::normalize_key(&self.license_key_input);
            match self.settings.save() {
                Ok(()) => {
                    self.status = "License saved. Thank you!".into();
                    self.license_open = false;
                    // A valid license suppresses the splash immediately.
                    self.splash_until = None;
                }
                Err(e) => {
                    self.license_msg = format!("Couldn't save settings: {e}");
                }
            }
        }
        if do_cancel || !open {
            self.license_open = false;
            self.license_msg.clear();
        }
    }

    /// Open the License Key dialog, prefilling the currently saved values.
    fn open_license_dialog(&mut self) {
        self.license_email_input = self.settings.license_email.clone();
        self.license_key_input = crate::license_mgr::format_key(&self.settings.license_key);
        self.license_msg.clear();
        self.license_open = true;
    }

    /// Open the Settings dialog, prefilling the saved TMDB Bearer token.
    fn open_settings_dialog(&mut self) {
        self.settings_token_input = self.settings.tmdb_bearer_token.clone();
        self.settings_open = true;
    }

    /// Settings dialog: enter a TMDB v4 read access token (Bearer). Saving
    /// persists it to settings and hands it to the worker so subsequent TMDB
    /// requests authenticate with it.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut do_save = false;
        let mut do_cancel = false;

        egui::Window::new("Settings")
            .id(egui::Id::new("settings"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(title_text("Settings"));
                    ui.add_space(8.0);
                });

                ui.label(body_text("TMDB Bearer Token"));
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings_token_input)
                        .hint_text("Paste your v4 read access token")
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                ui.hyperlink_to(
                    "To get a TMDB API Read Access Token…",
                    TMDB_API_SETTINGS_URL,
                );

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                    if ui.button("Save").clicked() {
                        do_save = true;
                    }
                });
            });

        // Enter saves; Escape cancels.
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if do_save || enter {
            let token = self.settings_token_input.trim().to_string();
            self.settings.tmdb_bearer_token = token.clone();
            match self.settings.save() {
                Ok(()) => self.status = "TMDB token saved.".into(),
                Err(e) => self.status = format!("Failed to save settings: {e}"),
            }
            // Hand the new credential to the worker (empty clears it, falling
            // back to environment variables).
            let _ = self.worker.tx.send(Request::SetBearerToken(token));
            self.settings_open = false;
        } else if do_cancel || escape || !open {
            self.settings_open = false;
        }
    }

    /// Message dialog shown when a TMDB search is attempted without any
    /// credential configured. Offers a shortcut into the Settings dialog.
    fn no_credential_window(&mut self, ctx: &egui::Context) {
        if !self.no_credential_open {
            return;
        }
        let mut open = true;
        let mut do_ok = false;
        let mut do_settings = false;

        egui::Window::new("TMDB credential required")
            .id(egui::Id::new("no_credential"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(title_text("TMDB credential required"));
                    ui.add_space(8.0);
                });
                ui.label(body_text(
                    "A TMDB account and API Read Access Token are required \
                     for TMDB searches.",
                ));
                ui.add_space(6.0);
                ui.label(body_text(
                    "Configure your TMDB Read Access Token in the Settings dialog.",
                ));
                ui.add_space(6.0);
                ui.hyperlink_to(
                    "To get a TMDB API Read Access Token…",
                    TMDB_API_SETTINGS_URL,
                );

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        do_ok = true;
                    }
                    if ui.button("Open Settings…").clicked() {
                        do_settings = true;
                    }
                });
            });

        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if do_settings {
            self.no_credential_open = false;
            self.open_settings_dialog();
        } else if do_ok || enter || escape || !open {
            self.no_credential_open = false;
        }
    }

    /// Install the `tagtiger` CLI onto the user's PATH (macOS).
    ///
    /// The CLI ships beside the GUI executable inside the notarized app bundle
    /// (`TagTiger.app/Contents/MacOS/tagtiger-cli`). Because this code runs from
    /// the already-notarized app, creating the symlink here avoids the
    /// Gatekeeper quarantine block that a loose `.command` script hits. We
    /// symlink `/usr/local/bin/tagtiger` -> that binary, escalating with an
    /// authorization prompt only when `/usr/local/bin` isn't writable.
    #[cfg(target_os = "macos")]
    fn install_cli(&mut self) {
        use std::path::PathBuf;

        // The CLI lives next to the running GUI executable.
        let cli = match std::env::current_exe() {
            Ok(exe) => exe
                .parent()
                .map(|dir| dir.join("tagtiger-cli"))
                .unwrap_or_else(|| PathBuf::from("tagtiger-cli")),
            Err(e) => {
                self.status = format!("Couldn't locate the app executable: {e}");
                return;
            }
        };
        if !cli.exists() {
            self.status =
                "Couldn't find the bundled CLI (tagtiger-cli) next to the app.".into();
            return;
        }

        let dest = "/usr/local/bin/tagtiger";
        let src = cli.to_string_lossy().to_string();

        // Fast path: /usr/local/bin exists and is writable — link directly.
        let bindir = std::path::Path::new("/usr/local/bin");
        let writable_no_sudo = bindir.exists()
            && std::fs::metadata(bindir)
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    // Writable by the current user in practice: try a probe.
                    m.permissions().mode() & 0o200 != 0
                })
                .unwrap_or(false)
            // A permission-bit check isn't authoritative (ownership matters), so
            // confirm with an actual write probe.
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
            // Replace any existing link/file, then create the symlink.
            let _ = std::fs::remove_file(dest);
            match std::os::unix::fs::symlink(&src, dest) {
                Ok(()) => {
                    self.status = format!("Installed CLI: run `tagtiger --help`. ({dest})");
                }
                Err(e) => self.status = format!("Failed to install CLI: {e}"),
            }
            return;
        }

        // Privileged path: prompt for admin rights via osascript. The shell
        // command creates /usr/local/bin if needed and (re)creates the symlink.
        // Paths are single-quoted for the shell; embedded single quotes in the
        // source path are escaped defensively.
        let esc = |s: &str| s.replace('\'', r"'\''");
        let shell_cmd = format!(
            "mkdir -p /usr/local/bin && ln -sf '{}' '{}'",
            esc(&src),
            esc(dest)
        );
        // osascript "do shell script ... with administrator privileges" wants a
        // string literal; escape backslashes and double quotes for AppleScript.
        let as_literal = shell_cmd.replace('\\', r"\\").replace('"', r#"\""#);
        let script = format!(
            "do shell script \"{as_literal}\" with administrator privileges"
        );

        match std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            Ok(out) if out.status.success() => {
                self.status = format!("Installed CLI: run `tagtiger --help`. ({dest})");
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                // User cancelling the auth prompt shows up as "User canceled."
                if err.contains("User canceled") || err.contains("(-128)") {
                    self.status = "CLI install cancelled.".into();
                } else {
                    self.status = format!("Failed to install CLI: {}", err.trim());
                }
            }
            Err(e) => self.status = format!("Failed to run installer: {e}"),
        }
    }
}

/// egui version string, for the About "Built with" line.
const EGUI_VERSION: &str = "0.36";
const GLOWING_CAT_URL: &str = "https://glowingcat.com/TagTiger.html";
const ISSUES_URL: &str = "https://github.com/richlesh/TagTiger/issues";
/// Where users generate a TMDB v4 read access token.
const TMDB_API_SETTINGS_URL: &str = "https://www.themoviedb.org/settings/api";

/// A dark dialog background matching TagTiger's `#1e1e1e` panels.
fn dialog_frame() -> egui::Frame {
    egui::Frame::window(&egui::Style::default())
        .fill(egui::Color32::from_rgb(0x1e, 0x1e, 0x1e))
        .inner_margin(egui::Margin::symmetric(28, 20))
}

/// Title text (`h1`): 20px, near-white.
fn title_text(s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .size(20.0)
        .strong()
        .color(egui::Color32::from_rgb(0xe0, 0xe0, 0xe0))
}

/// Body text (`#e0e0e0`), 14px.
fn body_text(s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .size(14.0)
        .color(egui::Color32::from_rgb(0xe0, 0xe0, 0xe0))
}

/// Muted secondary text (`#aaa`), 14px.
fn muted_text(s: &str) -> egui::RichText {
    egui::RichText::new(s)
        .size(14.0)
        .color(egui::Color32::from_rgb(0xaa, 0xaa, 0xaa))
}

/// Open a URL in the system browser. Best-effort; ignores failures.
fn webbrowser_open(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
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

/// Read plain text from the system clipboard, if any.
fn read_clipboard_text() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    clipboard.get_text().ok()
}

/// Write an encoded image (PNG/JPEG bytes) to the system clipboard as an image.
fn write_clipboard_image(bytes: &[u8]) -> Result<(), ()> {
    let img = image::load_from_memory(bytes).map_err(|_| ())?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data = arboard::ImageData {
        width: w,
        height: h,
        bytes: std::borrow::Cow::Owned(img.into_raw()),
    };
    let mut clipboard = arboard::Clipboard::new().map_err(|_| ())?;
    clipboard.set_image(data).map_err(|_| ())
}

/// Read an image from the system clipboard and return it encoded as PNG.
/// Returns `None` if the clipboard has no image or can't be read/encoded.
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
