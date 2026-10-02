//! The window. `State::apply` folds worker updates into plain data (unit tested);
//! `App::draw` renders it and turns clicks into worker commands (kittest tested).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use podbay_core::config::{Config, Profile};
use podbay_core::cost::{self, Idle};
use podbay_core::guard::Check;
use podbay_core::ollama::{ChatCheck, Tag};
use podbay_core::remote::{GpuStat, PullState};
use podbay_core::runpod::{Account, GpuOffer, Pod};
use podbay_core::spec;

use crate::worker::{self, Cmd, Outbox, Update, ZedStatus};

const LOG_CAP: usize = 400;
const OK: Color32 = Color32::from_rgb(80, 200, 120);
const BAD: Color32 = Color32::from_rgb(230, 90, 80);
const WARN: Color32 = Color32::from_rgb(230, 180, 60);

#[derive(Default)]
pub struct State {
    pub cfg: Option<Config>,
    pub account: Option<Account>,
    pub pods: Vec<Pod>,
    pub offers: HashMap<u32, Vec<GpuOffer>>,
    pub log: VecDeque<String>,
    pub error: Option<String>,
    pub busy: Option<String>,
    pub pulls: BTreeMap<String, PullState>,
    pub tunnel: bool,
    pub pod_models: Vec<Tag>,
    pub guard: Vec<Check>,
    pub check: Option<ChatCheck>,
    pub gpus: Vec<GpuStat>,
    pub idle: Option<Idle>,
    pub zed: ZedStatus,
}

impl State {
    pub fn apply(&mut self, u: Update) {
        match u {
            Update::Config(c) => self.cfg = Some(*c),
            Update::Account(a) => self.account = Some(a),
            Update::Pods(p) => {
                self.pods = p;
                if self.current_pod().is_none() {
                    self.tunnel = false;
                    self.gpus.clear();
                    self.idle = None;
                }
            }
            Update::Offers(n, o) => {
                self.offers.insert(n, o);
            }
            Update::Log(s) => self.push_log(s),
            Update::Error(e) => {
                self.push_log(format!("error: {e}"));
                self.error = Some(e);
            }
            Update::Busy(b) => self.busy = b,
            Update::Pull { model, state } => {
                if state == PullState::Done {
                    self.pulls.remove(&model);
                } else {
                    self.pulls.insert(model, state);
                }
            }
            Update::Tunnel(t) => self.tunnel = t,
            Update::PodModels(m) => self.pod_models = m,
            Update::Guard(g) => self.guard = g,
            Update::Check(c) => self.check = Some(c),
            Update::Gpu(g) => self.gpus = g,
            Update::Idle(i) => self.idle = Some(i),
            Update::Zed(z) => self.zed = z,
        }
    }

    fn push_log(&mut self, s: String) {
        if self.log.len() >= LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(s);
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        self.cfg.as_ref()?.active().ok()
    }

    /// The live pod of the active profile.
    pub fn current_pod(&self) -> Option<&Pod> {
        let name = spec::pod_name(self.active_profile()?);
        self.pods
            .iter()
            .find(|p| p.name == name && p.desired_status != "TERMINATED")
    }

    /// The cheapest offer that matches the profile's GPU list and count right now.
    pub fn best_offer(&self, p: &Profile) -> Option<&GpuOffer> {
        self.offers
            .get(&p.gpu_count)?
            .iter()
            .filter(|o| p.gpu_type_ids.contains(&o.id) && o.price_per_hr.is_some())
            .min_by(|a, b| {
                a.price_per_hr
                    .unwrap_or(f64::MAX)
                    .total_cmp(&b.price_per_hr.unwrap_or(f64::MAX))
            })
    }

    pub fn runway_with(&self, extra: f64) -> Option<f64> {
        self.account.as_ref()?.runway_hours(extra)
    }

    pub fn pod_session_cost(&self, now_unix: i64) -> Option<f64> {
        let p = self.current_pod()?;
        let t = cost::parse_timestamp(p.last_started_at.as_deref()?)?;
        Some(cost::session_cost(p.cost_per_hr, t, now_unix))
    }
}

/// Whether the profile's largest model fits the offer's VRAM with room for its cache.
pub fn fits(p: &Profile, offer: &GpuOffer) -> bool {
    let largest = p.models.iter().map(|m| m.size_gb).fold(0.0, f64::max);
    largest * 1.15 <= f64::from(offer.total_vram_gb())
}

pub fn money(v: f64) -> String {
    format!("${v:.2}")
}

pub fn hours(h: Option<f64>) -> String {
    match h {
        None => "unlimited".into(),
        Some(h) if h >= 48.0 => format!("{:.1} days", h / 24.0),
        Some(h) => format!("{h:.1} h"),
    }
}

#[derive(Default)]
struct Ui {
    pull_input: String,
    default_model: Option<String>,
    confirm_launch: bool,
    launch_override: bool,
    confirm_shutdown: bool,
    confirm_close: bool,
    allow_close: bool,
    auto_stop_input: String,
}

pub struct App {
    cmd: Sender<Cmd>,
    upd: Receiver<Update>,
    out: Outbox,
    pub st: State,
    ui: Ui,
}

impl App {
    pub fn new(cmd: Sender<Cmd>, upd: Receiver<Update>, out: Outbox) -> Self {
        Self {
            cmd,
            upd,
            out,
            st: State::default(),
            ui: Ui::default(),
        }
    }

    fn send(&self, c: Cmd) {
        // The worker only stops when the app does.
        let _ = self.cmd.send(c);
    }

    fn drain(&mut self) {
        while let Ok(u) = self.upd.try_recv() {
            self.st.apply(u);
        }
    }

    pub fn draw(&mut self, root: &mut egui::Ui) {
        self.drain();
        let ctx = root.ctx().clone();
        ctx.request_repaint_after(Duration::from_secs(1));
        self.handle_close(&ctx);
        egui::Panel::top("top").show(root, |ui| self.top_bar(ui));
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(150.0)
            .show(root, |ui| self.log_panel(ui));
        egui::Panel::left("profiles")
            .resizable(true)
            .default_size(360.0)
            .show(root, |ui| self.profiles_panel(ui));
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.pod_panel(ui));
        });
        self.modals(&ctx);
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested())
            && self.st.current_pod().is_some()
            && !self.ui.allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ui.confirm_close = true;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("podbay");
            ui.separator();
            match &self.st.account {
                Some(a) => {
                    ui.label(format!("Balance {}", money(a.client_balance)));
                    ui.label(format!("Spending {}/hr", money(a.current_spend_per_hr)));
                    let runway = a.runway_hours(0.0);
                    let color = match runway {
                        Some(h) if h < 2.0 => BAD,
                        Some(h) if h < 8.0 => WARN,
                        _ => OK,
                    };
                    ui.label(RichText::new(format!("Runway {}", hours(runway))).color(color));
                }
                None => {
                    ui.label("Reading the account…");
                }
            }
            if let Some(b) = &self.st.busy {
                ui.separator();
                ui.spinner();
                ui.label(b);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.send(Cmd::Refresh);
                }
            });
        });
        if let Some(e) = self.st.error.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Error: {e}")).color(BAD));
                if ui.small_button("Dismiss").clicked() {
                    self.st.error = None;
                }
            });
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Activity").strong());
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.st.log {
                    let color = if line.starts_with("error:") {
                        BAD
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.label(RichText::new(line).monospace().color(color));
                }
            });
    }

    fn profiles_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let Some(cfg) = self.st.cfg.clone() else {
            ui.label("Loading…");
            return;
        };
        let mut chosen = cfg.active_profile.clone();
        for p in &cfg.profiles {
            ui.radio_value(
                &mut chosen,
                p.name.clone(),
                format!("{} ({:?})", p.name, p.tier),
            );
        }
        if chosen != cfg.active_profile {
            self.send(Cmd::SetProfile(chosen));
        }
        ui.separator();
        let Some(p) = self.st.active_profile().cloned() else {
            return;
        };
        egui::Grid::new("profile").num_columns(2).show(ui, |ui| {
            ui.label("GPUs");
            ui.label(format!("{} × first free of:", p.gpu_count));
            ui.end_row();
            for g in &p.gpu_type_ids {
                ui.label("");
                ui.label(g.trim_start_matches("NVIDIA "));
                ui.end_row();
            }
            ui.label("Models");
            ui.label(
                p.models
                    .iter()
                    .map(|m| format!("{} ({:.0} GB)", m.name, m.size_gb))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            ui.end_row();
            ui.label("Context");
            ui.label(format!("{} tokens", p.context_length));
            ui.end_row();
            ui.label("Storage");
            ui.label(match &p.network_volume_id {
                Some(v) => format!("network volume {v} (kept between pods)"),
                None => format!("{} GB pod disk (deleted with the pod)", p.volume_gb),
            });
            ui.end_row();
        });
        ui.separator();
        match self.st.best_offer(&p) {
            Some(o) => {
                let price = o.price_per_hr.unwrap_or(0.0);
                ui.label(format!(
                    "Cheapest free match: {} × {} at {}/hr",
                    p.gpu_count,
                    o.id.trim_start_matches("NVIDIA "),
                    money(price)
                ));
                let fit = fits(&p, o);
                ui.label(
                    RichText::new(format!(
                        "{} GB VRAM: {}",
                        o.total_vram_gb(),
                        if fit {
                            "the largest model fits"
                        } else {
                            "the largest model may not fit"
                        }
                    ))
                    .color(if fit { OK } else { WARN }),
                );
                ui.label(format!(
                    "Runway with this pod: {}",
                    hours(self.st.runway_with(price))
                ));
            }
            None => {
                ui.label(
                    RichText::new(format!(
                        "No listed GPU has {} free right now; RunPod may refuse the pod.",
                        p.gpu_count
                    ))
                    .color(WARN),
                );
            }
        }
        ui.add_space(6.0);
        let can_launch = self.st.current_pod().is_none() && self.st.busy.is_none();
        if ui
            .add_enabled(
                can_launch,
                egui::Button::new(RichText::new("Launch pod").strong()),
            )
            .clicked()
        {
            self.ui.confirm_launch = true;
            self.ui.launch_override = false;
        }
        ui.separator();
        ui.collapsing("GPU market", |ui| {
            if let Some(offers) = self.st.offers.get(&p.gpu_count) {
                egui::Grid::new("market").striped(true).show(ui, |ui| {
                    ui.label(RichText::new(format!("GPU ×{}", p.gpu_count)).strong());
                    ui.label(RichText::new("VRAM").strong());
                    ui.label(RichText::new("$/hr").strong());
                    ui.end_row();
                    for o in offers.iter().filter(|o| o.price_per_hr.is_some()) {
                        let listed = p.gpu_type_ids.contains(&o.id);
                        let name = RichText::new(o.id.trim_start_matches("NVIDIA "));
                        ui.label(if listed { name.strong() } else { name });
                        ui.label(format!("{} GB", o.total_vram_gb()));
                        ui.label(o.price_per_hr.map(money).unwrap_or_default());
                        ui.end_row();
                    }
                });
            }
        });
        ui.collapsing("Auto-stop", |ui| {
            let current = cfg.auto_stop_idle_minutes;
            ui.label(match current {
                Some(m) => format!("Terminate the pod after {m} minutes with every GPU idle."),
                None => "Off: the pod runs until you shut it down.".into(),
            });
            ui.horizontal(|ui| {
                ui.label("Minutes:");
                ui.text_edit_singleline(&mut self.ui.auto_stop_input);
                if ui.button("Set").clicked()
                    && let Ok(m) = self.ui.auto_stop_input.trim().parse::<u32>()
                    && m >= 5
                {
                    self.send(Cmd::SetAutoStop(Some(m)));
                }
                if ui.button("Off").clicked() {
                    self.send(Cmd::SetAutoStop(None));
                }
            });
        });
    }

    fn pod_panel(&mut self, ui: &mut egui::Ui) {
        let Some(pod) = self.st.current_pod().cloned() else {
            ui.heading("No pod running");
            ui.label("Pick a profile and launch it. Models then run on RunPod, and Zed reaches them only through podbay's tunnel.");
            let others: Vec<&Pod> = self
                .st
                .pods
                .iter()
                .filter(|p| !p.name.starts_with("podbay-"))
                .collect();
            if !others.is_empty() {
                ui.separator();
                ui.label(
                    RichText::new("Other pods on the account (podbay leaves these alone)").weak(),
                );
                for p in others {
                    ui.label(format!(
                        "{} — {} — {}/hr",
                        p.name,
                        p.desired_status,
                        money(p.cost_per_hr)
                    ));
                }
            }
            return;
        };
        ui.heading(format!("{} ({})", pod.name, pod.id));
        egui::Grid::new("pod").num_columns(2).show(ui, |ui| {
            ui.label("Status");
            ui.label(&pod.desired_status);
            ui.end_row();
            ui.label("GPU");
            ui.label(format!(
                "{} × {}",
                pod.gpu_count.max(1),
                pod.gpu_type().unwrap_or("?")
            ));
            ui.end_row();
            ui.label("Cost");
            ui.label(format!(
                "{}/hr, {} since start",
                money(pod.cost_per_hr),
                self.st
                    .pod_session_cost(cost::now_unix())
                    .map(money)
                    .unwrap_or_else(|| "?".into())
            ));
            ui.end_row();
            ui.label("SSH");
            ui.label(
                pod.ssh_endpoint()
                    .map_or("not yet".into(), |(h, p)| format!("{h}:{p}")),
            );
            ui.end_row();
            ui.label("Tunnel");
            let port = self.st.cfg.as_ref().map_or(0, |c| c.tunnel_port);
            ui.label(if self.st.tunnel {
                RichText::new(format!("open on 127.0.0.1:{port}")).color(OK)
            } else {
                RichText::new("closed: Zed's pod models are unreachable").color(WARN)
            });
            ui.end_row();
            if !self.st.gpus.is_empty() {
                ui.label("GPU load");
                ui.label(
                    self.st
                        .gpus
                        .iter()
                        .map(|g| {
                            format!(
                                "#{} {}% · {:.0}/{:.0} GB",
                                g.index,
                                g.util_pct,
                                g.mem_used_mb as f64 / 1024.0,
                                g.mem_total_mb as f64 / 1024.0
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                ui.end_row();
            }
            if let Some(i) = self.st.idle {
                ui.label("Idle");
                ui.label(match i {
                    Idle::Busy => "busy".to_string(),
                    Idle::Idle(d) => format!("idle {} min", d.as_secs() / 60),
                    Idle::Stop => "idle limit reached".to_string(),
                    Idle::Unknown => "no GPU reading".to_string(),
                });
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            let busy = self.st.busy.is_some();
            if self.st.tunnel {
                if ui.button("Close tunnel").clicked() {
                    self.send(Cmd::CloseTunnel);
                }
            } else if ui
                .add_enabled(!busy, egui::Button::new("Open tunnel"))
                .clicked()
            {
                self.send(Cmd::OpenTunnel);
            }
            if ui.button("Open /workspace in Zed").clicked() {
                self.send(Cmd::OpenZedRemote);
            }
            if ui.button(RichText::new("Shut down").color(BAD)).clicked() {
                self.ui.confirm_shutdown = true;
            }
        });

        ui.separator();
        ui.heading("Models on the pod");
        if self.st.pod_models.is_empty() {
            ui.label(RichText::new("Open the tunnel to list them.").weak());
        }
        for m in self.st.pod_models.clone() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}  ({:.1} GB, {} {})",
                    m.name,
                    m.size as f64 / 1e9,
                    m.details.parameter_size,
                    m.details.quantization_level
                ));
                if ui
                    .add_enabled(
                        self.st.busy.is_none() && self.st.tunnel,
                        egui::Button::new("Test"),
                    )
                    .clicked()
                {
                    self.send(Cmd::Check(m.name.clone()));
                }
            });
        }
        for (model, state) in &self.st.pulls {
            match state {
                PullState::Running {
                    status,
                    completed,
                    total,
                } if *total > 0 => {
                    let frac = *completed as f32 / *total as f32;
                    ui.add(egui::ProgressBar::new(frac).text(format!(
                        "{model}: {:.1} / {:.1} GB ({status})",
                        *completed as f64 / 1e9,
                        *total as f64 / 1e9
                    )));
                }
                PullState::Running { status, .. } => {
                    ui.label(format!("{model}: {status}"));
                }
                PullState::Failed(e) => {
                    ui.label(RichText::new(format!("{model}: {e}")).color(BAD));
                }
                PullState::NotStarted | PullState::Done => {}
            }
        }
        ui.horizontal(|ui| {
            ui.label("Pull:");
            ui.add(
                egui::TextEdit::singleline(&mut self.ui.pull_input).hint_text("e.g. gpt-oss:120b"),
            );
            let name = self.ui.pull_input.trim().to_string();
            if ui
                .add_enabled(
                    !name.is_empty() && self.st.busy.is_none(),
                    egui::Button::new("Pull"),
                )
                .clicked()
            {
                self.send(Cmd::Pull(name));
                self.ui.pull_input.clear();
            }
        });
        if let Some(c) = &self.st.check {
            ui.label(
                RichText::new(format!(
                    "{}: answered in {:.1}s over {} streamed chunks; tool call: {}",
                    c.model,
                    c.seconds,
                    c.streamed_chunks,
                    c.tool_call.as_deref().unwrap_or("none")
                ))
                .color(if c.tool_call.is_some() { OK } else { WARN }),
            );
        }

        ui.separator();
        ui.heading("Zed");
        let provider = self
            .st
            .cfg
            .as_ref()
            .map_or("podbay".into(), |c| c.zed_provider.clone());
        if self.st.zed.provider_models.is_empty() {
            ui.label(RichText::new("podbay's provider is not in Zed's settings yet.").color(WARN));
        } else {
            ui.label(format!(
                "Provider \"{provider}\" offers: {}",
                self.st.zed.provider_models.join(", ")
            ));
        }
        ui.label(match &self.st.zed.default {
            Some(d) => format!("Zed's default agent model: {} / {}", d.provider, d.model),
            None => "Zed has no default agent model set.".into(),
        });
        if !self.st.zed.key_env {
            ui.label(RichText::new(format!(
                "{} is not set yet; podbay sets it when it writes the provider. Restart Zed once afterwards.",
                podbay_core::zed::api_key_env_name(&provider)
            )).color(WARN));
        }
        ui.horizontal(|ui| {
            let names: Vec<String> = self
                .st
                .active_profile()
                .map(|p| p.models.iter().map(|m| m.name.clone()).collect())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("default_model")
                .selected_text(
                    self.ui
                        .default_model
                        .clone()
                        .unwrap_or_else(|| "keep Zed's default".into()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.ui.default_model, None, "keep Zed's default");
                    for n in names {
                        ui.selectable_value(&mut self.ui.default_model, Some(n.clone()), n);
                    }
                });
            if ui
                .add_enabled(
                    self.st.busy.is_none(),
                    egui::Button::new("Write Zed provider"),
                )
                .clicked()
            {
                self.send(Cmd::ConfigureZed {
                    default_model: self.ui.default_model.clone(),
                });
            }
            if ui.button("Remove from Zed").clicked() {
                self.send(Cmd::RemoveZed);
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Never on this GPU");
            if ui
                .add_enabled(self.st.busy.is_none(), egui::Button::new("Run checks"))
                .clicked()
            {
                self.send(Cmd::Guard);
            }
        });
        if self.st.guard.is_empty() {
            ui.label(RichText::new("Not run yet.").weak());
        }
        for c in &self.st.guard {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if c.ok { "✔" } else { "✖" }).color(if c.ok {
                    OK
                } else {
                    BAD
                }));
                ui.label(RichText::new(c.name).strong());
                ui.label(RichText::new(&c.detail).weak());
            });
        }
    }

    fn modals(&mut self, ctx: &egui::Context) {
        if self.ui.confirm_launch {
            let p = self.st.active_profile().cloned();
            let offer = p.as_ref().and_then(|p| self.st.best_offer(p)).cloned();
            let r = egui::Modal::new(egui::Id::new("confirm_launch")).show(ctx, |ui| {
                ui.heading("Launch pod?");
                let Some(p) = p else { return };
                let price = offer.as_ref().and_then(|o| o.price_per_hr).unwrap_or(0.0);
                let runway = self.st.runway_with(price);
                ui.label(format!(
                    "{}: {} × {} at about {}/hr.",
                    p.name,
                    p.gpu_count,
                    offer.as_ref().map_or("first free listed GPU", |o| o.id.as_str()),
                    money(price)
                ));
                ui.label(format!("Downloads about {:.0} GB of models on the pod.", p.total_model_gb()));
                ui.label(format!("Runway with this pod: {}.", hours(runway)));
                let low = runway.is_some_and(|h| h < 1.0);
                if low {
                    ui.label(RichText::new("Under an hour of runway. At zero RunPod stops every pod on the account, including ones podbay does not manage.").color(BAD));
                    ui.checkbox(&mut self.ui.launch_override, "Launch anyway");
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!low || self.ui.launch_override, egui::Button::new("Launch")).clicked() {
                        self.send(Cmd::Launch);
                        self.ui.confirm_launch = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.ui.confirm_launch = false;
                    }
                });
            });
            if r.should_close() {
                self.ui.confirm_launch = false;
            }
        }
        if self.ui.confirm_shutdown || self.ui.confirm_close {
            let closing = self.ui.confirm_close;
            let pod = self.st.current_pod().cloned();
            let r = egui::Modal::new(egui::Id::new("confirm_shutdown")).show(ctx, |ui| {
                let Some(pod) = pod else {
                    ui.label("The pod is already gone.");
                    if ui.button("OK").clicked() {
                        self.ui.confirm_shutdown = false;
                        if closing {
                            self.ui.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    return;
                };
                ui.heading(if closing {
                    "The pod is still running"
                } else {
                    "Shut down the pod?"
                });
                ui.label(format!(
                    "{} costs {}/hr while it runs.",
                    pod.name,
                    money(pod.cost_per_hr)
                ));
                ui.label(if pod.network_volume_id.is_some() {
                    "Terminating keeps the models on the network volume."
                } else {
                    "Terminating deletes the pod disk and the models pulled onto it."
                });
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Terminate").color(BAD)).clicked() {
                        // Close the tunnel on purpose first, so the supervisor does
                        // not try to reopen it to a pod that is going away.
                        self.send(Cmd::CloseTunnel);
                        worker::shutdown_now(pod.clone(), self.out.clone());
                        self.ui.confirm_shutdown = false;
                        if closing {
                            self.ui.allow_close = true;
                            self.ui.confirm_close = false;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                    if closing && ui.button("Keep it running and quit").clicked() {
                        self.ui.allow_close = true;
                        self.ui.confirm_close = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Cancel").clicked() {
                        self.ui.confirm_shutdown = false;
                        self.ui.confirm_close = false;
                    }
                });
            });
            if r.should_close() {
                self.ui.confirm_shutdown = false;
                self.ui.confirm_close = false;
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use podbay_core::config::Config;
    use std::sync::mpsc;

    fn pod(name: &str) -> Pod {
        serde_json::from_value(serde_json::json!({
            "id": "abc", "name": name, "desiredStatus": "RUNNING", "costPerHr": 1.59,
            "publicIp": "1.2.3.4", "portMappings": {"22": 2222}, "ports": ["22/tcp"],
            "lastStartedAt": "2026-10-02 16:00:00.000 +0000 UTC", "gpuCount": 1,
            "machine": {"gpuTypeId": "NVIDIA A100-SXM4-80GB"}
        }))
        .expect("pod json")
    }

    fn offer(id: &str, mem: u32, price: Option<f64>) -> GpuOffer {
        GpuOffer {
            id: id.into(),
            display_name: id.into(),
            memory_gb: mem,
            gpu_count: 1,
            price_per_hr: price,
            stock: None,
        }
    }

    fn state() -> State {
        let mut s = State::default();
        s.apply(Update::Config(Box::default()));
        s.apply(Update::Account(Account {
            client_balance: 13.4,
            current_spend_per_hr: 0.8,
            spend_limit: None,
        }));
        s
    }

    #[test]
    fn current_pod_matches_the_active_profile_only() {
        let mut s = state();
        s.apply(Update::Pods(vec![
            pod("ai-playtest-personas"),
            pod("podbay-small"),
        ]));
        assert!(
            s.current_pod().is_none(),
            "medium is active; small's pod is not it"
        );
        s.apply(Update::Pods(vec![pod("podbay-medium")]));
        assert_eq!(
            s.current_pod().map(|p| p.name.as_str()),
            Some("podbay-medium")
        );
    }

    #[test]
    fn losing_the_pod_clears_tunnel_and_gpu_state() {
        let mut s = state();
        s.apply(Update::Pods(vec![pod("podbay-medium")]));
        s.apply(Update::Tunnel(true));
        s.apply(Update::Idle(Idle::Busy));
        s.apply(Update::Pods(vec![]));
        assert!(!s.tunnel);
        assert!(s.idle.is_none());
    }

    #[test]
    fn best_offer_is_cheapest_free_listed_gpu() {
        let mut s = state();
        s.apply(Update::Offers(
            1,
            vec![
                offer("NVIDIA A40", 48, Some(0.49)),
                offer("NVIDIA H100 80GB HBM3", 80, Some(3.49)),
                offer("NVIDIA A100-SXM4-80GB", 80, Some(1.59)),
                offer("NVIDIA A100 80GB PCIe", 80, None),
            ],
        ));
        let p = s.active_profile().expect("active").clone();
        let best = s.best_offer(&p).expect("an offer");
        assert_eq!(
            best.id, "NVIDIA A100-SXM4-80GB",
            "A40 is cheaper but not in the medium list"
        );
        assert!(fits(&p, best));
        assert!(
            !fits(&p, &offer("x", 48, Some(1.0))),
            "gpt-oss:120b (65 GB) cannot fit 48 GB"
        );
    }

    #[test]
    fn pulls_and_logs_fold_in() {
        let mut s = state();
        s.apply(Update::Pull {
            model: "m".into(),
            state: PullState::Running {
                status: "pulling".into(),
                completed: 1,
                total: 2,
            },
        });
        assert_eq!(s.pulls.len(), 1);
        s.apply(Update::Pull {
            model: "m".into(),
            state: PullState::Done,
        });
        assert!(s.pulls.is_empty());
        for i in 0..(LOG_CAP + 10) {
            s.apply(Update::Log(format!("{i}")));
        }
        assert_eq!(s.log.len(), LOG_CAP);
        s.apply(Update::Error("boom".into()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert_eq!(s.log.back().map(String::as_str), Some("error: boom"));
    }

    #[test]
    fn money_and_hours_format() {
        assert_eq!(money(1.594), "$1.59");
        assert_eq!(hours(None), "unlimited");
        assert_eq!(hours(Some(5.62)), "5.6 h");
        assert_eq!(hours(Some(72.0)), "3.0 days");
    }

    pub(crate) fn test_app() -> (App, mpsc::Receiver<Cmd>, mpsc::Sender<Update>) {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (upd_tx, upd_rx) = mpsc::channel();
        let out = Outbox::new(upd_tx.clone(), egui::Context::default());
        (App::new(cmd_tx, upd_rx, out), cmd_rx, upd_tx)
    }
}

#[cfg(test)]
pub(crate) use tests::test_app;

/// Click-through tests: render the real UI in egui's test harness, feed it worker
/// updates, click, and check which commands reach the worker.
#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use podbay_core::config::Config;
    use std::sync::mpsc::Receiver;

    fn pod(name: &str) -> Pod {
        serde_json::from_value(serde_json::json!({
            "id": "abc", "name": name, "desiredStatus": "RUNNING", "costPerHr": 1.59,
            "publicIp": "1.2.3.4", "portMappings": {"22": 2222}, "ports": ["22/tcp"], "gpuCount": 1
        }))
        .expect("pod json")
    }

    fn harness(updates: Vec<Update>) -> (Harness<'static, App>, Receiver<Cmd>) {
        let (mut app, cmds, _tx) = test_app();
        app.st.apply(Update::Config(Box::default()));
        app.st.apply(Update::Account(Account {
            client_balance: 13.4,
            current_spend_per_hr: 0.8,
            spend_limit: None,
        }));
        app.st.apply(Update::Offers(
            1,
            vec![GpuOffer {
                id: "NVIDIA A100-SXM4-80GB".into(),
                display_name: "A100".into(),
                memory_gb: 80,
                gpu_count: 1,
                price_per_hr: Some(1.59),
                stock: Some("Low".into()),
            }],
        ));
        for u in updates {
            app.st.apply(u);
        }
        let h = Harness::builder()
            .with_size(egui::vec2(1180.0, 900.0))
            .build_ui_state(|ui, app: &mut App| app.draw(ui), app);
        (h, cmds)
    }

    fn sent(cmds: &Receiver<Cmd>) -> Vec<Cmd> {
        cmds.try_iter().collect()
    }

    #[test]
    fn launch_needs_confirmation_and_then_sends_launch() {
        let (mut h, cmds) = harness(vec![]);
        h.run();
        h.get_by_label_contains("Cheapest free match");
        h.get_by_label("Launch pod").click();
        h.run();
        assert!(
            !sent(&cmds).contains(&Cmd::Launch),
            "the first click only opens the dialog"
        );
        h.get_by_label("Launch pod?");
        h.get_by_label("Launch").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::Launch));
    }

    #[test]
    fn low_runway_blocks_launch_until_overridden() {
        let (mut h, cmds) = harness(vec![Update::Account(Account {
            client_balance: 1.0,
            current_spend_per_hr: 0.8,
            spend_limit: None,
        })]);
        h.run();
        h.get_by_label("Launch pod").click();
        h.run();
        h.get_by_label_contains("Under an hour of runway");
        h.get_by_label("Launch").click();
        h.run();
        assert!(
            !sent(&cmds).contains(&Cmd::Launch),
            "disabled until the override is ticked"
        );
        h.get_by_label("Launch anyway").click();
        h.run();
        h.get_by_label("Launch").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::Launch));
    }

    #[test]
    fn running_pod_shows_controls_and_tunnel_state() {
        let (mut h, cmds) = harness(vec![Update::Pods(vec![
            pod("podbay-medium"),
            pod("ai-playtest-personas"),
        ])]);
        h.run();
        h.get_by_label("podbay-medium (abc)");
        h.get_by_label_contains("closed: Zed's pod models are unreachable");
        h.get_by_label("Open tunnel").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::OpenTunnel));
        h.state_mut().st.apply(Update::Tunnel(true));
        h.run();
        h.get_by_label_contains("open on 127.0.0.1:11435");
        h.get_by_label("Close tunnel").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::CloseTunnel));
    }

    #[test]
    fn shutdown_asks_and_explains_data_loss() {
        let (mut h, cmds) = harness(vec![Update::Pods(vec![pod("podbay-medium")])]);
        h.run();
        h.get_by_label("Shut down").click();
        h.run();
        h.get_by_label("Shut down the pod?");
        h.get_by_label_contains("deletes the pod disk");
        h.get_by_label("Cancel").click();
        h.run();
        assert!(h.query_by_label("Shut down the pod?").is_none());
        assert!(sent(&cmds).is_empty(), "cancel sends nothing");
    }

    #[test]
    fn guard_results_render_pass_and_fail() {
        let (mut h, cmds) = harness(vec![
            Update::Pods(vec![pod("podbay-medium")]),
            Update::Guard(vec![
                Check {
                    name: "Tunnel avoids the local Ollama port",
                    ok: true,
                    detail: "11435".into(),
                },
                Check {
                    name: "Pod models are not on this machine",
                    ok: false,
                    detail: "also local: x".into(),
                },
            ]),
        ]);
        h.run();
        h.get_by_label("Tunnel avoids the local Ollama port");
        h.get_by_label("also local: x");
        h.get_by_label("Run checks").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::Guard));
    }

    #[test]
    fn pull_field_sends_the_typed_model() {
        let (mut h, cmds) = harness(vec![Update::Pods(vec![pod("podbay-medium")])]);
        h.state_mut().ui.pull_input = "gpt-oss:120b".into();
        h.run();
        h.get_by_label("Pull").click();
        h.run();
        assert!(sent(&cmds).contains(&Cmd::Pull("gpt-oss:120b".into())));
    }

    #[test]
    fn other_pods_are_listed_but_not_controlled() {
        let (mut h, _cmds) = harness(vec![Update::Pods(vec![pod("ai-playtest-personas")])]);
        h.run();
        h.get_by_label("No pod running");
        h.get_by_label_contains("ai-playtest-personas");
        assert!(h.query_by_label("Shut down").is_none());
    }
}
