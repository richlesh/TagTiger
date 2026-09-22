//! Reads the user's chosen Windows accent / highlight colors so the app's
//! highlights and the save-progress bar match the OS instead of Slint's
//! hardcoded Fluent blue.
//!
//! - Accent (progress bar): DWM colorization color (`DwmGetColorizationColor`),
//!   which is the user's chosen accent; falls back to `COLOR_HIGHLIGHT`.
//! - Selection highlight + text: `COLOR_HIGHLIGHT` / `COLOR_HIGHLIGHTTEXT`, the
//!   classic system selection colors.

#![cfg(target_os = "windows")]

use crate::sys_colors::{Rgb, SystemColors};

use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT};

/// Decode a Win32 `COLORREF` (0x00BBGGRR) into RGB.
fn sys_color(index: windows::Win32::Graphics::Gdi::SYS_COLOR_INDEX) -> Rgb {
    let c = unsafe { GetSysColor(index) };
    Rgb {
        r: (c & 0xFF) as u8,
        g: ((c >> 8) & 0xFF) as u8,
        b: ((c >> 16) & 0xFF) as u8,
    }
}

/// Read the current OS accent / selection colors.
pub fn system_colors() -> Option<SystemColors> {
    let highlight = sys_color(COLOR_HIGHLIGHT);
    let highlight_text = sys_color(COLOR_HIGHLIGHTTEXT);

    // DWM colorization is 0xAARRGGBB; use it as the accent when available.
    let accent = {
        let mut argb = 0u32;
        let mut _opaque = windows::core::BOOL::default();
        if unsafe { DwmGetColorizationColor(&mut argb, &mut _opaque) }.is_ok() {
            Rgb {
                r: ((argb >> 16) & 0xFF) as u8,
                g: ((argb >> 8) & 0xFF) as u8,
                b: (argb & 0xFF) as u8,
            }
        } else {
            highlight
        }
    };

    Some(SystemColors {
        highlight,
        highlight_text,
        accent,
    })
}
