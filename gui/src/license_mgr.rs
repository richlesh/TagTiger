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
    /// TMDB v4 read access token (Bearer). When set, it takes precedence over
    /// the `TMDB_BEARER_TOKEN` / `TMDB_API_KEY` environment variables.
    #[serde(default)]
    pub tmdb_bearer_token: String,
    /// UI theme: "Light" or "Dark". Defaults to Dark (see `default_theme`) so
    /// existing settings files without this field keep the original look.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI font size: "System", "Small", "Medium", or "Large". Defaults to
    /// "System" so existing settings files use the OS-defined size.
    #[serde(default = "default_font_size")]
    pub font_size: String,
}

/// Default theme when none is stored: Dark (the app's original appearance).
fn default_theme() -> String {
    "Dark".to_string()
}

/// Default font size when none is stored: follow the OS.
fn default_font_size() -> String {
    "System".to_string()
}

/// Fixed font-size tiers (in logical pixels / points at standard DPI).
pub const SMALL_FONT_PX: f32 = 12.0;
pub const MEDIUM_FONT_PX: f32 = 15.0;
pub const LARGE_FONT_PX: f32 = 18.0;

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

    /// Whether the UI theme is Dark. Treats any value other than "Light" as
    /// Dark, so a missing/empty field (older settings, `Default`) is Dark.
    pub fn theme_is_dark(&self) -> bool {
        !self.theme.eq_ignore_ascii_case("Light")
    }

    /// Resolve the configured font size to pixels. "System" uses the provided
    /// OS size; the fixed tiers are Small = 12, Medium = 15, Large = 18.
    pub fn font_size_px(&self, system_px: f32) -> f32 {
        match self.font_size.to_ascii_lowercase().as_str() {
            "small" => SMALL_FONT_PX,
            "medium" => MEDIUM_FONT_PX,
            "large" => LARGE_FONT_PX,
            // "system" and anything unrecognized fall back to the OS size.
            _ => system_px,
        }
    }

    /// The Font Size combo index: 0 = System, 1 = Small, 2 = Medium, 3 = Large.
    /// Unrecognized values map to System.
    pub fn font_size_index(&self) -> i32 {
        match self.font_size.to_ascii_lowercase().as_str() {
            "small" => 1,
            "medium" => 2,
            "large" => 3,
            _ => 0,
        }
    }

    /// Map a Font Size combo index back to its stored string.
    pub fn font_size_from_index(index: i32) -> String {
        match index {
            1 => "Small",
            2 => "Medium",
            3 => "Large",
            _ => "System",
        }
        .to_string()
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

    #[test]
    fn theme_defaults_to_dark_and_is_backward_compatible() {
        // A settings file predating the theme field deserializes to Dark.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.theme, "Dark");
        assert!(s.theme_is_dark());

        // Explicit values are honored (case-insensitive).
        let light: Settings = serde_json::from_str(r#"{"theme":"Light"}"#).unwrap();
        assert!(!light.theme_is_dark());
        let dark: Settings = serde_json::from_str(r#"{"theme":"Dark"}"#).unwrap();
        assert!(dark.theme_is_dark());

        // Default::default() has an empty theme, which counts as Dark.
        assert!(Settings::default().theme_is_dark());
    }

    #[test]
    fn font_size_defaults_to_system_and_maps_to_pixels() {
        // A settings file predating the font_size field defaults to "System".
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.font_size, "System");
        assert_eq!(s.font_size_index(), 0);
        // System uses the provided OS size verbatim.
        assert_eq!(s.font_size_px(17.0), 17.0);

        let small: Settings = serde_json::from_str(r#"{"font_size":"Small"}"#).unwrap();
        assert_eq!(small.font_size_index(), 1);
        assert_eq!(small.font_size_px(17.0), SMALL_FONT_PX);
        assert_eq!(small.font_size_px(17.0), 12.0);

        let medium: Settings = serde_json::from_str(r#"{"font_size":"Medium"}"#).unwrap();
        assert_eq!(medium.font_size_index(), 2);
        assert_eq!(medium.font_size_px(17.0), MEDIUM_FONT_PX);
        assert_eq!(medium.font_size_px(17.0), 15.0);

        let large: Settings = serde_json::from_str(r#"{"font_size":"Large"}"#).unwrap();
        assert_eq!(large.font_size_index(), 3);
        assert_eq!(large.font_size_px(17.0), LARGE_FONT_PX);
        assert_eq!(large.font_size_px(17.0), 18.0);

        // Index round-trips through the string mapping.
        for i in 0..=3 {
            let name = Settings::font_size_from_index(i);
            let s = Settings {
                font_size: name,
                ..Default::default()
            };
            assert_eq!(s.font_size_index(), i);
        }
    }
}
