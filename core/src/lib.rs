//! tagtiger-core: cross-platform MP4/M4V metadata tagging.
//!
//! Architecture:
//! - [`model`]    — provider-agnostic domain types.
//! - [`providers`]— metadata sources (TMDB, ...) behind a trait.
//! - [`naming`]   — Plex-style filename parsing into queries.
//! - [`artwork`]  — poster download / decode / normalize.
//! - [`tag`]      — maps metadata onto MP4 atoms and writes files.
//!
//! Nothing in `providers` knows about atoms; nothing in `tag` knows about
//! TMDB. The GUI and CLI are thin frontends over this library.

pub mod artwork;
pub mod config;
pub mod error;
pub mod model;
pub mod mp4dim;
pub mod mp4rewrite;
pub mod naming;
pub mod providers;
pub mod tag;

pub use error::{Error, Result};
pub use model::{
    Artwork, Definition, EpisodeInfo, MediaKind, MediaKindMeta, MediaMetadata, MediaQuery, Person,
    ProviderId, SearchResult, VideoKind,
};
pub use providers::{tmdb::TmdbProvider, MetadataProvider};

/// High-level convenience: parse a filename, search a provider, and return the
/// candidate results. The caller (GUI/CLI) picks one, fetches details, lets the
/// user choose artwork, then calls [`tag::write_to_file`].
pub async fn search_for_file(
    provider: &dyn MetadataProvider,
    path: impl AsRef<std::path::Path>,
) -> Result<(MediaQuery, Vec<SearchResult>)> {
    let query = naming::parse(path)?;
    let results = provider.search(&query).await?;
    Ok((query, results))
}
