//! egui desktop entry point for TagTiger.

mod app;
mod license;
mod license_mgr;
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
