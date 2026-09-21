//! Windows file-association registration (per-user, no admin required).
//!
//! Windows has no equivalent of macOS's `CFBundleDocumentTypes` or the Linux
//! `.desktop` `MimeType=` — associations live in the registry. Because TagTiger
//! ships as a plain `.zip` (no installer), the app registers itself under
//! `HKEY_CURRENT_USER\Software\Classes`, which needs no elevation. Registration
//! is idempotent and runs on normal startup, and can also be invoked explicitly
//! with `--register-file-types` / `--unregister-file-types`.
//!
//! We shell out to `reg.exe` (always present on Windows) to avoid adding a
//! registry-access crate dependency.

#![cfg(windows)]

use std::process::Command;

/// ProgID used for TagTiger's file class.
const PROGID: &str = "TagTiger.Movie";
/// Extensions we associate with.
const EXTS: &[&str] = &["mp4", "m4v"];

/// Path to the running executable, quoted for a registry command string.
fn exe_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_string_lossy().into_owned();
    // `"C:\path\TagTiger.exe" "%1"` — %1 is the opened file.
    Some(format!("\"{exe}\" \"%1\""))
}

/// Run a `reg.exe` command, returning whether it succeeded.
fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Register TagTiger as an opener for `.mp4` / `.m4v` for the current user.
/// Returns true if all writes succeeded.
pub fn register() -> bool {
    let Some(cmd) = exe_command() else {
        return false;
    };
    let mut ok = true;

    // Define the ProgID: friendly name + open command.
    ok &= reg(&[
        "add",
        &format!(r"HKCU\Software\Classes\{PROGID}"),
        "/ve",
        "/d",
        "MPEG-4 Movie",
        "/f",
    ]);
    ok &= reg(&[
        "add",
        &format!(r"HKCU\Software\Classes\{PROGID}\shell\open\command"),
        "/ve",
        "/d",
        &cmd,
        "/f",
    ]);

    // Point the icon at the executable's embedded icon.
    if let Ok(exe) = std::env::current_exe() {
        let icon = format!("{},0", exe.to_string_lossy());
        ok &= reg(&[
            "add",
            &format!(r"HKCU\Software\Classes\{PROGID}\DefaultIcon"),
            "/ve",
            "/d",
            &icon,
            "/f",
        ]);
    }

    // For each extension, add our ProgID under OpenWithProgids so TagTiger
    // shows up in "Open with" without hijacking the user's default handler.
    for ext in EXTS {
        ok &= reg(&[
            "add",
            &format!(r"HKCU\Software\Classes\.{ext}\OpenWithProgids"),
            "/v",
            PROGID,
            "/t",
            "REG_NONE",
            "/f",
        ]);
    }
    ok
}

/// Remove the registration written by [`register`].
pub fn unregister() -> bool {
    let mut ok = true;
    ok &= reg(&["delete", &format!(r"HKCU\Software\Classes\{PROGID}"), "/f"]);
    for ext in EXTS {
        ok &= reg(&[
            "delete",
            &format!(r"HKCU\Software\Classes\.{ext}\OpenWithProgids"),
            "/v",
            PROGID,
            "/f",
        ]);
    }
    ok
}
