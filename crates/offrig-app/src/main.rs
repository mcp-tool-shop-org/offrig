//! offrig desktop app.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod worker;

use std::sync::mpsc;

use eframe::egui;
use offrig_core::config::Config;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("offrig")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([860.0, 560.0]),
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
            let mut app = app::App::new(cmd_tx, upd_rx, out.clone());
            match cfg {
                Ok(cfg) => worker::spawn(cfg, out, cmd_rx),
                Err(e) => app.st.apply(worker::Update::Error(format!(
                    "offrig config: {}",
                    offrig_core::error::chain(&e)
                ))),
            }
            Ok(Box::new(app))
        }),
    )
}
