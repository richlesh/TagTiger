//! Filename parsing for Plex-style naming conventions.
//!
//! Movies:   `Movie Name (2020).mp4`
//! Episodes: `Show Name - s01e02 - Episode Title.mkv`
//!           `Show Name S01E02.mp4`
//!           `Show Name/Season 01/Show Name - S01E02 - Title.m4v`
//!
//! The parser produces a [`MediaQuery`] used to drive provider searches. It is
//! intentionally isolated from providers and the tag layer.

use crate::model::{MediaKind, MediaQuery};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static EPISODE_RE: Lazy<Regex> = Lazy::new(|| {
    // Matches SxxEyy (case-insensitive), capturing show prefix, season, episode.
    Regex::new(r"(?i)^(?P<show>.*?)[ ._-]*s(?P<season>\d{1,2})[ ._-]*e(?P<episode>\d{1,3})")
        .expect("valid episode regex")
});

static YEAR_RE: Lazy<Regex> = Lazy::new(|| {
    // A 4-digit year in parentheses or standalone, 1900-2099.
    Regex::new(r"[\(\[ .]((?:19|20)\d{2})[\)\] .]?").expect("valid year regex")
});

static TRAILING_TITLE_RE: Lazy<Regex> = Lazy::new(|| {
    // After the SxxEyy token, an optional " - Episode Title".
    Regex::new(r"(?i)s\d{1,2}[ ._-]*e\d{1,3}[ ._-]+(?P<title>.+)$").expect("valid title regex")
});

/// Normalize separators (dots, underscores) to spaces and collapse whitespace.
fn normalize(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a media filename (or full path) into a [`MediaQuery`].
pub fn parse(path: impl AsRef<Path>) -> Result<MediaQuery, crate::error::Error> {
    let path = path.as_ref();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| crate::error::Error::Naming(format!("{}", path.display())))?;

    if let Some(caps) = EPISODE_RE.captures(stem) {
        let show = normalize(caps.name("show").map(|m| m.as_str()).unwrap_or(""));
        let season: u32 = caps["season"].parse().unwrap_or(0);
        let episode: u32 = caps["episode"].parse().unwrap_or(0);
        let title = if show.is_empty() {
            // Fall back to parent directory name (e.g. show folder).
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(normalize)
                .unwrap_or_default()
        } else {
            show
        };
        return Ok(MediaQuery {
            title,
            year: None,
            kind: MediaKind::TvShow,
            season: Some(season),
            episode: Some(episode),
        });
    }

    // Movie: strip a trailing year, use it as the query year.
    let year = YEAR_RE
        .captures(stem)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<i32>().ok());

    let title_part = if let Some(m) = YEAR_RE.find(stem) {
        &stem[..m.start()]
    } else {
        stem
    };
    let title = normalize(title_part);
    if title.is_empty() {
        return Err(crate::error::Error::Naming(format!(
            "empty title from `{stem}`"
        )));
    }

    Ok(MediaQuery {
        title,
        year,
        kind: MediaKind::Movie,
        season: None,
        episode: None,
    })
}

/// Extract just the episode title (if present) from a filename stem.
pub fn episode_title(stem: &str) -> Option<String> {
    TRAILING_TITLE_RE
        .captures(stem)
        .and_then(|c| c.name("title"))
        .map(|m| normalize(m.as_str()))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_movie_with_year() {
        let q = parse("The Matrix (1999).mp4").unwrap();
        assert_eq!(q.title, "The Matrix");
        assert_eq!(q.year, Some(1999));
        assert_eq!(q.kind, MediaKind::Movie);
    }

    #[test]
    fn parses_movie_with_dots() {
        let q = parse("The.Matrix.1999.1080p.mp4").unwrap();
        assert_eq!(q.title, "The Matrix");
        assert_eq!(q.year, Some(1999));
    }

    #[test]
    fn parses_episode() {
        let q = parse("Breaking Bad - S01E02 - Cat's in the Bag.mkv").unwrap();
        assert_eq!(q.title, "Breaking Bad");
        assert_eq!(q.kind, MediaKind::TvShow);
        assert_eq!(q.season, Some(1));
        assert_eq!(q.episode, Some(2));
    }

    #[test]
    fn parses_episode_compact() {
        let q = parse("The.Office.S03E10.720p.mp4").unwrap();
        assert_eq!(q.title, "The Office");
        assert_eq!(q.season, Some(3));
        assert_eq!(q.episode, Some(10));
    }

    #[test]
    fn extracts_episode_title() {
        assert_eq!(
            episode_title("Breaking Bad - S01E02 - Cat's in the Bag"),
            Some("Cat's in the Bag".to_string())
        );
    }
}
