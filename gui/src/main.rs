//! egui desktop entry point for TagTiger.

mod app;
mod worker;

use app::App;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([980.0, 720.0])
            .with_title("TagTiger"),
        ..Default::default()
    };

    eframe::run_native(
        "TagTiger",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
