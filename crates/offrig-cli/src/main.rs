//! `offrig`: the command-line front end. Every command maps onto offrig-core; the
//! desktop app drives the same calls.

mod budget_menu;

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use offrig_core::config::{Config, Profile};
use offrig_core::cost::{self, Idle, IdleTracker};
use offrig_core::error::chain;
use offrig_core::lanes::Registry;
use offrig_core::remote::{self, PullState};
use offrig_core::runpod::Pod;
use offrig_core::session::{Event, Session, Wait};
use offrig_core::siblings;
use offrig_core::trace::{self, Level};
use offrig_core::tunnel::{self, Tunnel};
use offrig_core::{guard, ollama::Ollama, spec, zed};

#[derive(Parser)]
#[command(
    name = "offrig",
    version,
    about = "Run models on RunPod and wire them into Zed, never on the local GPU"
)]
struct Cli {
    /// Errors only: no progress or confirmations (a command's own results still print)
    #[arg(short, long, global = true, conflicts_with_all = ["verbose", "debug"])]
    quiet: bool,
    /// Also say which RunPod calls are made and how long they take (to stderr)
    #[arg(short, long, global = true, conflicts_with = "debug")]
    verbose: bool,
    /// Everything --verbose says, plus full error chains and failed response bodies.
    /// Secrets are redacted at every level.
    #[arg(long, global = true)]
    debug: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

impl Cli {
    fn level(&self) -> Level {
        if self.debug {
            Level::Debug
        } else if self.verbose {
            Level::Verbose
        } else if self.quiet {
            Level::Quiet
        } else {
            Level::Normal
        }
    }
}

/// Progress and confirmations: printed at the normal level and above, not by `--quiet`.
macro_rules! info {
    ($($arg:tt)*) => {
        if trace::enabled(Level::Normal) {
            println!("{}", trace::redact(&format!($($arg)*)));
        }
    };
}

/// A failure that happened while running (as opposed to a mistake in what the user
/// asked for), for the exit status: the failure has no `offrig_core::Error` behind it.
#[derive(Debug)]
struct RuntimeFailure(String);

impl std::fmt::Display for RuntimeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeFailure {}

fn runtime(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(RuntimeFailure(msg.into()))
}

/// Exit status: 0 ok, 1 the user's to fix (arguments, config, guard or budget
/// refusals, missing key, not found), 2 something failed while running (RunPod API,
/// network, ssh, timeout, io, database). Nothing here is partially successful, so 3
/// is unused.
const EXIT_USER: i32 = 1;
const EXIT_RUNTIME: i32 = 2;

fn code_exit(code: &str) -> i32 {
    match code {
        "runpod_api" | "network" | "ssh" | "timeout" | "io" | "database" | "internal"
        | "model_server" | "no_capacity" | "pod_not_ready" => EXIT_RUNTIME,
        _ => EXIT_USER,
    }
}

/// The error's stable code and exit status: the first `offrig_core::Error` in its
/// chain decides; otherwise a marked runtime failure or a raw io/json error is a
/// runtime error, and anything else is a mistake in the request.
fn classify(err: &anyhow::Error) -> (&'static str, i32) {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<offrig_core::Error>() {
            return (e.code(), code_exit(e.code()));
        }
        if cause.is::<RuntimeFailure>() {
            return ("runtime", EXIT_RUNTIME);
        }
        if cause.is::<std::io::Error>() {
            return ("io", EXIT_RUNTIME);
        }
        if cause.is::<serde_json::Error>() {
            return ("internal", EXIT_RUNTIME);
        }
    }
    ("usage", EXIT_USER)
}

/// What is printed for a failure: the chain on one line, or at `--debug` the code, the
/// exit status and the full chain. Always redacted.
fn render_error(err: &anyhow::Error) -> String {
    let (code, exit) = classify(err);
    let text = if trace::enabled(Level::Debug) {
        format!("error [{code}, exit {exit}]: {err:?}")
    } else {
        format!("error: {err:#}")
    };
    trace::redact(&text)
}

#[derive(Subcommand)]
enum Cmd {
    /// Balance, runway, and pods on the account
    Status,
    /// GPU offers on secure cloud for a GPU count, cheapest first
    Gpus {
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// Only offers with at least this much VRAM in total
        #[arg(long, default_value_t = 0)]
        min_vram: u32,
    },
    /// Profiles in the config, with their models and sizes
    Profiles,
    /// Write the default config file if there is none, and print its path
    Init,
    /// Launch the profile's pod, pull its models, wire Zed, then hold the tunnel
    Up {
        profile: Option<String>,
        /// Skip the low-runway refusal
        #[arg(long)]
        yes: bool,
        /// Make this model Zed's default agent model
        #[arg(long)]
        default_model: Option<String>,
        /// Exit after setup instead of holding the tunnel open
        #[arg(long)]
        detach: bool,
        /// Leave Zed's settings alone
        #[arg(long)]
        no_zed: bool,
        /// Minutes to wait for the GPUs if none are free (overrides the profile;
        /// nothing is rented while waiting)
        #[arg(long)]
        wait: Option<u32>,
    },
    /// Hold the tunnel to the running pod open (with idle auto-stop)
    Tunnel { profile: Option<String> },
    /// Pull a model onto the pod (runs on the pod; survives this command exiting)
    Pull {
        model: String,
        profile: Option<String>,
    },
    /// Models on the pod, read over SSH
    Models { profile: Option<String> },
    /// Write offrig's provider into Zed's settings
    Zed {
        profile: Option<String>,
        #[arg(long)]
        default_model: Option<String>,
    },
    /// Remove offrig's provider from Zed's settings
    ZedRemove,
    /// Run the "never on my GPU" checks
    Guard { profile: Option<String> },
    /// Streamed chat with a tool through the tunnel, the way Zed calls it
    Check {
        model: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Open the pod in Zed for remote editing
    Connect {
        #[arg(default_value = "/workspace")]
        path: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Show or set this project's spending cap for agent-planned sessions. Only a
    /// human sets it: the side-car's tools can read it but never change it. In a
    /// terminal, with no amount, it opens a menu to set a new cap or zero it.
    Budget {
        /// New cap in USD; omit to show the current budget (a menu in a terminal)
        usd: Option<f64>,
        /// Print the budget line only, even in a terminal
        #[arg(long)]
        show: bool,
        /// Project directory (default: the current directory)
        #[arg(long)]
        project: Option<std::path::PathBuf>,
    },
    /// Stage a recipe profile's weights on a RunPod network volume so launches skip
    /// the download. The volume bills monthly until removed; only a human stages.
    Stage {
        profile: String,
        /// Data center for the volume (one with network storage and the profile's GPUs)
        #[arg(long)]
        dc: Option<String>,
        /// Delete the profile's volume and return it to downloading at launch
        #[arg(long)]
        remove: bool,
        /// Confirm the monthly charge (or the deletion)
        #[arg(long)]
        yes: bool,
    },
    /// Terminate the profile's pod
    Down {
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

fn main() {
    // A bad command line is the user's to fix: exit 1, not clap's default 2.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let help_or_version = !e.use_stderr();
            let _ = e.print();
            std::process::exit(if help_or_version { 0 } else { EXIT_USER });
        }
    };
    trace::set_level(cli.level());
    // A panic is a bug, not a stack trace for the user: one line, exit 2.
    // `--debug` keeps the default report.
    if !trace::enabled(Level::Debug) {
        std::panic::set_hook(Box::new(|info| {
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            eprintln!(
                "error: internal error: {} (re-run with --debug for details)",
                trace::redact(&msg)
            );
            std::process::exit(EXIT_RUNTIME);
        }));
    }
    let started = Instant::now();
    let result = run(cli);
    trace::verbose(&format!("finished in {} ms", started.elapsed().as_millis()));
    if let Err(e) = result {
        eprintln!("{}", render_error(&e));
        std::process::exit(classify(&e).1);
    }
}

fn print_event(last_pct: &mut HashMap<String, u64>) -> impl FnMut(Event) + '_ {
    move |e| match e {
        Event::Step(s) => info!("  - {s}"),
        Event::Warn(s) => info!("  ! {s}"),
        Event::Pod(p) => info!("  - pod {} at {:?}", p.id, p.ssh_endpoint()),
        // The step line the wait prints every few minutes already says it.
        Event::Waiting { .. } => {}
        Event::Pull { model, state } => {
            if let PullState::Running {
                status,
                completed,
                total,
            } = state
                && total > 0
            {
                let pct = completed * 100 / total;
                if last_pct.get(&model) != Some(&pct) {
                    last_pct.insert(model.clone(), pct);
                    info!(
                        "  - {model}: {pct:>3}% of {:.1} GB ({status})",
                        total as f64 / 1e9
                    );
                }
            }
        }
    }
}

fn load() -> Result<Config> {
    trace::verbose("loading the offrig config");
    Config::load().context("loading offrig config")
}

fn pick<'a>(cfg: &'a Config, name: &Option<String>) -> Result<&'a Profile> {
    Ok(match name {
        Some(n) => cfg.profile(n)?,
        None => cfg.active()?,
    })
}

fn session_for(profile: &Option<String>) -> Result<(Session, Profile)> {
    let cfg = load()?;
    let p = pick(&cfg, profile)?.clone();
    Ok((Session::new(cfg)?, p))
}

/// The profile's running pod, with the SSH alias pointed at it.
fn require_pod(s: &Session, p: &Profile) -> Result<Pod> {
    let pod = s.current_pod(p)?.with_context(|| {
        format!(
            "no running pod for profile {} (run `offrig up {}`)",
            p.name, p.name
        )
    })?;
    s.write_ssh(&pod)?;
    Ok(pod)
}

/// Use an open tunnel if there is one, otherwise open one for the command's duration.
fn ensure_tunnel(s: &Session) -> Result<Option<Tunnel>> {
    if tunnel::port_open(s.cfg.tunnel_port) {
        return Ok(None);
    }
    let mut last = HashMap::new();
    Ok(Some(s.open_tunnel(
        s.cfg.active()?,
        &mut print_event(&mut last),
    )?))
}

/// A person at a terminal: both stdin and stdout are terminals. A tool that captures
/// stdout (an agent's shell, a script, a test) gets the plain line instead of a menu.
fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Budget { usd, show, project } => {
            let dir = match project {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            let store = offrig_core::store::Store::open(&dir.join(".offrig").join("offrig.db"))?;
            if let Some(cap) = usd {
                store.set_budget_cap(cap)?;
                info!("budget cap set to ${cap:.2} for {}", dir.display());
            } else if !show && interactive() {
                // The menu names the project by its folder, never the full path.
                let name = dir.file_name().map_or_else(
                    || "this project".into(),
                    |n| n.to_string_lossy().into_owned(),
                );
                return budget_menu::run(
                    &store,
                    &name,
                    &mut std::io::stdin().lock(),
                    &mut std::io::stdout(),
                );
            }
            println!("{}", budget_menu::line(&store.budget()?));
            Ok(())
        }
        Cmd::Init => {
            let path = offrig_core::config::config_path()?;
            if path.exists() {
                println!("config already exists: {}", path.display());
            } else {
                Config::default().save()?;
                println!("wrote {}", path.display());
            }
            Ok(())
        }
        Cmd::Profiles => {
            let cfg = load()?;
            for p in &cfg.profiles {
                let mark = if p.name == cfg.active_profile {
                    "*"
                } else {
                    " "
                };
                let disk = p
                    .network_volume_id
                    .as_deref()
                    .map_or(format!("{} GB pod disk", p.volume_gb), |v| {
                        format!("network volume {v}")
                    });
                println!(
                    "{mark} {:<10} {:?}  {}x [{}]  ctx {}  {disk}",
                    p.name,
                    p.tier,
                    p.gpu_count,
                    p.gpu_type_ids.join(" | "),
                    p.context_length,
                );
                for m in &p.models {
                    println!("      {} ({:.0} GB)", m.name, m.size_gb);
                }
            }
            Ok(())
        }
        Cmd::Status => {
            let s = Session::new(load()?)?;
            let a = s.rp.account()?;
            println!(
                "balance ${:.2}, spending ${:.2}/hr, runway {}",
                a.client_balance,
                a.current_spend_per_hr,
                a.runway_hours(0.0)
                    .map_or("unlimited".into(), |h| format!("{h:.1} h"))
            );
            // Other lanes' pods are named from the lane registry, and their plans read
            // from those projects' stores, read-only. A registry that cannot be read
            // leaves them as plain "lane" pods.
            let lanes = Registry::open_default()
                .and_then(|r| r.all_if_present())
                .unwrap_or_default();
            for p in s.rp.list_pods()? {
                let ours = if s.cfg.owns_pod(&p.name) {
                    "offrig"
                } else if p.name.starts_with("offrig-") {
                    "lane  "
                } else {
                    "other "
                };
                let spent = p
                    .last_started_at
                    .as_deref()
                    .and_then(cost::parse_timestamp)
                    .map(|t| cost::session_cost(p.cost_per_hr, t, cost::now_unix()));
                println!(
                    "  [{ours}] {} {} {} ${:.2}/hr{}{}",
                    p.name,
                    p.id,
                    p.desired_status,
                    p.cost_per_hr,
                    spent.map_or(String::new(), |c| format!(", ${c:.2} this session")),
                    p.ssh_endpoint()
                        .map_or(String::new(), |(h, port)| format!(", ssh {h}:{port}")),
                );
                if ours != "offrig"
                    && let Some((lane, project)) = siblings::lane_of(&p.name, &s.cfg, &lanes)
                {
                    let sib = siblings::Sibling {
                        plan: project
                            .as_deref()
                            .map_or(siblings::PlanView::NoProject, |d| {
                                siblings::read_plan(std::path::Path::new(d), &p.id)
                            }),
                        lane,
                        project,
                        pod: p,
                    };
                    println!("      {}", sib.summary());
                }
            }
            Ok(())
        }
        Cmd::Gpus { count, min_vram } => {
            let s = Session::new(load()?)?;
            println!(
                "{:<52} {:>6} {:>10} {:>8}",
                format!("GPU (x{count})"),
                "VRAM",
                "$/hr",
                "stock"
            );
            for o in
                s.rp.gpu_offers(count)?
                    .into_iter()
                    .filter(|o| o.total_vram_gb() >= min_vram)
            {
                println!(
                    "{:<52} {:>4}GB {:>10} {:>8}",
                    o.id,
                    o.total_vram_gb(),
                    o.price_per_hr
                        .map_or("none free".into(), |p| format!("{p:.2}")),
                    o.stock.unwrap_or_default()
                );
            }
            Ok(())
        }
        Cmd::Up {
            profile,
            yes,
            default_model,
            detach,
            no_zed,
            wait,
        } => {
            let (s, p) = session_for(&profile)?;
            if p.is_job() {
                return Err(offrig_core::session::job_serves_nothing(&p).into());
            }
            preflight(&s, &p, yes)?;
            let stop = ctrl_c_flag()?;
            let minutes = wait.unwrap_or(p.wait_for_gpu_minutes);
            let mut last = HashMap::new();
            let pod = s.launch_waiting(
                &p,
                Wait::minutes(minutes),
                &stop,
                &mut print_event(&mut last),
            )?;
            let tunnel = s.open_tunnel(&p, &mut print_event(&mut last))?;
            s.ensure_models(&p, &mut print_event(&mut last))?;
            if !no_zed {
                configure_zed(&s, &p, default_model.as_deref())?;
                let checks = guard::evaluate(&guard::gather(&s.cfg, &pod, &zed::settings_path()?)?);
                print_checks(&checks);
            }
            info!(
                "ready: {} ({}) at ${:.2}/hr",
                pod.name, pod.id, pod.cost_per_hr
            );
            if detach {
                drop(tunnel);
                info!(
                    "tunnel closed (--detach). Reopen with `offrig tunnel {}`.",
                    p.name
                );
                return Ok(());
            }
            hold(&s, &p, pod, Some(tunnel), &stop)
        }
        Cmd::Tunnel { profile } => {
            let (s, p) = session_for(&profile)?;
            let pod = require_pod(&s, &p)?;
            let stop = ctrl_c_flag()?;
            hold(&s, &p, pod, None, &stop)
        }
        Cmd::Pull { model, profile } => {
            let (s, p) = session_for(&profile)?;
            require_pod(&s, &p)?;
            remote::pull_start(&s.cfg.ssh_alias, &model)?;
            info!("pull of {model} started on the pod");
            let mut last = HashMap::new();
            let mut print = print_event(&mut last);
            loop {
                std::thread::sleep(Duration::from_secs(4));
                let state = remote::pull_state(&s.cfg.ssh_alias, &model)?;
                print(Event::Pull {
                    model: model.clone(),
                    state: state.clone(),
                });
                match state {
                    PullState::Done => {
                        info!("{model} pulled");
                        return Ok(());
                    }
                    PullState::Failed(e) => {
                        return Err(runtime(format!("pull of {model} failed: {e}")));
                    }
                    PullState::NotStarted => {
                        return Err(runtime(format!("pull of {model} vanished")));
                    }
                    PullState::Running { .. } => {}
                }
            }
        }
        Cmd::Models { profile } => {
            let (s, p) = session_for(&profile)?;
            require_pod(&s, &p)?;
            // The OpenAI model list: Ollama and recipe engines both serve it.
            let json = remote::pod_models_json(&s.cfg.ssh_alias)?;
            let v: serde_json::Value = serde_json::from_str(&json)?;
            for id in offrig_core::ollama::model_ids(&v) {
                println!("  {id}");
            }
            Ok(())
        }
        Cmd::Zed {
            profile,
            default_model,
        } => {
            let (s, p) = session_for(&profile)?;
            require_pod(&s, &p)?;
            let _t = ensure_tunnel(&s)?;
            configure_zed(&s, &p, default_model.as_deref())
        }
        Cmd::ZedRemove => {
            let s = Session::new(load()?)?;
            s.unconfigure_zed(None)?;
            info!(
                "removed provider {} from Zed's settings",
                s.cfg.zed_provider
            );
            Ok(())
        }
        Cmd::Guard { profile } => {
            let (s, p) = session_for(&profile)?;
            let pod = require_pod(&s, &p)?;
            let _t = ensure_tunnel(&s)?;
            let checks = guard::evaluate(&guard::gather(&s.cfg, &pod, &zed::settings_path()?)?);
            print_checks(&checks);
            if guard::all_ok(&checks) {
                Ok(())
            } else {
                bail!("guard checks failed")
            }
        }
        Cmd::Check { model, profile } => {
            let (s, p) = session_for(&profile)?;
            require_pod(&s, &p)?;
            let _t = ensure_tunnel(&s)?;
            let model = match model {
                Some(m) => m,
                None => p
                    .models
                    .first()
                    .map(|m| m.name.clone())
                    .context("profile has no models")?,
            };
            let o = Ollama::new(&s.cfg.tunnel_base_url());
            let c = o.chat_check(&model)?;
            println!(
                "{}: {} chunks in {:.1}s, tool call: {}, text: {:?}",
                c.model,
                c.streamed_chunks,
                c.seconds,
                c.tool_call.as_deref().unwrap_or("none"),
                c.reply.chars().take(120).collect::<String>()
            );
            for l in o.loaded()? {
                println!(
                    "  loaded: {} ({:.1} GB in VRAM)",
                    l.name,
                    l.size_vram as f64 / 1e9
                );
            }
            Ok(())
        }
        Cmd::Connect { path, profile } => {
            let (s, p) = session_for(&profile)?;
            require_pod(&s, &p)?;
            let target = format!("ssh://{}{}", s.cfg.ssh_alias, path);
            Command::new("zed")
                .arg(&target)
                .spawn()
                .context("launching zed")?;
            info!("opened {target} in Zed");
            Ok(())
        }
        Cmd::Stage {
            profile,
            dc,
            remove,
            yes,
        } => stage_cmd(&profile, dc, remove, yes),
        Cmd::Down { profile, yes } => {
            let (s, p) = session_for(&profile)?;
            let Some(pod) = s.current_pod(&p)? else {
                info!("no pod for profile {}", p.name);
                return Ok(());
            };
            if !yes {
                bail!(
                    "this terminates {} ({}); {}. Re-run with --yes.",
                    pod.name,
                    pod.id,
                    if pod.network_volume_id.is_some() {
                        "models on the network volume are kept"
                    } else {
                        "its disk and pulled models are deleted"
                    }
                );
            }
            let mut last = HashMap::new();
            s.shutdown(&pod, &mut print_event(&mut last))?;
            Ok(())
        }
    }
}

/// Record (or clear) where a profile's weights are staged, in the saved config.
fn set_staging(
    cfg: &mut Config,
    name: &str,
    volume: Option<String>,
    dc: Option<String>,
) -> Result<()> {
    let p = cfg
        .profiles
        .iter_mut()
        .find(|p| p.name == name)
        .with_context(|| format!("no profile {name}"))?;
    p.network_volume_id = volume;
    p.data_center_id = dc;
    cfg.save().context("saving the offrig config")
}

fn stage_cmd(name: &str, dc: Option<String>, remove: bool, yes: bool) -> Result<()> {
    let mut cfg = load()?;
    let p = pick(&cfg, &Some(name.to_string()))?.clone();
    let rp = offrig_core::runpod::RunPod::from_env()?;
    if remove {
        let Some(vol) = p.network_volume_id.clone() else {
            bail!("profile {} has no staged volume", p.name);
        };
        if !yes {
            bail!(
                "this deletes network volume {vol} and the weights on it; launches of {} go back to downloading. Re-run with --yes.",
                p.name
            );
        }
        rp.delete_volume(&vol)?;
        set_staging(&mut cfg, &p.name, None, None)?;
        info!(
            "deleted volume {vol}; {} downloads its weights at launch again",
            p.name
        );
        return Ok(());
    }
    // Before anything that creates or costs.
    offrig_core::stage::stageable(&p)?;
    let dc = dc
        .or_else(|| p.data_center_id.clone())
        .context("pass --dc <data center id>: one with network storage and the profile's GPUs")?;
    let size = offrig_core::stage::volume_size_gb(&p);
    if !yes {
        bail!(
            "staging {} creates (or reuses) a {size} GB network volume in {dc}: about ${:.2}/month, \
             billed until `offrig stage {} --remove --yes`, plus a staging pod for roughly 20 minutes. \
             Its pods will then launch only in {dc}. Re-run with --yes.",
            p.name,
            offrig_core::stage::monthly_usd(size),
            p.name
        );
    }
    let (vol, created) = offrig_core::stage::ensure_volume(&rp, &p, &dc)?;
    info!(
        "{} volume {} ({} GB) in {}",
        if created { "created" } else { "reusing" },
        vol.id,
        vol.size,
        vol.data_center_id
    );
    // Record the volume before filling it, so a failed fill is never an orphan the
    // config forgets: `--remove` can always find it.
    set_staging(
        &mut cfg,
        &p.name,
        Some(vol.id.clone()),
        Some(vol.data_center_id.clone()),
    )?;
    let mut last = HashMap::new();
    if let Err(e) = offrig_core::stage::fill(&cfg, rp, &p, &vol, &mut print_event(&mut last)) {
        // The core error stays in the chain, so the exit status and code follow it.
        return Err(anyhow::Error::new(e).context(format!(
            "staging failed. The staging pod was terminated. Volume {} is kept and still              bills: run `offrig stage {} --yes` to resume, or `offrig stage {} --remove --yes`",
            vol.id, p.name, p.name
        )));
    }
    info!(
        "{} is staged on {} in {}: its launches skip the download and run Hugging Face offline",
        p.name, vol.id, vol.data_center_id
    );
    Ok(())
}

fn preflight(s: &Session, p: &Profile, yes: bool) -> Result<()> {
    s.plan_check(p)?;
    // Prices and the balance come from RunPod's GraphQL API. Launching needs only
    // REST, so if GraphQL is down or retired, warn and launch without the estimate.
    let offers = match s.rp.gpu_offers_in(p.gpu_count, p.data_center_id.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            info!(
                "  ! could not read GPU prices ({}); launching without an estimate",
                chain(&e)
            );
            Vec::new()
        }
    };
    let best = offers
        .iter()
        .filter(|o| p.gpu_type_ids.contains(&o.id))
        .filter_map(|o| o.price_per_hr.map(|price| (o, price)))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((o, price)) => info!(
            "{}: cheapest free match {}x {} ({} GB VRAM) at ${price:.2}/hr",
            p.name,
            p.gpu_count,
            o.id,
            o.total_vram_gb()
        ),
        None => info!(
            "{}: none of [{}] has {} GPU(s) free on secure cloud right now; RunPod may refuse the pod",
            p.name,
            p.gpu_type_ids.join(" | "),
            p.gpu_count
        ),
    }
    // A pod that is already up is in the account's current spend; do not add it twice.
    let already_up = s.current_pod(p)?.is_some();
    let price = if already_up {
        0.0
    } else {
        best.map_or(0.0, |b| b.1)
    };
    let account = match s.rp.account() {
        Ok(a) => a,
        Err(e) => {
            info!(
                "  ! could not read the balance ({}); check runway in the RunPod console",
                chain(&e)
            );
            return Ok(());
        }
    };
    let runway = account.runway_hours(price);
    info!(
        "balance ${:.2}; with this pod the account spends ${:.2}/hr; runway {}",
        account.client_balance,
        account.current_spend_per_hr + price,
        runway.map_or("unlimited".into(), |h| format!("{h:.1} h"))
    );
    if let Some(h) = runway
        && h < 1.0
        && !yes
    {
        bail!(
            "under an hour of runway; at zero RunPod stops every pod on the account. Add funds or pass --yes."
        );
    }
    Ok(())
}

fn configure_zed(s: &Session, p: &Profile, default_model: Option<&str>) -> Result<()> {
    let models = s.zed_models(p)?;
    let out = s.configure_zed(&models, default_model)?;
    info!(
        "Zed provider {} written to {}",
        s.cfg.zed_provider,
        out.settings.display()
    );
    for m in &out.models {
        info!(
            "  {} (ctx {}, tools {}, images {})",
            m.display_name, m.max_tokens, m.tools, m.images
        );
    }
    if let Some(d) = default_model {
        info!("  Zed's default agent model is now {d}");
    }
    if out.api_key_env_ready {
        info!(
            "  {} is set; restart Zed once if it was just created",
            zed::api_key_env_name(&s.cfg.zed_provider)
        );
    }
    Ok(())
}

fn print_checks(checks: &[guard::Check]) {
    println!("guard:");
    for c in checks {
        println!(
            "  [{}] {}: {}",
            if c.ok { "ok" } else { "FAIL" },
            c.name,
            c.detail
        );
    }
}

/// One Ctrl+C flag per run (the handler can be installed only once): it cancels a
/// GPU wait, then closes a held tunnel.
fn ctrl_c_flag() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .context("installing the Ctrl+C handler")?;
    Ok(stop)
}

/// Keep the tunnel up until Ctrl+C, reopening it if it drops, and terminate the pod
/// after the configured idle window.
fn hold(
    s: &Session,
    p: &Profile,
    pod: Pod,
    tunnel: Option<Tunnel>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut last = HashMap::new();
    let mut tunnel = match tunnel {
        Some(t) => t,
        None => s.open_tunnel(p, &mut print_event(&mut last))?,
    };
    let mut idle = s
        .cfg
        .auto_stop_idle_minutes
        .map(|m| IdleTracker::new(Duration::from_secs(u64::from(m) * 60)));
    info!(
        "holding the tunnel on 127.0.0.1:{} (Ctrl+C closes it; the pod keeps running){}",
        s.cfg.tunnel_port,
        s.cfg
            .auto_stop_idle_minutes
            .map_or(String::new(), |m| format!(
                "; the pod is terminated after {m} idle minutes"
            ))
    );
    let mut next_sample = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if !tunnel.is_alive() {
            info!("  ! tunnel dropped ({}); reopening", tunnel.last_error());
            std::thread::sleep(Duration::from_secs(3));
            match s.current_pod(p)? {
                Some(fresh) => s.write_ssh(&fresh)?,
                None => return Err(runtime("the pod is gone")),
            }
            tunnel = s.open_tunnel(p, &mut print_event(&mut last))?;
        }
        if Instant::now() >= next_sample {
            next_sample = Instant::now() + Duration::from_secs(60);
            if let Some(tracker) = idle.as_mut() {
                let stats = remote::gpu_stats(&s.cfg.ssh_alias).unwrap_or_default();
                match tracker.observe(&stats, Instant::now()) {
                    Idle::Stop => {
                        info!(
                            "  ! every GPU idle for {} min; terminating {}",
                            tracker.limit.as_secs() / 60,
                            pod.name
                        );
                        drop(tunnel);
                        s.shutdown(&pod, &mut print_event(&mut last))?;
                        return Ok(());
                    }
                    Idle::Idle(d) if d.as_secs() >= 300 && d.as_secs() % 300 < 60 => {
                        info!("  - GPUs idle for {} min", d.as_secs() / 60);
                    }
                    _ => {}
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    info!(
        "tunnel closed; {} is still running (stop it with `offrig down {} --yes`)",
        spec::pod_name(&s.cfg, p),
        p.name
    );
    Ok(())
}
