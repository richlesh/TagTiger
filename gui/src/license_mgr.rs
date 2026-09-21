//! License-key validation and persisted settings.
//!
//! The license key is derived from the user's email with an HMAC, mirroring the
//! scheme used across the Glowing Cat apps:
//!
//! ```text
//! key = HMAC-SHA256(LICENSE_SALT, email.to_lowercase().trim())
//!         -> hex, first 16 chars, uppercased, grouped as XXXX-XXXX-XXXX-XXXX
//! ```
//!
//! Settings (including the saved license) persist to
//! `~/.tagtiger-settings.json`.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::path::PathBuf;

use crate::license::LICENSE_SALT;

type HmacSha256 = Hmac<Sha256>;

/// Compute the expected 16-char (dash-less, uppercase) license key for an
/// email address.
pub fn expected_key(email: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(LICENSE_SALT.as_bytes()).expect("HMAC accepts any key length");
    mac.update(email.to_lowercase().trim().as_bytes());
    let digest = mac.finalize().into_bytes();
    // Hex-encode, take the first 16 hex chars, uppercase.
    let hex = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    hex[..16].to_uppercase()
}

/// Format a raw (dash-less) key as `XXXX-XXXX-XXXX-XXXX`.
pub fn format_key(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>()
        .to_uppercase();
    clean
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("-")
}

/// Strip formatting (dashes/spaces) from a key, uppercasing.
pub fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

/// Whether `key` is a valid license for `email`.
pub fn is_valid(key: &str, email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() {
        return false;
    }
    let clean = normalize_key(key);
    clean.len() == 16 && clean == expected_key(email)
}

/// Persisted user settings, stored as JSON in the home directory.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Saved license key (dash-less, uppercase) — empty if unlicensed.
    #[serde(default)]
    pub license_key: String,
    /// Email the license key was issued to.
    #[serde(default)]
    pub license_email: String,
    /// Number of successful tag-write operations, used to show the splash
    /// every N writes.
    #[serde(default)]
    pub tag_count: u64,
}

impl Settings {
    /// Path to `~/.tagtiger-settings.json` (falls back to the current dir if
    /// the home directory can't be resolved).
    pub fn path() -> PathBuf {
        let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        p.push(".tagtiger-settings.json");
        p
    }

    /// Load settings, returning defaults if the file is missing or unreadable.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist settings to disk (best-effort; returns any IO/serialize error).
    pub fn save(&self) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::path(), json)
    }

    /// Whether the saved license is currently valid.
    pub fn is_licensed(&self) -> bool {
        is_valid(&self.license_key, &self.license_email)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_key_is_16_upper_hex() {
        let k = expected_key("user@example.com");
        assert_eq!(k.len(), 16);
        assert!(k
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase()));
    }

    #[test]
    fn validation_is_case_and_dash_insensitive_and_email_normalized() {
        let email = "User@Example.com";
        let raw = expected_key(email); // 16 upper hex
        let dashed = format_key(&raw);
        // Exact.
        assert!(is_valid(&raw, email));
        // Dashed form accepted.
        assert!(is_valid(&dashed, email));
        // Lowercased key accepted.
        assert!(is_valid(&raw.to_lowercase(), email));
        // Email case/whitespace normalized.
        assert!(is_valid(&raw, "  user@example.com  "));
    }

    #[test]
    fn rejects_bad_key_or_empty_email() {
        let email = "user@example.com";
        assert!(!is_valid("0000-0000-0000-0000", email));
        assert!(!is_valid(&expected_key(email), ""));
        assert!(!is_valid("", email));
        // Wrong email -> different key.
        assert!(!is_valid(&expected_key("a@b.com"), "c@d.com"));
    }

    #[test]
    fn format_key_groups_in_fours() {
        assert_eq!(format_key("ABCDEF0123456789"), "ABCD-EF01-2345-6789");
        assert_eq!(format_key("abcd1234"), "ABCD-1234");
    }
}
