//! Slint desktop entry point for TagTiger.
// On Windows, mark this as a GUI (windows) subsystem binary so launching it
// does not spawn a console window. Only in release builds, so `cargo run` /
// debug builds still show logs in a terminal.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod license;
mod license_mgr;
#[cfg(target_os = "macos")]
mod macos_open;
mod worker;

fn main() -> Result<(), slint::PlatformError> {
    // Install the rustls `ring` crypto provider process-wide (reqwest is built
    // with `rustls-no-provider`, so no provider is auto-installed). Must run
    // before any TLS client is created by the worker. Ignore an error, which
    // only means one is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // macOS: register the Open-Documents Apple Event handler so "Open With" /
    // dock drops onto an already-running app deliver the file. (Cold-launch
    // Open With is a known limitation; see macos_open.)
    #[cfg(target_os = "macos")]
    macos_open::install();

    app::run()
}
