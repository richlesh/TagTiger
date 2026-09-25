//! Background worker: runs a tokio runtime on its own thread and services
//! requests from the (single-threaded) UI. The UI sends [`Request`]s and
//! receives [`Event`]s without ever blocking on network or disk.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use tagtiger_core::{
    artwork::{self, EncodedArtwork},
    model::{
        Artwork, MediaKind, MediaKindMeta, MediaMetadata, MediaQuery, ProviderId, SearchResult,
        TvSeasonSummary, TvShowMatch,
    },
    naming, tag, MetadataProvider, TmdbProvider,
};

/// One show hit for the Matches tree root list.
#[allow(dead_code)]
pub struct TvShowHit {
    pub series_id: String,
    pub name: String,
    pub year: Option<i32>,
}

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
    /// Search TMDB for a user-typed movie title (top search box).
    Search {
        query: String,
    },
    /// Search TMDB for a TV show's episodes (the Show-field search button).
    /// Resolves the show by name, then:
    ///   - season set, episode not: lists every episode of that season;
    ///   - season+episode set: returns matching shows enriched with that
    ///     episode's name;
    ///   - neither: returns matching shows.
    SearchTvEpisodes {
        show: String,
        season: Option<u32>,
        episode: Option<u32>,
    },
    /// Search a show's episodes by (partial) episode title. Used by the Title
    /// field's search button when the media kind is TV Show.
    SearchEpisodeByTitle {
        show: String,
        season: Option<u32>,
        title: String,
    },
    /// Search across ALL shows matching `show` and ALL of their seasons and
    /// episodes, filtering episodes by `title`, and return a hierarchical
    /// Show → Season → Episode structure for the Matches tree.
    SearchEpisodeTreeByTitle {
        show: String,
        title: String,
    },
    /// Search shows by (partial) name for the Matches tree. Returns every
    /// matching show as a tree root. `season_filter`, when set, is echoed back
    /// so each show's seasons can later be restricted to that one season.
    SearchShows {
        show: String,
        season_filter: Option<u32>,
    },
    /// Lazily fetch a show's season summaries when its tree root is expanded.
    FetchShowSeasons {
        series_id: String,
        season_filter: Option<u32>,
    },
    /// Fetch a show's poster artwork (show-level, cached by the controller).
    FetchShowPosters {
        series_id: String,
    },
    /// Lazily fetch the episode (number, name) list for one season of a show,
    /// used when a season node in the tree is expanded.
    FetchSeasonEpisodes {
        series_id: String,
        season: u32,
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
    /// The list of matching shows for the Matches tree roots is ready.
    TvShowsReady {
        shows: Vec<TvShowHit>,
        season_filter: Option<u32>,
    },
    /// A filtered episode-title tree is ready: matching shows, each with the
    /// seasons/episodes that matched, for the Matches tree.
    TvTreeFilteredReady {
        shows: Vec<TvShowMatch>,
    },
    /// A show's season summaries are ready (lazy expand of a show root).
    ShowSeasonsReady {
        series_id: String,
        seasons: Vec<TvSeasonSummary>,
        season_filter: Option<u32>,
    },
    /// A show's poster artwork is ready (show-level; cached by the controller).
    ShowPostersReady {
        series_id: String,
        artwork: Vec<Artwork>,
    },
    /// A season's episode (number, name) list is ready (lazy expand).
    SeasonEpisodesReady {
        series_id: String,
        season: u32,
        episodes: Vec<(u32, String)>,
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
        Request::SearchEpisodeByTitle {
            show,
            season,
            title,
        } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let results = provider
                .search_episodes_by_title(&show, season, &title)
                .await?;
            Ok(Some(Event::SearchDone { results }))
        }
        Request::SearchEpisodeTreeByTitle { show, title } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let shows = provider.search_episode_tree_by_title(&show, &title).await?;
            Ok(Some(Event::TvTreeFilteredReady { shows }))
        }
        Request::SearchShows {
            show,
            season_filter,
        } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let mquery = MediaQuery {
                title: show,
                year: None,
                kind: MediaKind::TvShow,
                season: None,
                episode: None,
            };
            let hits = provider.search(&mquery).await?;
            let shows = hits
                .into_iter()
                .map(|h| TvShowHit {
                    series_id: h.id.id,
                    name: h.title,
                    year: h.year,
                })
                .collect();
            Ok(Some(Event::TvShowsReady {
                shows,
                season_filter,
            }))
        }
        Request::FetchShowSeasons {
            series_id,
            season_filter,
        } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let (_show_name, mut seasons) = provider.tv_seasons(&series_id).await?;
            if let Some(season) = season_filter {
                seasons.retain(|s| s.season_number == season);
            }
            Ok(Some(Event::ShowSeasonsReady {
                series_id,
                seasons,
                season_filter,
            }))
        }
        Request::FetchShowPosters { series_id } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let artwork = provider
                .tv_show_posters(&series_id)
                .await
                .unwrap_or_default();
            Ok(Some(Event::ShowPostersReady { series_id, artwork }))
        }
        Request::FetchSeasonEpisodes { series_id, season } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let episodes = provider.season_episode_names(&series_id, season).await?;
            Ok(Some(Event::SeasonEpisodesReady {
                series_id,
                season,
                episodes,
            }))
        }
        Request::SearchTvEpisodes {
            show,
            season,
            episode,
        } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            let mquery = MediaQuery {
                title: show,
                year: None,
                kind: MediaKind::TvShow,
                season: None,
                episode: None,
            };
            let mut results = provider.search(&mquery).await?;

            match (season, episode) {
                // Season known, no episode: list every episode of that season
                // for the best-matching show (the first search hit).
                (Some(season), None) => {
                    if let Some(first) = results.first() {
                        let series_id = first.id.id.clone();
                        let show_name = first.title.clone();
                        match provider
                            .list_season_episodes(&series_id, &show_name, season)
                            .await
                        {
                            Ok(episodes) if !episodes.is_empty() => {
                                results = episodes;
                            }
                            // No episodes (or error): fall back to the show hits,
                            // carrying the season so a later selection still knows it.
                            _ => {
                                for r in results.iter_mut() {
                                    r.id.season = Some(season);
                                }
                            }
                        }
                    }
                }
                // Season+episode known: enrich show hits with the episode name.
                (Some(season), Some(episode)) => {
                    const MAX_ENRICH: usize = 12;
                    let lookups = results.iter().take(MAX_ENRICH).map(|r| {
                        let provider = provider.clone();
                        let id = r.id.id.clone();
                        async move {
                            provider
                                .episode_name(&id, season, episode)
                                .await
                                .ok()
                                .flatten()
                        }
                    });
                    let names = futures::future::join_all(lookups).await;
                    for (r, name) in results.iter_mut().zip(names) {
                        r.id.season = Some(season);
                        r.id.episode = Some(episode);
                        r.episode_name = name;
                    }
                }
                // Neither: plain show search.
                _ => {}
            }
            Ok(Some(Event::SearchDone { results }))
        }
        Request::FetchDetails { mut id, file } => {
            let provider =
                provider.ok_or_else(|| anyhow::anyhow!("Set TMDB_BEARER_TOKEN or TMDB_API_KEY"))?;
            // For TV, make sure the id carries a season/episode so the provider
            // can fetch the specific episode's title/overview/air date/stills.
            // The GUI supplies these from its TV fields; fall back to the values
            // parsed from the filename when it didn't.
            if id.kind == MediaKind::TvShow && (id.season.is_none() || id.episode.is_none()) {
                if let Ok(q) = naming::parse(&file) {
                    id.season = id.season.or(q.season);
                    id.episode = id.episode.or(q.episode);
                }
            }
            let mut meta = provider.fetch_details(&id).await?;
            // Ensure the episode numbers are reflected even if the provider
            // couldn't fetch the episode (e.g. sparse data): prefer the id's,
            // else the filename's.
            if let MediaKindMeta::Episode(ref mut ep) = meta.kind {
                if ep.season == 0 {
                    ep.season = id.season.unwrap_or(0);
                }
                if ep.episode == 0 {
                    ep.episode = id.episode.unwrap_or(0);
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
    let (mut meta, cover_bytes) = tag::read_from_file(&file).unwrap_or_default();
    // Pre-fill Show / Season / Episode from a `Show - sXXeXX`-style filename
    // when those aren't already set by the file's atoms.
    prefill_episode_from_filename(&mut meta, &file);
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

/// If `file`'s name matches the episode pattern (e.g. `Show - S01E02`),
/// back-fill the Show / Season / Episode fields that the file's atoms didn't
/// already provide. Values present in the atoms (a non-empty show name, or a
/// non-zero season/episode) are never overwritten.
fn prefill_episode_from_filename(meta: &mut MediaMetadata, file: &std::path::Path) {
    use tagtiger_core::model::EpisodeInfo;

    // Only act on filenames the parser recognizes as episodes.
    let Ok(q) = naming::parse(file) else {
        return;
    };
    if q.kind != MediaKind::TvShow {
        return;
    }

    // Start from any episode info the atoms produced; otherwise a blank one.
    let mut ep = match std::mem::take(&mut meta.kind) {
        MediaKindMeta::Episode(ep) => ep,
        MediaKindMeta::Movie => EpisodeInfo {
            show_name: String::new(),
            season: 0,
            episode: 0,
            episode_title: None,
            network: None,
            episode_id: None,
        },
    };

    // Fill only the fields the atoms left unset.
    if ep.show_name.trim().is_empty() && !q.title.trim().is_empty() {
        ep.show_name = q.title.clone();
    }
    if ep.season == 0 {
        if let Some(season) = q.season {
            ep.season = season;
        }
    }
    if ep.episode == 0 {
        if let Some(episode) = q.episode {
            ep.episode = episode;
        }
    }

    meta.kind = MediaKindMeta::Episode(ep);
    // Reveal the TV fields in the editor when the file didn't explicitly set a
    // media kind (`stik`). An explicit atom kind is left untouched.
    if meta.video_kind.is_none() {
        meta.video_kind = Some(tagtiger_core::model::VideoKind::TvShow);
    }
}

#[cfg(test)]
mod prefill_tests {
    use super::*;
    use std::path::Path;
    use tagtiger_core::model::{EpisodeInfo, VideoKind};

    #[test]
    fn fills_show_season_episode_from_episode_filename() {
        let mut meta = MediaMetadata::default();
        prefill_episode_from_filename(&mut meta, Path::new("Breaking Bad - S01E02.mp4"));
        match meta.kind {
            MediaKindMeta::Episode(ep) => {
                assert_eq!(ep.show_name, "Breaking Bad");
                assert_eq!(ep.season, 1);
                assert_eq!(ep.episode, 2);
            }
            _ => panic!("expected Episode kind"),
        }
        // No stik atom -> the kind is set to TV Show so the fields show.
        assert_eq!(meta.video_kind, Some(VideoKind::TvShow));
    }

    #[test]
    fn does_not_overwrite_existing_atom_values() {
        let mut meta = MediaMetadata {
            video_kind: Some(VideoKind::TvShow),
            kind: MediaKindMeta::Episode(EpisodeInfo {
                show_name: "Atom Show".into(),
                season: 5,
                episode: 9,
                episode_title: Some("From Atoms".into()),
                network: Some("HBO".into()),
                episode_id: Some("5x09".into()),
            }),
            ..Default::default()
        };
        prefill_episode_from_filename(&mut meta, Path::new("Breaking Bad - S01E02.mp4"));
        match meta.kind {
            MediaKindMeta::Episode(ep) => {
                // Existing atom values are preserved.
                assert_eq!(ep.show_name, "Atom Show");
                assert_eq!(ep.season, 5);
                assert_eq!(ep.episode, 9);
                assert_eq!(ep.episode_title.as_deref(), Some("From Atoms"));
                assert_eq!(ep.network.as_deref(), Some("HBO"));
            }
            _ => panic!("expected Episode kind"),
        }
    }

    #[test]
    fn fills_only_missing_fields() {
        // Atoms carried a show name but no season/episode (both 0).
        let mut meta = MediaMetadata {
            kind: MediaKindMeta::Episode(EpisodeInfo {
                show_name: "Kept Name".into(),
                season: 0,
                episode: 0,
                episode_title: None,
                network: None,
                episode_id: None,
            }),
            ..Default::default()
        };
        prefill_episode_from_filename(&mut meta, Path::new("Breaking Bad - S03E10.mp4"));
        match meta.kind {
            MediaKindMeta::Episode(ep) => {
                assert_eq!(ep.show_name, "Kept Name"); // preserved
                assert_eq!(ep.season, 3); // filled
                assert_eq!(ep.episode, 10); // filled
            }
            _ => panic!("expected Episode kind"),
        }
    }

    #[test]
    fn movie_filename_leaves_kind_unchanged() {
        let mut meta = MediaMetadata::default();
        prefill_episode_from_filename(&mut meta, Path::new("The Matrix (1999).mp4"));
        assert!(matches!(meta.kind, MediaKindMeta::Movie));
        assert_eq!(meta.video_kind, None);
    }
}
