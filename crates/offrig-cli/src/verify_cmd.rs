//! `offrig verify`: the verifier's commands. Only `calibrate` so far.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use offrig_core::calibrate_run::{
    self, GoldFile, RunArgs, Settings, Split, Structured, default_out_dir, display_name,
    parse_check_filter, parse_think, render_table,
};
use offrig_core::cost::now_unix;
use offrig_core::ollama::Ollama;
use offrig_core::store::Store;
use offrig_core::trace::{self, Level};

#[derive(Subcommand)]
pub enum VerifyCmd {
    /// Run gold claims through a local Ollama model and score it for the default rule
    ///
    /// Oracle mode: the model sees each claim's own context as evidence, in file order
    /// (--swap-evidence reverses it), one claim per call. Every outcome is appended to
    /// <out>/verdicts.jsonl and saved to the store as untrusted model output; a call
    /// that fails is counted as unusable and never scored. The run is resumable with
    /// --resume, and --report-only rescores a finished or partial run. Local models
    /// on a loopback server only: Ollama Cloud models and remote URLs are refused.
    Calibrate(Box<CalibrateArgs>),
}

#[derive(Args)]
pub struct CalibrateArgs {
    /// Gold files (JSONL), run in the order given
    #[arg(required_unless_present = "report_only")]
    gold: Vec<PathBuf>,
    /// The Ollama model to calibrate (a local model; names containing "cloud" are refused)
    #[arg(long, required_unless_present = "report_only")]
    model: Option<String>,
    /// Ollama base URL (loopback only)
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    url: String,
    /// How much the model thinks before it answers
    #[arg(long, default_value = "on", value_parser = ["off", "on", "low", "medium", "high"])]
    think: String,
    /// Send the reply schema as `format` (auto falls back to plain text if it collapses)
    #[arg(long, default_value = "auto", value_parser = ["auto", "on", "off"])]
    structured: String,
    /// Which half of the gold set to run
    #[arg(long, default_value = "tune", value_parser = ["tune", "heldout", "all"])]
    split: String,
    /// Only claims of this check type
    #[arg(long, default_value = "all", value_parser = ["grounded", "reasoning", "knowledge", "all"])]
    check_type: String,
    /// Run only the first N selected claims
    #[arg(long)]
    limit: Option<usize>,
    /// Show each claim's evidence in reverse order
    #[arg(long)]
    swap_evidence: bool,
    #[arg(long, default_value_t = 0)]
    seed: i64,
    #[arg(long, default_value_t = 0.0)]
    temperature: f64,
    /// Context window (default 16384)
    #[arg(long)]
    num_ctx: Option<u32>,
    /// Reply token limit (default 4096)
    #[arg(long)]
    num_predict: Option<i32>,
    /// Dollars per GPU hour, for the cost per claim (0 for a local card). Boot and
    /// model pull time are not included.
    #[arg(long, default_value_t = 0.0)]
    gpu_cost_hr: f64,
    /// Output directory (default: <project>/.offrig/out/calibrate-<model>-<timestamp>)
    #[arg(long, conflicts_with_all = ["resume", "report_only"])]
    out: Option<PathBuf>,
    /// Continue the run in this directory, repeating the same model, gold and settings
    #[arg(long, conflicts_with = "report_only")]
    resume: Option<PathBuf>,
    /// Score the run in this directory again and call nothing
    #[arg(long)]
    report_only: Option<PathBuf>,
    /// Project directory (default: the current directory)
    #[arg(long)]
    project: Option<PathBuf>,
}

pub fn run(cmd: VerifyCmd) -> Result<()> {
    match cmd {
        VerifyCmd::Calibrate(a) => calibrate(*a),
    }
}

fn say(msg: &str) {
    if trace::enabled(Level::Normal) {
        println!("{}", trace::redact(msg));
    }
}

fn calibrate(a: CalibrateArgs) -> Result<()> {
    if let Some(dir) = &a.report_only {
        let report =
            calibrate_run::report_dir(dir, (a.gpu_cost_hr > 0.0).then_some(a.gpu_cost_hr))?;
        print!("{}", render_table(&report));
        say("metrics.json rewritten");
        return Ok(());
    }
    let model = a.model.clone().context("--model is required")?;
    let mut s = Settings::new(&model, &a.url);
    s.think = parse_think(&a.think)?;
    s.structured = Structured::parse(&a.structured)?;
    s.split = Split::parse(&a.split)?;
    s.check_type = parse_check_filter(&a.check_type)?;
    s.limit = a.limit;
    s.swap_evidence = a.swap_evidence;
    s.seed = a.seed;
    s.temperature = a.temperature;
    s.num_ctx = a.num_ctx.unwrap_or(s.num_ctx);
    s.num_predict = a.num_predict.unwrap_or(s.num_predict);
    s.gpu_cost_hr = a.gpu_cost_hr;
    // Refuse before reading anything: no file is touched for a cloud model.
    calibrate_run::guard_local(&s.model, &s.url)?;
    let gold = a
        .gold
        .iter()
        .map(|p| {
            Ok(GoldFile {
                name: display_name(p),
                // A gold file that cannot be read is the caller's to fix: exit 1.
                text: std::fs::read_to_string(p).map_err(|e| {
                    offrig_core::Error::Refused(format!(
                        "cannot read gold file {}: {e}",
                        display_name(p)
                    ))
                })?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let project = match &a.project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let store = Store::open(&project.join(".offrig").join("offrig.db"))?;
    let (out_dir, resume) = match (&a.resume, &a.out) {
        (Some(r), _) => (r.clone(), true),
        (None, Some(o)) => (o.clone(), false),
        (None, None) => (default_out_dir(&project, &model, now_unix()), false),
    };
    let ollama = Ollama::new(&a.url);
    let args = RunArgs {
        settings: &s,
        gold: &gold,
        store: &store,
        out_dir: &out_dir,
        resume,
    };
    let report = calibrate_run::run(&args, &ollama, &ollama, &mut |m| say(m))?;
    print!("{}", render_table(&report));
    say(&format!(
        "wrote verdicts.jsonl, manifest.json and metrics.json to {}",
        out_dir.display()
    ));
    Ok(())
}
