//! BestTerm.
//!
//! A native remote-access workspace for Linux and Windows.

// A release build must not open a console window behind the GUI on Windows. Debug builds keep it,
// because that is where the log goes during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use bestterm_app_ui::{BestTermApp, DEFAULT_WINDOW_SIZE};

/// Smallest window that still shows a usable grid alongside the chrome.
const MIN_WINDOW_SIZE: [f32; 2] = [720.0, 460.0];

fn main() -> eframe::Result {
    init_tracing();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting BestTerm");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BestTerm")
            .with_inner_size(DEFAULT_WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "BestTerm",
        options,
        Box::new(|_cc| Ok(Box::new(BestTermApp::new()))),
    )
}

/// Set up logging.
///
/// Defaults to `info` for BestTerm's own crates and `warn` for everything else, so the graphics
/// stack does not drown the log. Override with `RUST_LOG`.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,bestterm=info,bestterm_app_ui=info"));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_level(true)
        .init();
}
