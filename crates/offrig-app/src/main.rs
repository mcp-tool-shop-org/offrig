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
        Box::new(|cc| Ok(Box::new(build_app(cc.egui_ctx.clone(), Config::load())))),
    )
}

/// The window wired to a new worker thread, or, when the config will not load,
/// showing why there is no worker.
fn build_app(ctx: egui::Context, cfg: offrig_core::Result<Config>) -> app::App {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (upd_tx, upd_rx) = mpsc::channel();
    let out = worker::Outbox::new(upd_tx, ctx);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut app = app::App::new(cmd_tx, upd_rx, out.clone(), cancel.clone());
    match cfg {
        Ok(cfg) => worker::spawn(cfg, out, cmd_rx, cancel),
        Err(e) => app.st.apply(worker::Update::Error(format!(
            "offrig config: {}",
            offrig_core::error::chain(&e)
        ))),
    }
    app
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

    #[test]
    fn a_config_that_will_not_load_shows_as_an_error_and_starts_no_worker() {
        let app = build_app(
            egui::Context::default(),
            Err(offrig_core::Error::Config("bad toml".into())),
        );
        assert_eq!(
            app.st.error.as_deref(),
            Some("offrig config: config error: bad toml")
        );
        assert!(app.st.cfg.is_none());
    }

    #[test]
    fn a_loaded_config_starts_a_worker_that_reports_it() {
        let Some(rig) = crate::worker::tests::Rig::enter(
            module_path!(),
            "a_loaded_config_starts_a_worker_that_reports_it",
        ) else {
            return;
        };
        // The worker's RunPod client talks to this mock (the child's environment
        // points it there).
        let _rp = rig.runpod(|route, body, _| match route {
            "GET /pods" => (200, "[]".into()),
            "POST /graphql" if body.contains("myself") => (
                200,
                r#"{"data":{"myself":{"clientBalance":20.0,"currentSpendPerHr":0.0,"spendLimit":null}}}"#
                    .into(),
            ),
            _ => (200, r#"{"data":{"gpuTypes":[]}}"#.into()),
        });
        let mut app = build_app(egui::Context::default(), Ok(Config::default()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while app.st.cfg.is_none() || !app.st.offers.contains_key(&1) {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never reported"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
            app.drain();
        }
        assert_eq!(
            app.st.active_profile().map(|p| p.name.as_str()),
            Some("medium")
        );
    }
}
