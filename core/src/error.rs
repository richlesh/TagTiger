//! Crate-wide error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("MP4 tagging error: {0}")]
    Mp4(#[from] mp4ameta::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("provider `{provider}` error: {message}")]
    Provider { provider: String, message: String },

    #[error("no API key configured for provider `{0}`")]
    MissingApiKey(String),

    #[error("could not parse filename: {0}")]
    Naming(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
