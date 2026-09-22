//! Background worker: runs a tokio runtime on its own thread and services
//! requests from the (single-threaded) UI. The UI sends [`Request`]s and
//! receives [`Event`]s without ever blocking on network or disk.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use tagtiger_core::{
    artwork::{self, EncodedArtwork},
    model::{MediaKind, MediaKindMeta, MediaMetadata, MediaQuery, ProviderId, SearchResult},
    naming, tag, MetadataProvider, TmdbProvider,
};

/// Requests the UI sends to the worker.
//
// The worker integration test includes this file standalone via `#[path]`, so
// from that compilation unit's view the variants constructed only by the
// controller (app.rs) look "never constructed". They're fully used in the
// binary; allow dead_code so `-D warnings` doesn't trip on the test build.
#[allow(dead_code)]
pub enum Request {
    /// Open a native file picker (async, off the UI thread) and load the
    /// existing tags + cover art from the chosen file.
    PickFile,
    /// Open a specific file path (e.g. from drag-and-drop or the app icon).
    OpenFile {
        path: PathBuf,
    },
    /// Search TMDB for a user-typed movie title.
    Search {
        query: String,
    },
    FetchDetails {
        id: ProviderId,
        file: PathBuf,
    },
    FetchThumb {
        index: usize,
        url: String,
    },
    /// Fetch a larger image for the lightbox viewer.
    FetchFullImage {
        index: usize,
        url: String,
    },
    /// Download a full-resolution poster and make it the current cover.
    SetCoverFromUrl {
        url: String,
    },
    /// Download a poster (e.g. a selected TMDB grid poster) and hand its bytes
    /// back so the UI thread can place it on the system clipboard.
    CopyPosterToClipboard {
        url: String,
    },
    WriteTags {
        file: PathBuf,
        meta: Box<MediaMetadata>,
        artwork_url: Option<String>,
        /// Raw image bytes (PNG/JPEG) to use as the cover, overriding
        /// `artwork_url`. Set when the user pasted/dropped a poster.
        cover_override: Option<Vec<u8>>,
        /// Whether to write a fast-start (moov-first) layout. When false, moov
        /// is placed after mdat.
        fast_start: bool,
    },
    /// Replace the TMDB credential at runtime with a v4 Bearer token entered in
    /// the Settings dialog. An empty string clears it and falls back to the
    /// `TMDB_BEARER_TOKEN` / `TMDB_API_KEY` environment variables.
    SetBearerToken(String),
}

/// Events the worker sends back to the UI.
// See the note on `Request`: the integration test includes this file
// standalone, so variants/fields consumed only by the controller look unused.
#[allow(dead_code)]
pub enum Event {
    /// A file was opened: its existing metadata, an optional decoded cover
    /// thumbnail (rgba), and a suggested search string (existing title or the
    /// filename stem).
    FileLoaded {
        file: PathBuf,
        meta: Box<MediaMetadata>,
        suggested_query: String,
        cover: Option<(u32, u32, Vec<u8>)>,
        /// Original pixel dimensions of the file's cover, if any.
        cover_size: Option<(u32, u32)>,
        /// Raw encoded bytes of the file's cover (for clipboard/undo).
        cover_bytes: Option<Vec<u8>>,
        /// Video track pixel dimensions (width, height), if detectable.
        video_dimensions: Option<(u32, u32)>,
        /// Whether the file is currently a fast-start (moov-first) file.
        fast_start: bool,
    },
    SearchDone {
        results: Vec<SearchResult>,
    },
    DetailsDone {
        meta: Box<MediaMetadata>,
    },
    ThumbDone {
        index: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    FullImageDone {
        index: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// A poster was chosen as the current cover: decoded display image, its
    /// original dimensions, and the raw bytes to embed when writing.
    CoverSet {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        orig_size: (u32, u32),
        bytes: Vec<u8>,
    },
    /// Raw poster bytes to place on the system clipboard (UI thread does the
    /// actual clipboard write). Used by the grid-poster Copy action.
    CopyToClipboard {
        bytes: Vec<u8>,
    },
    WriteDone {
        file: PathBuf,
    },
    /// A shift-save started (streaming media to a temp file).
    WriteStarted,
    /// Progress of a shift-save: (phase_label, bytes_done, bytes_total).
    WriteProgress(&'static str, u64, u64),
    Error(String),
}

pub struct Worker {
    pub tx: Sender<Request>,
    pub rx: Receiver<Event>,
}

impl Worker {
    /// Spawn the worker thread. `repaint` is called after each event to wake
    /// the UI event loop so it processes the event. `initial_token` is the
    /// saved TMDB Bearer token from settings (empty to fall back to the env).
    pub fn spawn(initial_token: String, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<Event>();
        let repaint = Arc::new(repaint);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let client = reqwest::Client::new();
            let mut provider: Option<Arc<TmdbProvider>> = build_provider(&initial_token);

            while let Ok(req) = req_rx.recv() {
                // Credential updates are handled inline so they mutate the
                // loop-owned provider; everything else is dispatched to handle().
                if let Request::SetBearerToken(token) = req {
                    provider = build_provider(&token);
                    repaint();
                    continue;
                }
                let evt_tx = evt_tx.clone();
                let client = client.clone();
                let provider = provider.clone();
                let repaint_ref = repaint.clone();
                rt.block_on(async {
                    let result = handle(req, provider, client, &evt_tx, &*repaint_ref).await;
                    match result {
                        Ok(Some(evt)) => {
                            let _ = evt_tx.send(evt);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let _ = evt_tx.send(Event::Error(e.to_string()));
                        }
                    }
                });
                repaint_ref();
            }
        });

        Worker {
            tx: req_tx,
            rx: evt_rx,
        }
    }
}

/// Build a TMDB provider from a saved Bearer token, falling back to the
/// environment (`TMDB_BEARER_TOKEN` / `TMDB_API_KEY`) when the token is empty.
fn build_provider(token: &str) -> Option<Arc<TmdbProvider>> {
    let token = token.trim();
    if !token.is_empty() {
        Some(Arc::new(TmdbProvider::with_bearer(token)))
    } else {
        TmdbProvider::from_env().ok().map(Arc::new)
    }
}

async fn handle(
    req: Request,
    provider: Option<Arc<TmdbProvider>>,
    client: reqwest::Client,
    evt_tx: &Sender<Event>,
    repaint: &(dyn Fn() + Send + Sync),
) -> anyhow::Result<Option<Event>> {
    match req {
        Request::PickFile => {
            // Async dialog runs off the UI thread, avoiding the nested-native-
            // run-loop panic that occurs when a blocking rfd dialog is called
            // from inside the UI's event loop on macOS.
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("MP4/M4V", &["mp4", "m4v"])
                .pick_file()
                .await;
            let Some(handle) = picked else {
                // User cancelled; nothing to report.
                return Ok(None);
            };
            Ok(Some(load_file(handle.path().to_path_buf())))
        }
        Request::OpenFile { path } => Ok(Some(load_file(path))),
        Request::Search { query } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let mquery = MediaQuery {
                title: query,
                year: None,
                kind: MediaKind::Movie,
                season: None,
                episode: None,
            };
            let results = provider.search(&mquery).await?;
            Ok(Some(Event::SearchDone { results }))
        }
        Request::FetchDetails { id, file } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let mut meta = provider.fetch_details(&id).await?;
            if let MediaKindMeta::Episode(ref mut ep) = meta.kind {
                if let Ok(q) = naming::parse(&file) {
                    ep.season = q.season.unwrap_or(ep.season);
                    ep.episode = q.episode.unwrap_or(ep.episode);
                }
            }
            Ok(Some(Event::DetailsDone {
                meta: Box::new(meta),
            }))
        }
        Request::FetchThumb { index, url } => {
            let bytes = artwork::download(&client, &url).await?;
            let thumb = artwork::thumbnail(&bytes, 200)?;
            Ok(Some(Event::ThumbDone {
                index,
                width: thumb.width,
                height: thumb.height,
                rgba: thumb.rgba,
            }))
        }
        Request::FetchFullImage { index, url } => {
            let bytes = artwork::download(&client, &url).await?;
            // Bound the decoded size so a huge poster can't exhaust memory,
            // while still being large enough for an enlarged view.
            let full = artwork::thumbnail(&bytes, 1200)?;
            Ok(Some(Event::FullImageDone {
                index,
                width: full.width,
                height: full.height,
                rgba: full.rgba,
            }))
        }
        Request::SetCoverFromUrl { url } => {
            let bytes = artwork::download(&client, &url).await?;
            let orig_size = artwork::dimensions(&bytes).unwrap_or((0, 0));
            let disp = artwork::thumbnail(&bytes, 1000)?;
            Ok(Some(Event::CoverSet {
                width: disp.width,
                height: disp.height,
                rgba: disp.rgba,
                orig_size,
                bytes,
            }))
        }
        Request::CopyPosterToClipboard { url } => {
            let bytes = artwork::download(&client, &url).await?;
            Ok(Some(Event::CopyToClipboard { bytes }))
        }
        Request::WriteTags {
            file,
            meta,
            artwork_url,
            cover_override,
            fast_start,
        } => {
            let encoded: Option<EncodedArtwork> = if let Some(bytes) = cover_override {
                // Pasted/dropped image takes priority.
                Some(artwork::normalize_for_cover(&bytes)?)
            } else if let Some(url) = artwork_url {
                let bytes = artwork::download(&client, &url).await?;
                Some(artwork::normalize_for_cover(&bytes)?)
            } else {
                None
            };

            // A shift save streams the media with progress; an in-place save
            // never calls the progress callback. Announce a start only once.
            let mut announced = false;
            let mut on_progress = |phase: tag::WritePhase, done: u64, total: u64| {
                if !announced {
                    announced = true;
                    let _ = evt_tx.send(Event::WriteStarted);
                }
                let _ = evt_tx.send(Event::WriteProgress(phase.label(), done, total));
                repaint();
            };
            tag::write_to_file_with_progress(
                &file,
                &meta,
                encoded.as_ref(),
                fast_start,
                &mut on_progress,
            )?;
            Ok(Some(Event::WriteDone { file }))
        }
        // Handled inline in the worker loop (mutates the provider); never
        // dispatched here.
        Request::SetBearerToken(_) => Ok(None),
    }
}

/// Load a file's tags, cover, and dimensions into a `FileLoaded` event.
fn load_file(file: PathBuf) -> Event {
    // Read existing tags + cover art (best effort).
    let (meta, cover_bytes) = tag::read_from_file(&file).unwrap_or_default();
    // Video track dimensions (best effort).
    let video_dimensions = tagtiger_core::mp4dim::video_dimensions(&file);
    // Whether the file is currently fast-start (moov before mdat).
    let fast_start = tagtiger_core::mp4rewrite::is_fast_start(&file).unwrap_or(false);

    // Suggested search: existing title, else the filename stem.
    let suggested_query = if !meta.title.trim().is_empty() {
        meta.title.clone()
    } else {
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };

    // Decode the cover for display (thumbnail + lightbox share this texture).
    let cover = match cover_bytes.as_deref().map(|b| artwork::thumbnail(b, 1000)) {
        Some(Ok(img)) => Some((img.width, img.height, img.rgba)),
        _ => None,
    };
    let cover_size = cover_bytes
        .as_deref()
        .and_then(|b| artwork::dimensions(b).ok());

    Event::FileLoaded {
        file,
        meta: Box::new(meta),
        suggested_query,
        cover,
        cover_size,
        cover_bytes,
        video_dimensions,
        fast_start,
    }
}
