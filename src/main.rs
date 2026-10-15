//! Hostdeck: a modern SSH and RDP connection manager for Linux.

mod import;
mod launch;
mod model;
mod secrets;
mod terminal;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    let mut store_path = model::Store::default_path();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("hostdeck {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--config" => {
                if let Some(p) = args.next() {
                    store_path = p.into();
                }
            }
            "--help" | "-h" => {
                println!("Usage: hostdeck [--config <sessions.toml>]");
                return Ok(());
            }
            other => {
                eprintln!("hostdeck: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/hostdeck.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Hostdeck")
        .with_app_id("hostdeck")
        .with_inner_size([1320.0, 820.0])
        .with_min_inner_size([860.0, 520.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Hostdeck", options, Box::new(move |cc| Ok(Box::new(ui::App::new(cc, store_path)))))
}
