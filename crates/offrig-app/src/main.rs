//! offrig desktop app.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod worker;

use std::sync::mpsc;

use eframe::egui;
use offrig_core::config::Config;

/// The 256 px window icon, embedded in the binary.
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/icon/offrig_256.png");

/// Decode the embedded window icon.
fn window_icon() -> Result<egui::IconData, String> {
    eframe::icon_data::from_png_bytes(WINDOW_ICON_PNG).map_err(|e| e.to_string())
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("offrig")
        .with_inner_size([1180.0, 820.0])
        .with_min_inner_size([860.0, 560.0]);
    // A missing icon must never stop the app from starting.
    if let Ok(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "offrig",
        options,
        Box::new(|cc| {
            let (cmd_tx, cmd_rx) = mpsc::channel();
            let (upd_tx, upd_rx) = mpsc::channel();
            let out = worker::Outbox::new(upd_tx, cc.egui_ctx.clone());
            let cfg = Config::load();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut app = app::App::new(cmd_tx, upd_rx, out.clone(), cancel.clone());
            match cfg {
                Ok(cfg) => worker::spawn(cfg, out, cmd_rx, cancel),
                Err(e) => app.st.apply(worker::Update::Error(format!(
                    "offrig config: {}",
                    offrig_core::error::chain(&e)
                ))),
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_decodes_to_256_square() {
        let icon = window_icon().expect("embedded icon decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
