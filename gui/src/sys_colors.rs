//! Cross-platform access to the user's OS accent / selection colors, so the
//! app's highlights (selected match row, poster selection borders, search
//! button hover) and the save-progress bar match the user's chosen OS colors
//! rather than Slint's hardcoded Fluent blue.
//!
//! Each platform has its own reader (see `macos_theme`, `win_theme`,
//! `linux_theme`); `system_colors()` dispatches to the right one. Returns
//! `None` when the OS colors can't be determined, in which case the UI falls
//! back to Slint's Palette defaults.

/// An sRGB color as 8-bit components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// The colors we theme with: the selection background/foreground (selected
/// match row, text/poster selection) and the control accent (progress bar).
pub struct SystemColors {
    pub highlight: Rgb,
    pub highlight_text: Rgb,
    pub accent: Rgb,
}

impl Rgb {
    /// Pick black or white for legible text on this color, from perceptual
    /// luminance. Used where the OS provides an accent but no matching text
    /// color (e.g. the Linux XDG accent-color setting).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn contrast_text(self) -> Rgb {
        // Rec. 601 luma.
        let l = 0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32;
        if l > 140.0 {
            Rgb { r: 0, g: 0, b: 0 }
        } else {
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            }
        }
    }
}

/// Read the current OS accent / selection colors, or `None` if unavailable.
pub fn system_colors() -> Option<SystemColors> {
    #[cfg(target_os = "macos")]
    {
        crate::macos_theme::system_colors()
    }
    #[cfg(target_os = "windows")]
    {
        crate::win_theme::system_colors()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux_theme::system_colors()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

/// The OS-defined default UI font size in points/pixels, used for the "System"
/// Font Size setting. Falls back to a sensible desktop default (13.0) on
/// platforms where it isn't read.
pub fn system_font_size() -> f32 {
    #[cfg(target_os = "macos")]
    {
        crate::macos_theme::system_font_size().unwrap_or(13.0)
    }
    #[cfg(target_os = "windows")]
    {
        crate::win_theme::system_font_size().unwrap_or(13.0)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux_theme::system_font_size().unwrap_or(13.0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        13.0
    }
}
