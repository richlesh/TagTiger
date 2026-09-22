//! Reads the user's chosen accent color on Linux via the XDG desktop portal
//! (`org.freedesktop.portal.Settings`, key `org.freedesktop.appearance /
//! accent-color`), matching how Slint's own backend obtains it. The portal
//! only exposes an accent color (a `(ddd)` RGB tuple in [0,1]); there is no
//! separate selection-text color, so we derive a legible text color from the
//! accent's luminance.
//!
//! Returns `None` when the portal isn't present or doesn't expose the setting
//! (older desktops), in which case the UI keeps Slint's Palette defaults (which
//! themselves tint toward the accent when the backend's watcher populated it).

#![cfg(target_os = "linux")]

use crate::sys_colors::{Rgb, SystemColors};

use zbus::zvariant::{OwnedValue, Value};

/// Read the accent color from the XDG settings portal (one-shot, blocking).
fn read_accent() -> Option<Rgb> {
    // A short-lived blocking zbus connection to the session bus. `zbus` is
    // already in the dependency graph via Slint's winit backend on Linux.
    let conn = zbus::blocking::Connection::session().ok()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
    .ok()?;

    let args = ("org.freedesktop.appearance", "accent-color");

    // Prefer ReadOne (portal v2), which returns the accent value directly. Fall
    // back to Read (older portals), which wraps the value in an extra variant.
    let value: OwnedValue = proxy
        .call("ReadOne", &args)
        .or_else(|_| proxy.call("Read", &args))
        .ok()?;

    let (r, g, b) = tuple_from_value(value)?;
    // The portal reports -1 for "no preference"; treat any out-of-range as none.
    if !(0.0..=1.0).contains(&r) || !(0.0..=1.0).contains(&g) || !(0.0..=1.0).contains(&b) {
        return None;
    }
    let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Rgb {
        r: q(r),
        g: q(g),
        b: q(b),
    })
}

/// Unwrap a `(ddd)` RGB tuple from a portal reply, peeling any nested variant
/// wrappers (Read wraps one extra `v` compared to ReadOne).
fn tuple_from_value(value: OwnedValue) -> Option<(f64, f64, f64)> {
    // Try the direct tuple conversion first.
    if let Ok(t) = <(f64, f64, f64)>::try_from(value.clone()) {
        return Some(t);
    }
    // Otherwise unwrap one variant layer and retry.
    let inner: Value = value.into();
    if let Value::Value(boxed) = inner {
        if let Ok(owned) = OwnedValue::try_from(*boxed) {
            if let Ok(t) = <(f64, f64, f64)>::try_from(owned) {
                return Some(t);
            }
        }
    }
    None
}

/// Read the current OS accent color. Highlight uses the accent; the text color
/// is derived for legibility since the portal doesn't provide one.
pub fn system_colors() -> Option<SystemColors> {
    let accent = read_accent()?;
    Some(SystemColors {
        highlight: accent,
        highlight_text: accent.contrast_text(),
        accent,
    })
}
