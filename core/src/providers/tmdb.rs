//! TMDB (The Movie Database) provider.
//!
//! Uses the v3 REST data endpoints. Authentication uses a TMDB **v4 read
//! access token** (Bearer) when available, falling back to a v3 API key.
//! Configure via `TMDB_BEARER_TOKEN` (preferred) or `TMDB_API_KEY`. Note
//! TMDB's attribution requirements and rate limits when shipping.

use crate::error::{Error, Result};
use crate::model::{
    Artwork, EpisodeInfo, MediaKind, MediaKindMeta, MediaMetadata, MediaQuery, Person, ProviderId,
    SearchResult, TvEpisodeHit, TvSeasonMatch, TvSeasonSummary, TvShowMatch,
};
use async_trait::async_trait;
use chrono::NaiveDate;
use serde::Deserialize;

const API_BASE: &str = "https://api.themoviedb.org/3";
/// Base URL for images; `w342` is a good poster thumbnail size, `original`
/// for full artwork.
const IMG_BASE: &str = "https://image.tmdb.org/t/p";

/// How requests authenticate to TMDB.
#[derive(Clone)]
enum Auth {
    /// v4 read access token, sent as `Authorization: Bearer <token>`.
    Bearer(String),
    /// Legacy v3 API key, sent as an `api_key` query parameter.
    ApiKey(String),
}

pub struct TmdbProvider {
    auth: Auth,
    client: reqwest::Client,
}

impl TmdbProvider {
    /// Construct with a v3 API key (legacy auth).
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            auth: Auth::ApiKey(api_key.into()),
            client: reqwest::Client::new(),
        }
    }

    /// Construct with a v4 read access token (Bearer auth).
    pub fn with_bearer(token: impl Into<String>) -> Self {
        Self {
            auth: Auth::Bearer(token.into()),
            client: reqwest::Client::new(),
        }
    }

    /// Construct from the environment. Prefers `TMDB_BEARER_TOKEN` (a v4 read
    /// access token); falls back to `TMDB_API_KEY` (a v3 API key).
    pub fn from_env() -> Result<Self> {
        if let Ok(token) = std::env::var("TMDB_BEARER_TOKEN") {
            if !token.trim().is_empty() {
                return Ok(Self::with_bearer(token));
            }
        }
        let key = std::env::var("TMDB_API_KEY").map_err(|_| Error::MissingApiKey("tmdb".into()))?;
        Ok(Self::new(key))
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Build a GET request to a TMDB v3 endpoint with authentication applied.
    fn authed_get(&self, url: String) -> reqwest::RequestBuilder {
        let req = self.client.get(url);
        match &self.auth {
            Auth::Bearer(token) => req.bearer_auth(token),
            Auth::ApiKey(key) => req.query(&[("api_key", key.as_str())]),
        }
    }

    fn image_url(path: &Option<String>, size: &str) -> Option<String> {
        path.as_ref().map(|p| format!("{IMG_BASE}/{size}{p}"))
    }

    /// Fetch just the episode name for a given show/season/episode. Used to
    /// enrich TV search results (which are show-level) with the episode title
    /// for display. Returns `Ok(None)` when TMDB has no name for the episode.
    pub async fn episode_name(
        &self,
        series_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Option<String>> {
        let ep: TmdbEpisodeName = self
            .authed_get(format!(
                "{API_BASE}/tv/{series_id}/season/{season}/episode/{episode}"
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(ep.name.filter(|n| !n.trim().is_empty()))
    }

    /// List all episodes of a show's season as search hits, one per episode.
    /// Each result carries the show/season/episode in its `ProviderId` so
    /// selecting it fetches that specific episode's full details. `show_name`
    /// is used as the result title (the season endpoint doesn't repeat it).
    pub async fn list_season_episodes(
        &self,
        series_id: &str,
        show_name: &str,
        season: u32,
    ) -> Result<Vec<SearchResult>> {
        let detail: TmdbSeasonDetail = self
            .authed_get(format!("{API_BASE}/tv/{series_id}/season/{season}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(detail
            .episodes
            .into_iter()
            .map(|e| SearchResult {
                id: ProviderId {
                    provider: "tmdb".into(),
                    id: series_id.to_string(),
                    kind: MediaKind::TvShow,
                    season: Some(season),
                    episode: Some(e.episode_number),
                },
                title: show_name.to_string(),
                year: e.air_date.as_deref().and_then(parse_year),
                overview: e.overview,
                poster_thumb_url: Self::image_url(&e.still_path, "w342"),
                episode_name: e.name.filter(|n| !n.trim().is_empty()),
            })
            .collect())
    }

    /// Fetch a show's name and its season summaries, for building the Matches
    /// tree. Only seasons with at least one episode are returned, sorted by
    /// season number.
    pub async fn tv_seasons(&self, series_id: &str) -> Result<(String, Vec<TvSeasonSummary>)> {
        let detail: TmdbTvSeasonsDetail = self
            .authed_get(format!("{API_BASE}/tv/{series_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut seasons: Vec<TvSeasonSummary> = detail
            .seasons
            .into_iter()
            .filter(|s| s.episode_count > 0)
            .map(|s| TvSeasonSummary {
                season_number: s.season_number,
                name: s
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Season {}", s.season_number)),
                episode_count: s.episode_count,
            })
            .collect();
        seasons.sort_by_key(|s| s.season_number);
        Ok((detail.name, seasons))
    }

    /// Fetch a show's poster artwork (the primary poster plus the `images`
    /// posters gallery). These are show-level and identical regardless of
    /// season/episode, so the caller can fetch them once per show and cache.
    pub async fn tv_show_posters(&self, series_id: &str) -> Result<Vec<Artwork>> {
        let detail: TmdbTvDetail = self
            .authed_get(format!("{API_BASE}/tv/{series_id}"))
            .query(&[("append_to_response", "images")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(collect_artwork(&detail.poster_path, detail.images.as_ref()))
    }

    /// Fetch (episode_number, episode_name) pairs for one season, for the tree's
    /// leaf nodes. Missing names fall back to "Episode N".
    pub async fn season_episode_names(
        &self,
        series_id: &str,
        season: u32,
    ) -> Result<Vec<(u32, String)>> {
        let detail: TmdbSeasonDetail = self
            .authed_get(format!("{API_BASE}/tv/{series_id}/season/{season}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(detail
            .episodes
            .into_iter()
            .map(|e| {
                let name = e
                    .name
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Episode {}", e.episode_number));
                (e.episode_number, name)
            })
            .collect())
    }

    /// Search a show's episodes by (partial, case-insensitive) episode title.
    /// Resolves the show by name (first hit), then scans its seasons — the one
    /// given `season` if provided, otherwise every season (bounded) — and
    /// returns each episode whose name contains `title` as a `SearchResult`
    /// (carrying season/episode so a selection fetches that episode).
    pub async fn search_episodes_by_title(
        &self,
        show: &str,
        season: Option<u32>,
        title: &str,
    ) -> Result<Vec<SearchResult>> {
        // Resolve the show.
        let mquery = MediaQuery {
            title: show.to_string(),
            year: None,
            kind: MediaKind::TvShow,
            season: None,
            episode: None,
        };
        let hits = self.search(&mquery).await?;
        let Some(first) = hits.into_iter().next() else {
            return Ok(Vec::new());
        };
        let series_id = first.id.id.clone();
        let (show_name, seasons) = self.tv_seasons(&series_id).await?;

        // Which seasons to scan.
        let season_numbers: Vec<u32> = match season {
            Some(s) => vec![s],
            // Bound the number of season fetches for shows with many seasons.
            None => seasons.iter().map(|s| s.season_number).take(50).collect(),
        };

        let needle = title.trim().to_lowercase();
        let mut out = Vec::new();
        for s in season_numbers {
            // Reuse the season-episode listing (SearchResults with ids set).
            let episodes = self
                .list_season_episodes(&series_id, &show_name, s)
                .await
                .unwrap_or_default();
            for ep in episodes {
                // Match on the episode name when we have one; when the title
                // needle is empty, include all episodes of the scanned seasons.
                if episode_title_matches(ep.episode_name.as_deref(), &needle) {
                    out.push(ep);
                }
            }
        }
        Ok(out)
    }

    /// Search across ALL shows matching `show` and ALL of their seasons and
    /// episodes, keeping only episodes whose title matches `title` (an empty
    /// title keeps every episode). Returns a hierarchical structure
    /// (Show → Season → Episode) containing only shows/seasons with matches.
    ///
    /// Bounded for interactivity: at most 8 shows and 50 seasons per show.
    pub async fn search_episode_tree_by_title(
        &self,
        show: &str,
        title: &str,
    ) -> Result<Vec<TvShowMatch>> {
        const MAX_SHOWS: usize = 8;
        const MAX_SEASONS: usize = 50;

        let mquery = MediaQuery {
            title: show.to_string(),
            year: None,
            kind: MediaKind::TvShow,
            season: None,
            episode: None,
        };
        let hits = self.search(&mquery).await?;
        let needle = title.trim().to_lowercase();

        let mut out: Vec<TvShowMatch> = Vec::new();
        for hit in hits.into_iter().take(MAX_SHOWS) {
            let series_id = hit.id.id.clone();
            let (show_name, seasons) = match self.tv_seasons(&series_id).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let mut season_matches: Vec<TvSeasonMatch> = Vec::new();
            for summary in seasons.into_iter().take(MAX_SEASONS) {
                let episodes = self
                    .season_episode_names(&series_id, summary.season_number)
                    .await
                    .unwrap_or_default();
                let matching = filter_matching_episodes(episodes, &needle);
                if !matching.is_empty() {
                    season_matches.push(TvSeasonMatch {
                        season_number: summary.season_number,
                        name: summary.name,
                        episodes: matching,
                    });
                }
            }
            if !season_matches.is_empty() {
                // Prefer the search hit's title; fall back to the detail name.
                let name = if hit.title.trim().is_empty() {
                    show_name
                } else {
                    hit.title.clone()
                };
                out.push(TvShowMatch {
                    series_id,
                    show_name: name,
                    year: hit.year,
                    seasons: season_matches,
                });
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl MetadataProvider for TmdbProvider {
    fn name(&self) -> &str {
        "tmdb"
    }

    async fn search(&self, query: &MediaQuery) -> Result<Vec<SearchResult>> {
        match query.kind {
            MediaKind::Movie => {
                let mut req = self
                    .authed_get(format!("{API_BASE}/search/movie"))
                    .query(&[("query", &query.title)]);
                if let Some(year) = query.year {
                    req = req.query(&[("year", year.to_string())]);
                }
                let resp: TmdbSearch<TmdbMovieHit> =
                    req.send().await?.error_for_status()?.json().await?;
                Ok(resp
                    .results
                    .into_iter()
                    .map(|m| SearchResult {
                        id: ProviderId {
                            provider: "tmdb".into(),
                            id: m.id.to_string(),
                            kind: MediaKind::Movie,
                            season: None,
                            episode: None,
                        },
                        title: m.title,
                        year: m.release_date.as_deref().and_then(parse_year),
                        overview: m.overview,
                        poster_thumb_url: Self::image_url(&m.poster_path, "w342"),
                        episode_name: None,
                    })
                    .collect())
            }
            MediaKind::TvShow => {
                let resp: TmdbSearch<TmdbTvHit> = self
                    .authed_get(format!("{API_BASE}/search/tv"))
                    .query(&[("query", &query.title)])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                Ok(resp
                    .results
                    .into_iter()
                    .map(|t| SearchResult {
                        id: ProviderId {
                            provider: "tmdb".into(),
                            id: t.id.to_string(),
                            kind: MediaKind::TvShow,
                            season: None,
                            episode: None,
                        },
                        title: t.name,
                        year: t.first_air_date.as_deref().and_then(parse_year),
                        overview: t.overview,
                        poster_thumb_url: Self::image_url(&t.poster_path, "w342"),
                        episode_name: None,
                    })
                    .collect())
            }
        }
    }

    async fn fetch_details(&self, id: &ProviderId) -> Result<MediaMetadata> {
        match id.kind {
            MediaKind::Movie => {
                let detail: TmdbMovieDetail = self
                    .authed_get(format!("{API_BASE}/movie/{}", id.id))
                    .query(&[("append_to_response", "credits,images,release_dates")])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;

                let credits = detail.credits.unwrap_or_default();
                Ok(MediaMetadata {
                    title: detail.title,
                    release_date: detail.release_date.as_deref().and_then(parse_date),
                    summary: pick_summary(&detail.tagline, &detail.overview),
                    overview: detail.overview,
                    genres: detail.genres.into_iter().map(|g| g.name).collect(),
                    cast: credits
                        .cast
                        .into_iter()
                        .take(20)
                        .map(person_from_cast)
                        .collect(),
                    directors: credits
                        .crew
                        .iter()
                        .filter(|c| c.job.as_deref() == Some("Director"))
                        .map(|c| Person::new(c.name.clone()))
                        .collect(),
                    producers: crew_people(&credits.crew, is_producer),
                    writers: crew_people(&credits.crew, is_writer),
                    content_rating: us_movie_certification(detail.release_dates.as_ref()),
                    video_kind: Some(crate::model::VideoKind::Movie),
                    definition: None,
                    studio: detail
                        .production_companies
                        .into_iter()
                        .next()
                        .map(|c| c.name),
                    artwork: collect_artwork(&detail.poster_path, detail.images.as_ref()),
                    kind: MediaKindMeta::Movie,
                })
            }
            MediaKind::TvShow => {
                let detail: TmdbTvDetail = self
                    .authed_get(format!("{API_BASE}/tv/{}", id.id))
                    .query(&[("append_to_response", "credits,images,content_ratings")])
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;

                let credits = detail.credits.unwrap_or_default();
                let network = detail.networks.into_iter().next().map(|n| n.name);
                let show_name = detail.name.clone();

                // Show-level defaults. When a specific season+episode is known
                // (threaded via the ProviderId), fetch that episode and let its
                // fields override the show-level ones.
                let mut title = show_name.clone();
                let mut release_date = detail.first_air_date.as_deref().and_then(parse_date);
                let mut overview = detail.overview.clone();
                let mut summary = pick_summary(&detail.tagline, &detail.overview);
                let mut artwork = collect_artwork(&detail.poster_path, detail.images.as_ref());
                let mut episode_directors: Vec<Person> = vec![];
                let mut episode_writers: Vec<Person> = vec![];
                let mut guest_cast: Vec<Person> = vec![];
                let mut episode_title: Option<String> = None;

                if let (Some(season), Some(episode)) = (id.season, id.episode) {
                    // TV episode details live on the same v3 API / credential.
                    if let Ok(ep) = self
                        .authed_get(format!(
                            "{API_BASE}/tv/{}/season/{season}/episode/{episode}",
                            id.id
                        ))
                        .query(&[("append_to_response", "images")])
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                    {
                        if let Ok(ep) = ep.json::<TmdbEpisodeDetail>().await {
                            if let Some(name) = ep.name.filter(|n| !n.trim().is_empty()) {
                                episode_title = Some(name.clone());
                                // The episode's own title becomes the item title.
                                title = name;
                            }
                            if let Some(air) = ep.air_date.as_deref().and_then(parse_date) {
                                release_date = Some(air);
                            }
                            if let Some(ov) = ep.overview.filter(|o| !o.trim().is_empty()) {
                                summary = pick_summary(&None, &Some(ov.clone()));
                                overview = Some(ov);
                            }
                            // Episode stills become preferred artwork choices,
                            // shown before the show posters.
                            let stills = collect_still_artwork(&ep.still_path, ep.images.as_ref());
                            if !stills.is_empty() {
                                let mut merged = stills;
                                for a in artwork.into_iter() {
                                    if !merged.iter().any(|m| m.url == a.url) {
                                        merged.push(a);
                                    }
                                }
                                artwork = merged;
                            }
                            // Episode-specific crew/guest stars.
                            if let Some(c) = ep.crew {
                                episode_directors = c
                                    .iter()
                                    .filter(|c| c.job.as_deref() == Some("Director"))
                                    .map(|c| Person::new(c.name.clone()))
                                    .collect();
                                episode_writers = crew_people(&c, is_writer);
                            }
                            guest_cast = ep
                                .guest_stars
                                .into_iter()
                                .take(20)
                                .map(person_from_cast)
                                .collect();
                        }
                    }
                }

                // Cast: episode guest stars first (if any), else the show's
                // series-regular cast.
                let cast: Vec<Person> = if !guest_cast.is_empty() {
                    guest_cast
                } else {
                    credits
                        .cast
                        .into_iter()
                        .take(20)
                        .map(person_from_cast)
                        .collect()
                };

                Ok(MediaMetadata {
                    title,
                    release_date,
                    summary,
                    overview,
                    genres: detail.genres.into_iter().map(|g| g.name).collect(),
                    cast,
                    directors: episode_directors,
                    producers: vec![],
                    writers: episode_writers,
                    content_rating: us_tv_rating(detail.content_ratings.as_ref()),
                    video_kind: Some(crate::model::VideoKind::TvShow),
                    definition: None,
                    studio: network.clone(),
                    artwork,
                    kind: MediaKindMeta::Episode(EpisodeInfo {
                        show_name,
                        season: id.season.unwrap_or(0),
                        episode: id.episode.unwrap_or(0),
                        episode_title,
                        network,
                        episode_id: None,
                    }),
                })
            }
        }
    }
}

fn person_from_cast(c: TmdbCast) -> Person {
    Person {
        name: c.name,
        role: c.character,
    }
}

/// Collect distinct crew members (by name, preserving first-seen order) whose
/// role matches `pred`. TMDB lists the same person once per job, so a producer
/// credited as both "Producer" and "Executive Producer" would otherwise appear
/// twice.
fn crew_people(crew: &[TmdbCrew], pred: fn(&TmdbCrew) -> bool) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    for c in crew.iter().filter(|c| pred(c)) {
        if !out.iter().any(|p| p.name == c.name) {
            out.push(Person::new(c.name.clone()));
        }
    }
    out
}

/// Collect episode still frames as artwork choices. The primary `still_path`
/// comes first, followed by any additional stills from the images block
/// (deduped by URL). Stills are landscape frames, offered before the show's
/// portrait posters so the user can pick an episode-specific image.
fn collect_still_artwork(
    still_path: &Option<String>,
    images: Option<&TmdbStillImages>,
) -> Vec<Artwork> {
    let mut out = Vec::new();
    if let Some(p) = still_path {
        let dims = images
            .and_then(|imgs| imgs.stills.iter().find(|img| &img.file_path == p))
            .map(|img| (img.width, img.height))
            .unwrap_or((None, None));
        out.push(Artwork {
            url: format!("{IMG_BASE}/original{p}"),
            thumb_url: Some(format!("{IMG_BASE}/w342{p}")),
            width: dims.0,
            height: dims.1,
        });
    }
    if let Some(imgs) = images {
        for still in &imgs.stills {
            let url = format!("{IMG_BASE}/original{}", still.file_path);
            if out.iter().any(|a| a.url == url) {
                continue;
            }
            out.push(Artwork {
                url,
                thumb_url: Some(format!("{IMG_BASE}/w342{}", still.file_path)),
                width: still.width,
                height: still.height,
            });
        }
    }
    out
}

/// Whether a crew credit is a producer of any kind. Matches the whole
/// Production department (covers Producer, Executive Producer, Co-Producer,
/// Associate Producer, Line Producer, …), falling back to job-title matching
/// when the department is absent.
fn is_producer(c: &TmdbCrew) -> bool {
    if c.department.as_deref() == Some("Production") {
        return true;
    }
    c.job
        .as_deref()
        .map(|j| j.contains("Producer"))
        .unwrap_or(false)
}

/// Whether a crew credit is a writer of any kind. Matches the whole Writing
/// department (covers Screenplay, Writer, Story, Author, Novel, Characters, …),
/// falling back to job-title matching when the department is absent.
fn is_writer(c: &TmdbCrew) -> bool {
    if c.department.as_deref() == Some("Writing") {
        return true;
    }
    matches!(
        c.job.as_deref(),
        Some("Screenplay") | Some("Writer") | Some("Story") | Some("Author") | Some("Novel")
    )
}

fn collect_artwork(poster_path: &Option<String>, images: Option<&TmdbImages>) -> Vec<Artwork> {
    let mut out = Vec::new();
    if let Some(p) = poster_path {
        // If the primary poster also appears in the images list, borrow its
        // true dimensions.
        let dims = images
            .and_then(|imgs| imgs.posters.iter().find(|img| &img.file_path == p))
            .map(|img| (img.width, img.height))
            .unwrap_or((None, None));
        out.push(Artwork {
            url: format!("{IMG_BASE}/original{p}"),
            thumb_url: Some(format!("{IMG_BASE}/w342{p}")),
            width: dims.0,
            height: dims.1,
        });
    }
    if let Some(imgs) = images {
        for poster in &imgs.posters {
            let url = format!("{IMG_BASE}/original{}", poster.file_path);
            if out.iter().any(|a| a.url == url) {
                continue;
            }
            out.push(Artwork {
                url,
                thumb_url: Some(format!("{IMG_BASE}/w342{}", poster.file_path)),
                width: poster.width,
                height: poster.height,
            });
        }
    }
    out
}

/// Choose the Summary value: prefer the movie/show tagline (a short marketing
/// line); if there's no non-empty tagline, fall back to the overview truncated
/// to 255 characters.
fn pick_summary(tagline: &Option<String>, overview: &Option<String>) -> Option<String> {
    if let Some(t) = tagline {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.chars().take(255).collect());
        }
    }
    overview
        .as_ref()
        .map(|s| s.chars().take(255).collect::<String>())
        .filter(|s| !s.trim().is_empty())
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn parse_year(s: &str) -> Option<i32> {
    s.get(0..4).and_then(|y| y.parse().ok())
}

/// Whether an episode `name` matches a lowercased title `needle`. An empty
/// needle matches everything (list all); otherwise it's a case-insensitive
/// substring test. A missing name never matches a non-empty needle.
fn episode_title_matches(name: Option<&str>, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    name.map(|n| n.to_lowercase().contains(needle_lower))
        .unwrap_or(false)
}

/// Filter a season's `(episode_number, name)` list to the episodes matching a
/// lowercased title needle, producing `TvEpisodeHit`s. An empty needle keeps
/// every episode.
fn filter_matching_episodes(episodes: Vec<(u32, String)>, needle_lower: &str) -> Vec<TvEpisodeHit> {
    episodes
        .into_iter()
        .filter(|(_, name)| episode_title_matches(Some(name.as_str()), needle_lower))
        .map(|(number, name)| TvEpisodeHit { number, name })
        .collect()
}

// ---- TMDB response shapes (only the fields we use) ----

#[derive(Deserialize)]
struct TmdbSearch<T> {
    results: Vec<T>,
}

#[derive(Deserialize)]
struct TmdbMovieHit {
    id: i64,
    title: String,
    overview: Option<String>,
    release_date: Option<String>,
    poster_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbTvHit {
    id: i64,
    name: String,
    overview: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
}

#[derive(Deserialize)]
struct TmdbMovieDetail {
    title: String,
    overview: Option<String>,
    #[serde(default)]
    tagline: Option<String>,
    release_date: Option<String>,
    poster_path: Option<String>,
    #[serde(default)]
    genres: Vec<TmdbGenre>,
    #[serde(default)]
    production_companies: Vec<TmdbCompany>,
    credits: Option<TmdbCredits>,
    images: Option<TmdbImages>,
    /// From `append_to_response=release_dates`; holds per-country certifications.
    release_dates: Option<TmdbReleaseDates>,
}

#[derive(Deserialize)]
struct TmdbTvDetail {
    name: String,
    overview: Option<String>,
    #[serde(default)]
    tagline: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
    #[serde(default)]
    genres: Vec<TmdbGenre>,
    #[serde(default)]
    networks: Vec<TmdbCompany>,
    credits: Option<TmdbCredits>,
    images: Option<TmdbImages>,
    /// From `append_to_response=content_ratings`; per-country TV ratings.
    content_ratings: Option<TmdbContentRatings>,
}

#[derive(Deserialize)]
struct TmdbGenre {
    name: String,
}

#[derive(Deserialize)]
struct TmdbCompany {
    name: String,
}

#[derive(Deserialize, Default)]
struct TmdbCredits {
    #[serde(default)]
    cast: Vec<TmdbCast>,
    #[serde(default)]
    crew: Vec<TmdbCrew>,
}

#[derive(Deserialize)]
struct TmdbCast {
    name: String,
    character: Option<String>,
}

#[derive(Deserialize)]
struct TmdbCrew {
    name: String,
    job: Option<String>,
    #[serde(default)]
    department: Option<String>,
}

#[derive(Deserialize)]
struct TmdbImages {
    #[serde(default)]
    posters: Vec<TmdbImage>,
}

#[derive(Deserialize)]
struct TmdbImage {
    file_path: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// A single TV episode's details (`/tv/{id}/season/{s}/episode/{e}`), with
/// `append_to_response=images` supplying the still frames. All fields optional;
/// TMDB omits them for sparsely-documented episodes.
#[derive(Deserialize)]
struct TmdbEpisodeDetail {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    air_date: Option<String>,
    #[serde(default)]
    still_path: Option<String>,
    #[serde(default)]
    guest_stars: Vec<TmdbCast>,
    #[serde(default)]
    crew: Option<Vec<TmdbCrew>>,
    #[serde(default)]
    images: Option<TmdbStillImages>,
}

#[derive(Deserialize)]
struct TmdbStillImages {
    #[serde(default)]
    stills: Vec<TmdbImage>,
}

/// Minimal episode payload: just the name, for enriching search results.
#[derive(Deserialize)]
struct TmdbEpisodeName {
    #[serde(default)]
    name: Option<String>,
}

/// A season's episode list (`/tv/{id}/season/{s}`), used to populate the
/// Matches pane with every episode of a season.
#[derive(Deserialize)]
struct TmdbSeasonDetail {
    #[serde(default)]
    episodes: Vec<TmdbSeasonEpisode>,
}

#[derive(Deserialize)]
struct TmdbSeasonEpisode {
    episode_number: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    air_date: Option<String>,
    #[serde(default)]
    still_path: Option<String>,
}

/// A show's seasons list (`/tv/{id}`), used to build the Matches tree.
#[derive(Deserialize)]
struct TmdbTvSeasonsDetail {
    name: String,
    #[serde(default)]
    seasons: Vec<TmdbSeasonSummaryDto>,
}

#[derive(Deserialize)]
struct TmdbSeasonSummaryDto {
    season_number: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    episode_count: u32,
}

// ---- Ratings / certifications ----

#[derive(Deserialize)]
struct TmdbReleaseDates {
    #[serde(default)]
    results: Vec<TmdbReleaseDatesCountry>,
}

#[derive(Deserialize)]
struct TmdbReleaseDatesCountry {
    iso_3166_1: String,
    #[serde(default)]
    release_dates: Vec<TmdbReleaseDateEntry>,
}

#[derive(Deserialize)]
struct TmdbReleaseDateEntry {
    #[serde(default)]
    certification: String,
}

#[derive(Deserialize)]
struct TmdbContentRatings {
    #[serde(default)]
    results: Vec<TmdbContentRatingCountry>,
}

#[derive(Deserialize)]
struct TmdbContentRatingCountry {
    iso_3166_1: String,
    #[serde(default)]
    rating: String,
}

/// Extract the US movie certification (e.g. "PG-13") from the release_dates
/// block, falling back to the first non-empty certification found.
fn us_movie_certification(rd: Option<&TmdbReleaseDates>) -> Option<String> {
    let rd = rd?;
    // Prefer the US entry.
    if let Some(us) = rd.results.iter().find(|c| c.iso_3166_1 == "US") {
        if let Some(cert) = us
            .release_dates
            .iter()
            .map(|e| e.certification.trim())
            .find(|c| !c.is_empty())
        {
            return Some(cert.to_string());
        }
    }
    // Fallback: any non-empty certification.
    rd.results
        .iter()
        .flat_map(|c| c.release_dates.iter())
        .map(|e| e.certification.trim())
        .find(|c| !c.is_empty())
        .map(|c| c.to_string())
}

/// Extract the US TV content rating (e.g. "TV-14"), falling back to the first
/// non-empty rating found.
fn us_tv_rating(cr: Option<&TmdbContentRatings>) -> Option<String> {
    let cr = cr?;
    if let Some(us) = cr
        .results
        .iter()
        .find(|c| c.iso_3166_1 == "US" && !c.rating.trim().is_empty())
    {
        return Some(us.rating.trim().to_string());
    }
    cr.results
        .iter()
        .map(|c| c.rating.trim())
        .find(|r| !r.is_empty())
        .map(|r| r.to_string())
}

use super::MetadataProvider;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_url_builds() {
        let p = Some("/abc.jpg".to_string());
        assert_eq!(
            TmdbProvider::image_url(&p, "w342"),
            Some("https://image.tmdb.org/t/p/w342/abc.jpg".to_string())
        );
        assert_eq!(TmdbProvider::image_url(&None, "w342"), None);
    }

    #[test]
    fn summary_prefers_tagline() {
        // Tagline wins when present.
        assert_eq!(
            pick_summary(
                &Some("Believe the impossible.".into()),
                &Some("Long...".into())
            )
            .as_deref(),
            Some("Believe the impossible.")
        );
        // Empty tagline falls back to overview.
        assert_eq!(
            pick_summary(&Some("   ".into()), &Some("An overview.".into())).as_deref(),
            Some("An overview.")
        );
        // No tagline falls back to overview.
        assert_eq!(
            pick_summary(&None, &Some("An overview.".into())).as_deref(),
            Some("An overview.")
        );
        // Nothing available.
        assert_eq!(pick_summary(&None, &None), None);
    }

    #[test]
    fn parses_year_and_date() {
        assert_eq!(parse_year("1999-03-31"), Some(1999));
        assert_eq!(
            parse_date("1999-03-31"),
            NaiveDate::from_ymd_opt(1999, 3, 31)
        );
    }

    #[test]
    fn extracts_us_movie_certification() {
        let rd = TmdbReleaseDates {
            results: vec![
                TmdbReleaseDatesCountry {
                    iso_3166_1: "GB".into(),
                    release_dates: vec![TmdbReleaseDateEntry {
                        certification: "15".into(),
                    }],
                },
                TmdbReleaseDatesCountry {
                    iso_3166_1: "US".into(),
                    release_dates: vec![
                        TmdbReleaseDateEntry {
                            certification: "".into(),
                        },
                        TmdbReleaseDateEntry {
                            certification: "R".into(),
                        },
                    ],
                },
            ],
        };
        assert_eq!(us_movie_certification(Some(&rd)).as_deref(), Some("R"));
        assert_eq!(us_movie_certification(None), None);
    }

    #[test]
    fn extracts_us_tv_rating() {
        let cr = TmdbContentRatings {
            results: vec![
                TmdbContentRatingCountry {
                    iso_3166_1: "DE".into(),
                    rating: "16".into(),
                },
                TmdbContentRatingCountry {
                    iso_3166_1: "US".into(),
                    rating: "TV-14".into(),
                },
            ],
        };
        assert_eq!(us_tv_rating(Some(&cr)).as_deref(), Some("TV-14"));
    }

    fn crew(name: &str, job: Option<&str>, dept: Option<&str>) -> TmdbCrew {
        TmdbCrew {
            name: name.to_string(),
            job: job.map(str::to_string),
            department: dept.map(str::to_string),
        }
    }

    #[test]
    fn producers_match_whole_production_department() {
        let crew_list = vec![
            crew("Exec Only", Some("Executive Producer"), Some("Production")),
            crew("Co Prod", Some("Co-Producer"), Some("Production")),
            crew("A Director", Some("Director"), Some("Directing")),
            // No department, but a producer job title (fallback path).
            crew("Bare Producer", Some("Producer"), None),
        ];
        let names: Vec<_> = crew_people(&crew_list, is_producer)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Exec Only", "Co Prod", "Bare Producer"]);
    }

    #[test]
    fn writers_match_whole_writing_department() {
        let crew_list = vec![
            crew("Screen Writer", Some("Screenplay"), Some("Writing")),
            crew("Story Person", Some("Story"), Some("Writing")),
            crew("A Director", Some("Director"), Some("Directing")),
            // No department, but a writer job title (fallback path).
            crew("Bare Writer", Some("Writer"), None),
        ];
        let names: Vec<_> = crew_people(&crew_list, is_writer)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Screen Writer", "Story Person", "Bare Writer"]);
    }

    #[test]
    fn crew_people_dedupes_by_name() {
        // Same producer credited under two jobs should appear once.
        let crew_list = vec![
            crew("Jane Doe", Some("Producer"), Some("Production")),
            crew("Jane Doe", Some("Executive Producer"), Some("Production")),
        ];
        let names: Vec<_> = crew_people(&crew_list, is_producer)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["Jane Doe"]);
    }

    #[test]
    fn parses_producers_writers_from_tmdb_credits_json() {
        // A realistic (trimmed) TMDB `credits` payload, matching the shape of
        // the `append_to_response=credits` block on a movie detail response.
        let json = r#"{
            "cast": [
                { "name": "Keanu Reeves", "character": "Neo" }
            ],
            "crew": [
                { "name": "Lana Wachowski", "job": "Director", "department": "Directing" },
                { "name": "Lilly Wachowski", "job": "Writer", "department": "Writing" },
                { "name": "Joel Silver", "job": "Producer", "department": "Production" },
                { "name": "Bruce Berman", "job": "Executive Producer", "department": "Production" }
            ]
        }"#;
        let credits: TmdbCredits = serde_json::from_str(json).unwrap();

        let producers: Vec<_> = crew_people(&credits.crew, is_producer)
            .into_iter()
            .map(|p| p.name)
            .collect();
        let writers: Vec<_> = crew_people(&credits.crew, is_writer)
            .into_iter()
            .map(|p| p.name)
            .collect();

        assert_eq!(producers, vec!["Joel Silver", "Bruce Berman"]);
        assert_eq!(writers, vec!["Lilly Wachowski"]);
    }

    #[test]
    fn parses_episode_detail_json() {
        // A trimmed episode payload (with append_to_response=images stills).
        let json = r#"{
            "name": "Cat's in the Bag...",
            "overview": "Walt and Jesse attempt to tie up loose ends.",
            "air_date": "2008-01-27",
            "still_path": "/still1.jpg",
            "guest_stars": [ { "name": "Guest One", "character": "Victim" } ],
            "crew": [
                { "name": "Adam Bernstein", "job": "Director", "department": "Directing" },
                { "name": "Vince Gilligan", "job": "Writer", "department": "Writing" }
            ],
            "images": { "stills": [
                { "file_path": "/still1.jpg", "width": 1920, "height": 1080 },
                { "file_path": "/still2.jpg", "width": 1280, "height": 720 }
            ] }
        }"#;
        let ep: TmdbEpisodeDetail = serde_json::from_str(json).unwrap();
        assert_eq!(ep.name.as_deref(), Some("Cat's in the Bag..."));
        assert_eq!(ep.air_date.as_deref(), Some("2008-01-27"));

        // Stills become artwork: primary still first (with real dims), then the
        // extras, deduped.
        let art = collect_still_artwork(&ep.still_path, ep.images.as_ref());
        assert_eq!(art.len(), 2);
        assert!(art[0].url.ends_with("/still1.jpg"));
        assert_eq!(art[0].width, Some(1920));
        assert!(art[1].url.ends_with("/still2.jpg"));

        // Episode crew maps to directors/writers.
        let crew = ep.crew.unwrap();
        let directors: Vec<_> = crew
            .iter()
            .filter(|c| c.job.as_deref() == Some("Director"))
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(directors, vec!["Adam Bernstein"]);
        let writers: Vec<_> = crew_people(&crew, is_writer)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(writers, vec!["Vince Gilligan"]);
    }

    #[test]
    fn episode_detail_tolerates_missing_fields() {
        // TMDB omits most fields for sparsely-documented episodes.
        let ep: TmdbEpisodeDetail = serde_json::from_str("{}").unwrap();
        assert!(ep.name.is_none());
        assert!(ep.overview.is_none());
        assert!(ep.air_date.is_none());
        assert!(ep.still_path.is_none());
        assert!(ep.guest_stars.is_empty());
        assert!(ep.crew.is_none());
        assert!(collect_still_artwork(&ep.still_path, ep.images.as_ref()).is_empty());
    }

    #[test]
    fn parses_episode_name_payload() {
        // The minimal shape used to enrich TV search results with episode names.
        let ep: TmdbEpisodeName =
            serde_json::from_str(r#"{ "name": "Pilot", "overview": "..." }"#).unwrap();
        assert_eq!(ep.name.as_deref(), Some("Pilot"));
        // Missing/blank name -> None after the caller's filter.
        let none: TmdbEpisodeName = serde_json::from_str("{}").unwrap();
        assert!(none.name.is_none());
        let blank: TmdbEpisodeName = serde_json::from_str(r#"{ "name": "  " }"#).unwrap();
        assert!(blank.name.filter(|n| !n.trim().is_empty()).is_none());
    }

    #[test]
    fn parses_season_episode_list() {
        let json = r#"{
            "episodes": [
                { "episode_number": 1, "name": "Pilot", "air_date": "2008-01-20",
                  "overview": "First.", "still_path": "/s1.jpg" },
                { "episode_number": 2, "name": "Cat's in the Bag...", "air_date": "2008-01-27",
                  "overview": "Second.", "still_path": null }
            ]
        }"#;
        let season: TmdbSeasonDetail = serde_json::from_str(json).unwrap();
        assert_eq!(season.episodes.len(), 2);
        assert_eq!(season.episodes[0].episode_number, 1);
        assert_eq!(
            season.episodes[1].name.as_deref(),
            Some("Cat's in the Bag...")
        );
        assert!(season.episodes[1].still_path.is_none());
    }

    #[test]
    fn parses_tv_seasons_detail() {
        let json = r#"{
            "name": "Breaking Bad",
            "seasons": [
                { "season_number": 0, "name": "Specials", "episode_count": 3 },
                { "season_number": 1, "name": "Season 1", "episode_count": 7 },
                { "season_number": 2, "name": null, "episode_count": 0 }
            ]
        }"#;
        let detail: TmdbTvSeasonsDetail = serde_json::from_str(json).unwrap();
        assert_eq!(detail.name, "Breaking Bad");
        assert_eq!(detail.seasons.len(), 3);
        // Season 2 has 0 episodes; the caller (tv_seasons) filters it out, but
        // the DTO still parses it. Verify the raw parse.
        assert_eq!(detail.seasons[2].episode_count, 0);
        assert!(detail.seasons[2].name.is_none());
    }

    #[test]
    fn episode_title_matches_is_case_insensitive_substring() {
        // Empty needle matches everything (list all).
        assert!(episode_title_matches(Some("Pilot"), ""));
        assert!(episode_title_matches(None, ""));
        // Case-insensitive substring.
        assert!(episode_title_matches(Some("Cat's in the Bag..."), "cat"));
        assert!(episode_title_matches(Some("Pilot"), "pil"));
        assert!(!episode_title_matches(Some("Pilot"), "zzz"));
        // Missing name never matches a non-empty needle.
        assert!(!episode_title_matches(None, "pilot"));
    }

    #[test]
    fn filter_matching_episodes_keeps_only_matches() {
        let eps = vec![
            (1, "Pilot".to_string()),
            (2, "Cat's in the Bag...".to_string()),
            (3, "...And the Bag's in the River".to_string()),
        ];
        // Non-empty needle: substring, case-insensitive.
        let hits = filter_matching_episodes(eps.clone(), "bag");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].number, 2);
        assert_eq!(hits[1].number, 3);
        // Empty needle keeps everything.
        assert_eq!(filter_matching_episodes(eps.clone(), "").len(), 3);
        // No match -> empty.
        assert!(filter_matching_episodes(eps, "zzz").is_empty());
    }
}
