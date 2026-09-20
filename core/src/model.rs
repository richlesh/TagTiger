//! Provider-agnostic domain model.
//!
//! Nothing in this module knows about TMDB or MP4 atoms. Providers produce a
//! [`MediaMetadata`]; the [`crate::tag`] module maps it onto MP4 atoms. This
//! decoupling is what lets us add new providers and new tag targets
//! independently.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// The kind of media item, which drives the MP4 `stik` (media type) atom and
/// which set of atoms are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Movie,
    TvShow,
}

/// The Apple `stik` "media kind" for video files, chosen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoKind {
    /// `stik` = 0 (Home Video / generic movie).
    HomeVideo,
    /// `stik` = 9.
    Movie,
    /// `stik` = 10.
    TvShow,
    /// `stik` = 6.
    MusicVideo,
    /// `stik` = 11.
    Booklet,
}

impl VideoKind {
    /// The `stik` integer value for this kind.
    pub fn stik(self) -> u8 {
        match self {
            VideoKind::HomeVideo => 0,
            VideoKind::MusicVideo => 6,
            VideoKind::Movie => 9,
            VideoKind::TvShow => 10,
            VideoKind::Booklet => 11,
        }
    }

    /// Human-readable label for the menu.
    pub fn label(self) -> &'static str {
        match self {
            VideoKind::HomeVideo => "Home Video",
            VideoKind::Movie => "Movie",
            VideoKind::TvShow => "TV Show",
            VideoKind::MusicVideo => "Music Video",
            VideoKind::Booklet => "Booklet",
        }
    }

    /// All selectable kinds, in menu order.
    pub fn all() -> &'static [VideoKind] {
        &[
            VideoKind::Movie,
            VideoKind::TvShow,
            VideoKind::MusicVideo,
            VideoKind::HomeVideo,
            VideoKind::Booklet,
        ]
    }

    /// Map a `stik` integer back to a kind.
    pub fn from_stik(v: u8) -> Option<VideoKind> {
        match v {
            0 => Some(VideoKind::HomeVideo),
            6 => Some(VideoKind::MusicVideo),
            9 => Some(VideoKind::Movie),
            10 => Some(VideoKind::TvShow),
            11 => Some(VideoKind::Booklet),
            _ => None,
        }
    }
}

/// Video definition, stored in the Apple `hdvd` atom and deducible from the
/// video track's dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Definition {
    /// Standard definition. `hdvd` = 0.
    Sd,
    /// 720p HD. `hdvd` = 1.
    Hd720,
    /// 1080p HD. `hdvd` = 2.
    Hd1080,
    /// 4K UHD. `hdvd` = 3.
    Uhd4k,
}

impl Definition {
    /// The `hdvd` integer value for this definition.
    pub fn hdvd(self) -> u8 {
        match self {
            Definition::Sd => 0,
            Definition::Hd720 => 1,
            Definition::Hd1080 => 2,
            Definition::Uhd4k => 3,
        }
    }

    /// Human-readable label for the menu.
    pub fn label(self) -> &'static str {
        match self {
            Definition::Sd => "SD",
            Definition::Hd720 => "HD 720p",
            Definition::Hd1080 => "HD 1080p",
            Definition::Uhd4k => "4K",
        }
    }

    /// All definitions, in menu order.
    pub fn all() -> &'static [Definition] {
        &[
            Definition::Sd,
            Definition::Hd720,
            Definition::Hd1080,
            Definition::Uhd4k,
        ]
    }

    /// Map an `hdvd` integer back to a definition.
    pub fn from_hdvd(v: u8) -> Option<Definition> {
        match v {
            0 => Some(Definition::Sd),
            1 => Some(Definition::Hd720),
            2 => Some(Definition::Hd1080),
            3 => Some(Definition::Uhd4k),
            _ => None,
        }
    }

    /// Deduce the definition from a video track's pixel dimensions. Uses the
    /// larger of width/height against common thresholds so both landscape and
    /// portrait content classify sensibly.
    pub fn from_dimensions(width: u32, height: u32) -> Definition {
        // Classify by the number of horizontal lines (height) primarily, but
        // fall back to width for very wide content.
        let w = width.max(height);
        let h = width.min(height);
        if w >= 3200 || h >= 1600 {
            Definition::Uhd4k
        } else if w >= 1900 || h >= 1000 {
            Definition::Hd1080
        } else if w >= 1200 || h >= 700 {
            Definition::Hd720
        } else {
            Definition::Sd
        }
    }
}

/// A person credited on a title (actor, director, producer, writer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub name: String,
    /// Character name for cast, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Person {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            role: None,
        }
    }
}

/// A candidate artwork image (usually a poster). The GUI shows these in a grid
/// for the user to pick from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artwork {
    /// Remote URL to fetch the full-resolution image.
    pub url: String,
    /// Optional smaller URL for thumbnails, if the provider distinguishes them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb_url: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Normalized metadata for a single title. Fields are optional because
/// providers vary in coverage; the tag layer writes only what is present.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<NaiveDate>,
    /// Short summary/description, mapped to the `desc` atom (255-char limit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Long description, mapped to the `ldes` atom.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cast: Vec<Person>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directors: Vec<Person>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub producers: Vec<Person>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writers: Vec<Person>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio: Option<String>,
    /// User-chosen media kind (`stik`); when set, overrides the kind derived
    /// from `kind`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_kind: Option<VideoKind>,
    /// Video definition (`hdvd`); auto-deduced from dimensions on file load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<Definition>,
    /// Candidate artworks; the chosen one (if any) is written as `covr`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artwork: Vec<Artwork>,
    pub kind: MediaKindMeta,
}

/// Kind plus kind-specific fields. TV support is modeled now so the tag layer
/// and providers can be extended without reshaping the model later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum MediaKindMeta {
    #[default]
    Movie,
    Episode(EpisodeInfo),
}

impl MediaKindMeta {
    pub fn kind(&self) -> MediaKind {
        match self {
            MediaKindMeta::Movie => MediaKind::Movie,
            MediaKindMeta::Episode(_) => MediaKind::TvShow,
        }
    }
}

/// TV-episode-specific fields, mapped to `tvsh`, `tvsn`, `tves`, `tven`, `tvnn`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeInfo {
    pub show_name: String,
    pub season: u32,
    pub episode: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

/// A provider-specific identifier for a title (e.g. TMDB numeric id + kind).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderId {
    pub provider: String,
    pub id: String,
    pub kind: MediaKind,
}

/// A lightweight search hit shown to the user before fetching full details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: ProviderId,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub poster_thumb_url: Option<String>,
}

/// A parsed query derived from a filename or user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaQuery {
    pub title: String,
    pub year: Option<i32>,
    pub kind: MediaKind,
    /// Present for episode lookups.
    pub season: Option<u32>,
    pub episode: Option<u32>,
}
