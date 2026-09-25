//! Reads the user's chosen macOS accent / selection colors so the app's
//! highlights (selected match row, poster selection borders, search-button
//! hover) and the save-progress bar match the OS, instead of Slint's hardcoded
//! Fluent blue.
//!
//! Slint's Fluent palette *tries* to tint toward the OS accent via its internal
//! `accent-color`, but in practice that isn't reliably populated (the app shows
//! blue even when the OS accent is, e.g., purple). We read the colors directly
//! with AppKit and push them into the `Theme` global.

#![cfg(target_os = "macos")]

use objc2_app_kit::{NSColor, NSColorSpace};

use crate::sys_colors::{Rgb, SystemColors};

/// Convert an `NSColor` to sRGB 8-bit components.
fn to_rgb(color: &NSColor) -> Option<Rgb> {
    unsafe {
        let srgb = NSColorSpace::sRGBColorSpace();
        // colorUsingColorSpace: returns an RGB-convertible color (or nil for
        // e.g. pattern colors, which none of ours are).
        let c = color.colorUsingColorSpace(&srgb)?;
        let r = c.redComponent();
        let g = c.greenComponent();
        let b = c.blueComponent();
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some(Rgb {
            r: q(r),
            g: q(g),
            b: q(b),
        })
    }
}

/// Read the current OS accent / selection colors. Best-effort; returns `None`
/// if any color can't be resolved.
pub fn system_colors() -> Option<SystemColors> {
    unsafe {
        let sel_bg = NSColor::selectedContentBackgroundColor();
        let sel_fg = NSColor::alternateSelectedControlTextColor();
        let accent = NSColor::controlAccentColor();
        Some(SystemColors {
            highlight: to_rgb(&sel_bg)?,
            highlight_text: to_rgb(&sel_fg)?,
            accent: to_rgb(&accent)?,
        })
    }
}

/// The macOS system font size in points (`NSFont.systemFontSize`), used for the
/// "System" Font Size setting. Returns `None` if it can't be read.
pub fn system_font_size() -> Option<f32> {
    use objc2_app_kit::NSFont;
    unsafe {
        let size = NSFont::systemFontSize();
        if size.is_finite() && size > 0.0 {
            Some(size as f32)
        } else {
            None
        }
    }
}
