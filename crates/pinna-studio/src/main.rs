//! Pinna Studio: the arrangement you know, plus a column of proposals you listen to
//! and decide on. Every Auftrag becomes a proposal (Git branch) with measured
//! evidence; a ghost on the track shows where it acts; A/B is loudness-matched;
//! accepting merges it.

mod agent;
mod app;
mod audio;
mod diff;
#[cfg(feature = "screenshot")]
mod screenshot;
mod state;
mod style;
mod text;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    if arg
        .as_ref()
        .is_some_and(|a| a.as_os_str() == "--help" || a.as_os_str() == "-h")
    {
        println!("usage: pinna-studio [PROJECT_DIR]");
        return Ok(());
    }
    if arg.as_ref().is_some_and(|a| a.as_os_str() == "--version") {
        println!("pinna-studio {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    #[cfg(feature = "screenshot")]
    let (size, persist_window) = match screenshot::size() {
        Some(s) => (s, false),
        None => ([1280.0, 800.0], true),
    };
    #[cfg(not(feature = "screenshot"))]
    let (size, persist_window) = ([1280.0, 800.0], true);
    let options = eframe::NativeOptions {
        persist_window,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Pinna")
            .with_inner_size(size)
            .with_min_inner_size([900.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Pinna",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, arg)))),
    )
}
