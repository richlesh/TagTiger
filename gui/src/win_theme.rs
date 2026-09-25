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

/// The Windows UI font size in logical pixels, read from the non-client metrics
/// message font (`SPI_GETNONCLIENTMETRICS` → `lfMessageFont.lfHeight`). A
/// negative `lfHeight` is the character height in logical units; we return its
/// magnitude. Returns `None` if the query fails.
pub fn system_font_size() -> Option<f32> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };

    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_err() {
        return None;
    }
    let h = ncm.lfMessageFont.lfHeight;
    let px = if h < 0 { (-h) as f32 } else { h as f32 };
    if px > 0.0 {
        Some(px)
    } else {
        None
    }
}
