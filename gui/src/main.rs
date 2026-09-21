//! egui desktop entry point for TagTiger.
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

use app::App;

/// Decode the embedded 256×256 PNG into an egui window icon. Returns `None` if
/// the bytes can't be decoded (in which case the app runs without an icon).
fn load_window_icon() -> Option<eframe::egui::IconData> {
    let bytes = include_bytes!("resources/app_icon_256.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
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

    // macOS: register the Open-Documents Apple Event handler as early as
    // possible — before the winit loop starts — so a cold-launch "Open With"
    // (whose odoc event fires during applicationDidFinishLaunching) is caught.
    // Re-registered in App::new too (idempotent) for the warm case.
    #[cfg(target_os = "macos")]
    macos_open::install();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([980.0, 720.0])
        .with_title("TagTiger");
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "TagTiger",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
