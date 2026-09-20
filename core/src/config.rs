//! Application configuration and platform paths.

use std::path::PathBuf;

/// Returns the platform cache directory for TagTiger (used for API/image caching).
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("TagTiger"))
}

/// Returns the platform config directory for TagTiger (used for settings such
/// as the TMDB API key).
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("TagTiger"))
}
