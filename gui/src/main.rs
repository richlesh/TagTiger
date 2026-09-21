//! egui desktop entry point for TagTiger.

mod app;
mod license;
mod license_mgr;
mod winassoc;
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Windows file-association registration (per-user, no admin). Explicit
    // flags let a user (or a future installer) register/unregister; otherwise
    // we self-register idempotently on every startup so "Open with" works after
    // first launch, since TagTiger ships without an installer.
    #[cfg(windows)]
    {
        match std::env::args().nth(1).as_deref() {
            Some("--register-file-types") => {
                let ok = winassoc::register();
                std::process::exit(if ok { 0 } else { 1 });
            }
            Some("--unregister-file-types") => {
                let ok = winassoc::unregister();
                std::process::exit(if ok { 0 } else { 1 });
            }
            _ => {
                let _ = winassoc::register();
            }
        }
    }

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
