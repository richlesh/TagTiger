//! TMDB (The Movie Database) provider.
//!
//! Uses the v3 REST data endpoints. Authentication uses a TMDB **v4 read
//! access token** (Bearer) when available, falling back to a v3 API key.
//! Configure via `TMDB_BEARER_TOKEN` (preferred) or `TMDB_API_KEY`. Note
//! TMDB's attribution requirements and rate limits when shipping.

use crate::error::{Error, Result};
use crate::model::{
    Artwork, EpisodeInfo, MediaKind, MediaKindMeta, MediaMetadata, MediaQuery, Person, ProviderId,
    SearchResult,
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
                        },
                        title: m.title,
                        year: m.release_date.as_deref().and_then(parse_year),
                        overview: m.overview,
                        poster_thumb_url: Self::image_url(&m.poster_path, "w342"),
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
                        },
                        title: t.name,
                        year: t.first_air_date.as_deref().and_then(parse_year),
                        overview: t.overview,
                        poster_thumb_url: Self::image_url(&t.poster_path, "w342"),
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
                Ok(MediaMetadata {
                    title: detail.name.clone(),
                    release_date: detail.first_air_date.as_deref().and_then(parse_date),
                    summary: pick_summary(&detail.tagline, &detail.overview),
                    overview: detail.overview,
                    genres: detail.genres.into_iter().map(|g| g.name).collect(),
                    cast: credits
                        .cast
                        .into_iter()
                        .take(20)
                        .map(person_from_cast)
                        .collect(),
                    directors: vec![],
                    producers: vec![],
                    writers: vec![],
                    content_rating: us_tv_rating(detail.content_ratings.as_ref()),
                    video_kind: Some(crate::model::VideoKind::TvShow),
                    definition: None,
                    studio: network.clone(),
                    artwork: collect_artwork(&detail.poster_path, detail.images.as_ref()),
                    // Season/episode are filled in by the caller from the parsed
                    // filename; details here describe the show.
                    kind: MediaKindMeta::Episode(EpisodeInfo {
                        show_name: detail.name,
                        season: 0,
                        episode: 0,
                        episode_title: None,
                        network,
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
}
