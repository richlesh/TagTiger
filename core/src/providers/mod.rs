//! Metadata providers.
//!
//! A [`MetadataProvider`] turns a [`MediaQuery`] into search results and full
//! [`MediaMetadata`]. Providers know nothing about MP4 atoms. Add new sources
//! (OMDb, TVDB, ...) by implementing this trait.

pub mod tmdb;

use crate::error::Result;
use crate::model::{MediaMetadata, MediaQuery, ProviderId, SearchResult};
use async_trait::async_trait;

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// Stable provider name, e.g. `"tmdb"`.
    fn name(&self) -> &str;

    /// Search for titles matching a query.
    async fn search(&self, query: &MediaQuery) -> Result<Vec<SearchResult>>;

    /// Fetch full normalized metadata for a specific title.
    async fn fetch_details(&self, id: &ProviderId) -> Result<MediaMetadata>;
}
