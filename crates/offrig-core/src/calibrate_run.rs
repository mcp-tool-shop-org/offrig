//! The calibration runner behind `offrig verify calibrate`: run gold claims through a
//! local Ollama model, one claim per call, record every outcome, and score the run with
//! [`crate::calibrate`]. Design: docs/verifier-design.md, section 6 and the Selection
//! protocol.
//!
//! Oracle mode only: the evidence a model sees is the claim's own `context`, in file
//! order (reversed with `swap_evidence`). That measures the verifier alone. End-to-end
//! calibration through retrieval is a separate follow-up.
//!
//! A run lives in one directory: `manifest.json` (what was run, written first),
//! `verdicts.jsonl` (one line per claim, appended as it finishes) and, at the end,
//! `metrics.json`. The directory is the unit of resume and of `--report-only`.
//!
//! Studio rules enforced here: no Ollama Cloud model (the name, or a tag that forwards
//! to a hosted service) and nothing but a loopback server. A failed call is an
//! `unusable` outcome with its error code; it is counted, never scored as a verdict.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::calibrate::{
    self, CheckMetrics, MAX_ABSTAIN, MAX_FALSE_ACCEPT_UPPER, MIN_BALANCED_ACCURACY,
    MIN_UNSUPPORTED, Outcome,
};
use crate::cost::now_unix;
use crate::error::{Error, Result};
use crate::index::sha256_hex;
use crate::ollama::{ChatRequest, ChatResponse, Ollama, Tag, ThinkLevel};
use crate::store::Store;
use crate::verify::{
    ChatBackend, CheckType, Claim, Label, Pins, Timing, Verdict, VerdictKind, VerifyConfig,
    evidence_list, quote_found, verify_one,
};

/// Consecutive transport failures after which the run stops (and stays resumable)
/// instead of recording a whole file of claims against a server that is down.
const MAX_CONSECUTIVE_TRANSPORT: usize = 5;

/// The note on a claim whose second timeout was recorded as its outcome.
const TIMED_OUT_TWICE: &str = "timed out twice; long think or stuck server";

/// The note on a claim whose second server error (5xx) was recorded as its outcome. A
/// server that fails the same claim twice at temperature 0, while it still answers a claim
/// it answered before, is failing on that model's reply (Ollama cancels the task
/// mid-generation), so it counts against the model, as a repeated timeout does. A server
/// that is down or out of memory fails the check too, so nothing is charged to the model.
const SERVER_ERROR_TWICE: &str =
    "server error or dropped reply twice on this claim; the reply, not the wire";

/// What the default rule counts against the model, stated wherever its result shows.
pub const RULE_NOTE: &str = "model-caused unusable outcomes (bad_verdict, truncated, repeated timeout, repeated server error or dropped reply) count as missing answers and fail the rule; a fail can come from those, not only from false accepts";

// ---- Settings

/// Whether to send the reply schema as `format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structured {
    /// Structured; if the first claims fail their schema, fall back to plain text.
    Auto,
    On,
    Off,
}

impl Structured {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(Error::Refused(format!(
                "--structured must be auto, on or off, not {other:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// Which half of the gold set to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    Tune,
    Heldout,
    All,
}

impl Split {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "tune" => Ok(Self::Tune),
            "heldout" => Ok(Self::Heldout),
            "all" => Ok(Self::All),
            other => Err(Error::Refused(format!(
                "--split must be tune, heldout or all, not {other:?}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tune => "tune",
            Self::Heldout => "heldout",
            Self::All => "all",
        }
    }
}

/// Parse a `--think` value.
pub fn parse_think(s: &str) -> Result<ThinkLevel> {
    match s {
        "off" => Ok(ThinkLevel::Off),
        "on" => Ok(ThinkLevel::On),
        "low" => Ok(ThinkLevel::Low),
        "medium" => Ok(ThinkLevel::Medium),
        "high" => Ok(ThinkLevel::High),
        other => Err(Error::Refused(format!(
            "--think must be off, on, low, medium or high, not {other:?}"
        ))),
    }
}

/// Parse a `--check-type` value: `all` is no filter.
pub fn parse_check_filter(s: &str) -> Result<Option<CheckType>> {
    if s == "all" {
        return Ok(None);
    }
    CheckType::parse(s).map(Some).ok_or_else(|| {
        Error::Refused(format!(
            "--check-type must be grounded, reasoning, knowledge or all, not {s:?}"
        ))
    })
}

/// Everything about a run the caller chooses.
#[derive(Debug, Clone)]
pub struct Settings {
    pub model: String,
    pub url: String,
    pub think: ThinkLevel,
    pub structured: Structured,
    pub split: Split,
    pub check_type: Option<CheckType>,
    pub limit: Option<usize>,
    pub swap_evidence: bool,
    pub seed: i64,
    pub temperature: f64,
    pub num_ctx: u32,
    pub num_predict: i32,
    pub gpu_cost_hr: f64,
}

impl Settings {
    pub fn new(model: &str, url: &str) -> Self {
        let d = VerifyConfig::new(model);
        Self {
            model: model.to_string(),
            url: url.to_string(),
            think: d.think,
            structured: Structured::Auto,
            split: Split::Tune,
            check_type: None,
            limit: None,
            swap_evidence: false,
            seed: 0,
            temperature: 0.0,
            num_ctx: d.num_ctx,
            num_predict: d.num_predict,
            gpu_cost_hr: 0.0,
        }
    }

    /// The part of the settings that must not change across a resume.
    fn record(&self) -> Value {
        serde_json::json!({
            "model": self.model,
            "url": self.url,
            "think": self.think.as_str(),
            "structured": self.structured.as_str(),
            "split": self.split.as_str(),
            "check_type": self.check_type.map_or("all", CheckType::as_str),
            "limit": self.limit,
            "swap_evidence": self.swap_evidence,
            "seed": self.seed,
            "temperature": self.temperature,
            "num_ctx": self.num_ctx,
            "num_predict": self.num_predict,
            "quote_rule": crate::verify::QUOTE_RULE,
        })
    }
}

// ---- The studio's local-only rules

/// Refuse a cloud-routed model and any server that is not on this machine.
pub fn guard_local(model: &str, url: &str) -> Result<()> {
    if model.to_lowercase().contains("cloud") {
        return Err(Error::Refused(format!(
            "model {model:?} is an Ollama Cloud model; calibration runs on local models only (studio rule: no Ollama Cloud)"
        )));
    }
    match url_host(url).as_deref() {
        Some("127.0.0.1" | "localhost" | "[::1]") => Ok(()),
        _ => Err(Error::Refused(format!(
            "--url {url:?} is not a loopback address; calibration talks only to 127.0.0.1, localhost or [::1]"
        ))),
    }
}

/// The host of an http(s) URL, lower-cased, brackets kept for IPv6. None for anything
/// else, and for a URL with credentials (`user@host` tricks).
fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let host = if authority.starts_with('[') {
        format!("{}]", authority.split(']').next()?)
    } else {
        authority.split(':').next()?.to_string()
    };
    Some(host.to_lowercase())
}

// ---- Gold files

/// A gold file as read: its name (never its directory) and its text.
#[derive(Debug, Clone)]
pub struct GoldFile {
    pub name: String,
    pub text: String,
}

/// One gold line: the claim plus the strata fields the shared reader does not carry.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldEntry {
    pub claim: Claim,
    pub has_doc_comment: Option<bool>,
    pub self_referential: Option<bool>,
}

/// Parse gold files. Unknown fields are ignored; a bad line names its file and line.
pub fn read_gold(files: &[GoldFile]) -> Result<Vec<GoldEntry>> {
    let mut out = Vec::new();
    for f in files {
        for (i, line) in f.text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let bad = |e: &dyn std::fmt::Display| {
                Error::Refused(format!("{} line {}: {e}", f.name, i + 1))
            };
            let v: Value = serde_json::from_str(line).map_err(|e| bad(&e))?;
            let flag = |k: &str| v.get(k).and_then(Value::as_bool);
            let (doc, selfref) = (flag("has_doc_comment"), flag("self_referential"));
            let claim: Claim = serde_json::from_value(v).map_err(|e| bad(&e))?;
            out.push(GoldEntry {
                claim,
                has_doc_comment: doc,
                self_referential: selfref,
            });
        }
    }
    Ok(out)
}

/// The gold claims a run covers: labelled, in the split and check type asked for, in
/// file order, cut at the limit. Returns them with the number skipped for lacking a
/// label. A repeated id is refused: ids key the resume and the scoring.
pub fn select(entries: Vec<GoldEntry>, s: &Settings) -> Result<(Vec<GoldEntry>, usize)> {
    let mut skipped = 0;
    let mut out: Vec<GoldEntry> = Vec::new();
    let mut seen = HashSet::new();
    for e in entries {
        if e.claim.label.is_none() {
            skipped += 1;
            continue;
        }
        let in_split = match s.split {
            Split::All => true,
            Split::Tune => e.claim.split.as_deref() == Some("tune"),
            Split::Heldout => e.claim.split.as_deref() == Some("heldout"),
        };
        if !in_split || s.check_type.is_some_and(|ct| ct != e.claim.check_type) {
            continue;
        }
        if !seen.insert(e.claim.id.clone()) {
            return Err(Error::Refused(format!(
                "claim id {:?} appears twice in the selected gold claims",
                e.claim.id
            )));
        }
        out.push(e);
    }
    if let Some(n) = s.limit {
        out.truncate(n);
    }
    Ok((out, skipped))
}

// ---- The run directory

/// A selected gold claim as the manifest keeps it: enough to score and to stratify
/// without the gold files, so `--report-only` needs only the run directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selected {
    pub id: String,
    pub check_type: CheckType,
    pub label: Label,
    pub subtle: bool,
    pub has_doc_comment: Option<bool>,
    pub self_referential: Option<bool>,
    pub origin: Option<String>,
}

impl Selected {
    fn from_entry(e: &GoldEntry) -> Self {
        Self {
            id: e.claim.id.clone(),
            check_type: e.claim.check_type,
            label: e.claim.label.expect("selected claims are labelled"),
            subtle: e.claim.subtle == Some(true),
            has_doc_comment: e.has_doc_comment,
            self_referential: e.self_referential,
            origin: e.claim.origin.clone(),
        }
    }

    /// A scoring stand-in: the text is not needed to score.
    fn to_claim(&self) -> Claim {
        Claim {
            id: self.id.clone(),
            check_type: self.check_type,
            claim: String::new(),
            context: vec![],
            evidence_paths: vec![],
            high_stakes: false,
            label: Some(self.label),
            subtle: Some(self.subtle),
            split: None,
            origin: self.origin.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldInfo {
    pub file: String,
    pub sha256: String,
    pub lines: usize,
}

/// `manifest.json`: what was run, written before the first call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub offrig_version: String,
    pub started_at: i64,
    /// Unix seconds of each resume.
    pub resumes: Vec<i64>,
    pub model: String,
    pub model_digest: Option<String>,
    pub ollama_version: String,
    pub settings: Value,
    /// Cost of a GPU hour for the run; 0 for a local card.
    pub gpu_cost_hr: f64,
    pub gold: Vec<GoldInfo>,
    pub skipped_unlabelled: usize,
    pub selected: Vec<Selected>,
}

/// One line of `verdicts.jsonl`: one claim's outcome. `status` is `ok` (a verdict) or
/// `unusable` (the call failed; `error_code` says how).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub claim_id: String,
    pub check_type: CheckType,
    pub gold_label: Label,
    pub subtle: bool,
    pub has_doc_comment: Option<bool>,
    pub self_referential: Option<bool>,
    pub origin: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub model_verdict: Option<VerdictKind>,
    pub final_verdict: Option<VerdictKind>,
    pub reason: Option<String>,
    pub quote_found: Option<bool>,
    pub needs_human: Option<bool>,
    pub reasoning: Option<String>,
    pub evidence_quote: Option<String>,
    /// `file` or `reversed`.
    pub evidence_order: String,
    pub wall_seconds: f64,
    pub timing: Option<Timing>,
    pub pins: Option<Pins>,
}

impl Line {
    fn base(sel: &Selected, order: &str, wall: f64) -> Self {
        Self {
            claim_id: sel.id.clone(),
            check_type: sel.check_type,
            gold_label: sel.label,
            subtle: sel.subtle,
            has_doc_comment: sel.has_doc_comment,
            self_referential: sel.self_referential,
            origin: sel.origin.clone(),
            status: "ok".into(),
            error_code: None,
            error: None,
            model_verdict: None,
            final_verdict: None,
            reason: None,
            quote_found: None,
            needs_human: None,
            reasoning: None,
            evidence_quote: None,
            evidence_order: order.to_string(),
            wall_seconds: wall,
            timing: None,
            pins: None,
        }
    }

    fn from_verdict(sel: &Selected, order: &str, wall: f64, v: &Verdict, found: bool) -> Self {
        let mut l = Self::base(sel, order, wall);
        l.model_verdict = Some(v.model_verdict);
        l.final_verdict = Some(v.verdict);
        l.reason = v.reason.clone();
        l.quote_found = Some(found);
        l.needs_human = Some(v.needs_human);
        l.reasoning = Some(v.reasoning.clone());
        l.evidence_quote = Some(v.evidence_quote.clone());
        l.timing = Some(v.timing);
        l.pins = Some(v.pins.clone());
        l
    }

    fn from_error(sel: &Selected, order: &str, wall: f64, e: &Error) -> Self {
        let mut l = Self::base(sel, order, wall);
        l.status = "unusable".into();
        l.error_code = Some(e.code().to_string());
        l.error = Some(e.to_string());
        l
    }

    fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

fn write_json(path: &Path, v: &impl Serialize) -> Result<()> {
    let text = serde_json::to_string_pretty(v).map_err(|e| Error::decode("a run file", e))?;
    std::fs::write(path, text + "\n").map_err(|e| Error::io("writing a run file", e))
}

fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        Error::Refused(format!(
            "{} has no readable manifest.json: {e}",
            dir.display()
        ))
    })?;
    serde_json::from_str(&text).map_err(|e| Error::decode("manifest.json", e))
}

/// Read `verdicts.jsonl`. A last line that does not parse (a crash mid-write) is
/// dropped from the file so the claim runs again; a bad line elsewhere is an error.
fn read_lines(dir: &Path, repair: bool) -> Result<Vec<Line>> {
    let path = dir.join("verdicts.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(Error::io("reading verdicts.jsonl", e)),
    };
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        match serde_json::from_str::<Line>(row) {
            Ok(l) => out.push(l),
            Err(_) if i + 1 == rows.len() => {
                if repair {
                    let kept: String = rows[..i].iter().map(|r| format!("{r}\n")).collect();
                    std::fs::write(&path, kept)
                        .map_err(|e| Error::io("repairing verdicts.jsonl", e))?;
                }
            }
            Err(e) => {
                return Err(Error::Refused(format!(
                    "verdicts.jsonl line {} is not a valid outcome: {e}",
                    i + 1
                )));
            }
        }
    }
    Ok(out)
}

fn append_line(file: &mut std::fs::File, line: &Line) -> Result<()> {
    let mut text = serde_json::to_string(line).map_err(|e| Error::decode("an outcome", e))?;
    text.push('\n');
    file.write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|e| Error::io("appending to verdicts.jsonl", e))
}

/// Refuse a resume when the run being resumed was made differently.
fn check_resume(m: &Manifest, now: &Manifest) -> Result<()> {
    let differs = |what: &str, was: String, is: String| {
        Err(Error::Refused(format!(
            "cannot resume: {what} differs (run was {was}, now {is})"
        )))
    };
    if let (Some(a), Some(b)) = (m.settings.as_object(), now.settings.as_object()) {
        for (k, was) in a {
            let is = b.get(k).cloned().unwrap_or(Value::Null);
            if *was != is {
                return differs(k, was.to_string(), is.to_string());
            }
        }
    }
    if m.model_digest != now.model_digest {
        return differs(
            "model digest",
            format!("{:?}", m.model_digest),
            format!("{:?}", now.model_digest),
        );
    }
    let shas = |x: &Manifest| x.gold.iter().map(|g| g.sha256.clone()).collect::<Vec<_>>();
    if shas(m) != shas(now) {
        return differs("gold files", shas(m).join(","), shas(now).join(","));
    }
    if m.selected != now.selected {
        return differs(
            "selected claims",
            m.selected.len().to_string(),
            now.selected.len().to_string(),
        );
    }
    Ok(())
}

// ---- The server

/// What the runner asks of the model server besides chat.
pub trait Server {
    fn version(&self) -> Result<String>;
    fn tags(&self) -> Result<Vec<Tag>>;
}

impl Server for Ollama {
    fn version(&self) -> Result<String> {
        Ollama::version(self)
    }
    fn tags(&self) -> Result<Vec<Tag>> {
        Ollama::tags(self)
    }
}

/// The installed tag for `model`: a name without a tag means `:latest`.
fn find_tag<'a>(tags: &'a [Tag], model: &str) -> Option<&'a Tag> {
    let latest = format!("{model}:latest");
    tags.iter()
        .find(|t| t.name == model || (!model.contains(':') && t.name == latest))
}

// ---- Running

/// What `run` needs.
pub struct RunArgs<'a> {
    pub settings: &'a Settings,
    pub gold: &'a [GoldFile],
    pub store: &'a Store,
    pub out_dir: &'a Path,
    pub resume: bool,
    /// Write each claim's thinking text to [`THINKING_FILE`] in the run directory. A
    /// diagnosis aid: not scored, not stored, and not part of the resume identity.
    pub keep_thinking: bool,
}

/// The file `--keep-thinking` writes in the run directory. It holds model output that
/// can echo anything the evidence holds, so it stays out of the store and out of the
/// manifest, metrics and verdict files; copy it anywhere public only after a scan.
pub const THINKING_FILE: &str = "thinking.jsonl";

/// A chat backend that keeps the thinking text of every reply it passes through.
struct ThinkingRecorder<'a> {
    inner: &'a dyn ChatBackend,
    seen: RefCell<Vec<String>>,
}

impl ChatBackend for ThinkingRecorder<'_> {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let r = self.inner.chat(req);
        match &r {
            Ok(resp) => self.seen.borrow_mut().push(resp.thinking.clone()),
            // A reply cut off at the token limit is the one most worth reading.
            Err(Error::Truncated { thinking, .. }) => self.seen.borrow_mut().push(thinking.clone()),
            // A transport or server error carries no reply, so there is nothing to keep.
            Err(_) => {}
        }
        r
    }
}

/// Run (or resume) a calibration and return its report. `progress` gets one line per
/// claim and per notable event.
pub fn run(
    args: &RunArgs<'_>,
    chat: &dyn ChatBackend,
    server: &dyn Server,
    progress: &mut dyn FnMut(&str),
) -> Result<Report> {
    let s = args.settings;
    guard_local(&s.model, &s.url)?;
    let files: Vec<GoldInfo> = args
        .gold
        .iter()
        .map(|f| GoldInfo {
            file: f.name.clone(),
            sha256: sha256_hex(f.text.as_bytes()),
            lines: f.text.lines().filter(|l| !l.trim().is_empty()).count(),
        })
        .collect();
    let (picked, skipped) = select(read_gold(args.gold)?, s)?;
    if picked.is_empty() {
        return Err(Error::Refused(
            "no labelled gold claim matches --split, --check-type and --limit".into(),
        ));
    }
    let ollama_version = server.version()?;
    let tags = server.tags()?;
    let tag = find_tag(&tags, &s.model).ok_or_else(|| {
        Error::Refused(format!(
            "model {:?} is not installed on the server; pull it first",
            s.model
        ))
    })?;
    if !tag.remote_host.is_empty() {
        return Err(Error::Refused(format!(
            "model {:?} forwards to a hosted service; calibration runs on local models only",
            s.model
        )));
    }
    let digest = (!tag.digest.is_empty()).then(|| tag.digest.clone());
    let selected: Vec<Selected> = picked.iter().map(Selected::from_entry).collect();
    let mut manifest = Manifest {
        offrig_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: now_unix(),
        resumes: vec![],
        model: s.model.clone(),
        model_digest: digest.clone(),
        ollama_version,
        settings: s.record(),
        gpu_cost_hr: s.gpu_cost_hr,
        gold: files,
        skipped_unlabelled: skipped,
        selected: selected.clone(),
    };
    let dir = args.out_dir;
    if args.resume {
        let old = read_manifest(dir)?;
        check_resume(&old, &manifest)?;
        manifest = Manifest {
            resumes: [old.resumes.clone(), vec![now_unix()]].concat(),
            gpu_cost_hr: s.gpu_cost_hr,
            ..old
        };
    } else if dir.join("manifest.json").exists() {
        return Err(Error::Refused(format!(
            "{} already holds a run; use --resume to continue it",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io("creating the output directory", e))?;
    write_json(&dir.join("manifest.json"), &manifest)?;

    let prior = read_lines(dir, true)?;
    let done: HashSet<String> = prior.iter().map(|l| l.claim_id.clone()).collect();
    let attempts_path = dir.join("attempts.json");
    let mut attempts = read_attempts(&attempts_path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("verdicts.jsonl"))
        .map_err(|e| Error::io("opening verdicts.jsonl", e))?;

    let mut cfg = VerifyConfig::new(&s.model);
    cfg.model_digest = digest;
    cfg.temperature = s.temperature;
    cfg.seed = s.seed;
    cfg.think = s.think;
    cfg.num_ctx = s.num_ctx;
    cfg.num_predict = s.num_predict;
    // Auto starts structured; a resume carries on in whatever mode the run settled on.
    let last_ok = prior.iter().rev().find_map(|l| l.pins.as_ref());
    let mut settled = last_ok.is_some();
    cfg.structured = match s.structured {
        Structured::Off => false,
        Structured::On => true,
        Structured::Auto => last_ok.is_none_or(|p| p.structured),
    };
    let order = if s.swap_evidence { "reversed" } else { "file" };
    if !done.is_empty() {
        progress(&format!(
            "resuming: {} of {} claims already done",
            done.len(),
            picked.len()
        ));
    }

    let recorder = ThinkingRecorder {
        inner: chat,
        seen: RefCell::new(Vec::new()),
    };
    let mut thinking_file = if args.keep_thinking {
        Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(THINKING_FILE))
                .map_err(|e| Error::io("opening thinking.jsonl", e))?,
        )
    } else {
        None
    };
    let chat: &dyn ChatBackend = if args.keep_thinking { &recorder } else { chat };

    // Claims with a real answer, in run order: the health check for a repeated server error
    // asks the first of them again.
    let ok_ids: HashSet<&str> = prior
        .iter()
        .filter(|l| l.is_ok())
        .map(|l| l.claim_id.as_str())
        .collect();
    let mut answered: Vec<usize> = (0..picked.len())
        .filter(|&j| ok_ids.contains(picked[j].claim.id.as_str()))
        .collect();
    let server_answers = |j: usize, cfg: &VerifyConfig| {
        let mut c = picked[j].claim.clone();
        if s.swap_evidence {
            c.context.reverse();
        }
        verify_one(chat, cfg, &c, &[]).is_ok()
    };

    let mut transport_run = 0usize;
    for (i, entry) in picked.iter().enumerate() {
        if done.contains(&entry.claim.id) {
            continue;
        }
        let sel = &selected[i];
        let mut claim = entry.claim.clone();
        if s.swap_evidence {
            claim.context.reverse();
        }
        let evidence = evidence_list(&claim, &[]);
        let started = Instant::now();
        recorder.seen.borrow_mut().clear();
        let mut result = verify_one(chat, &cfg, &claim, &[]);
        if s.structured == Structured::Auto
            && cfg.structured
            && !settled
            && matches!(result, Err(Error::BadVerdict(_)))
        {
            progress("structured output failed its schema; continuing in plain text");
            cfg.structured = false;
            result = verify_one(chat, &cfg, &claim, &[]);
        }
        let wall = started.elapsed().as_secs_f64();
        if let Some(f) = thinking_file.as_mut() {
            // Every reply to this claim (a schema retry makes two), whatever its outcome,
            // so a truncated or broken reply shows what the model was doing.
            let calls = recorder.seen.borrow_mut().split_off(0);
            if !calls.is_empty() {
                let row = serde_json::json!({
                    "claim_id": sel.id,
                    "untrusted": true,
                    "chars": calls.iter().map(|t| t.chars().count()).sum::<usize>(),
                    "thinking": calls,
                });
                writeln!(f, "{row}").map_err(|e| Error::io("writing thinking.jsonl", e))?;
            }
        }
        let mut transport_note: Option<&'static str> = None;
        let line = match result {
            Ok(v) => {
                settled = true;
                answered.push(i);
                args.store.save_verdict(&v)?;
                let found = quote_found(&v.evidence_quote, &evidence);
                Some(Line::from_verdict(sel, order, wall, &v, found))
            }
            Err(e) if is_transport(&e) => {
                // Not an outcome: the server, not the model, failed. Nothing is
                // recorded, so a resume tries the claim again.
                // A 5xx, or a connection that drops mid-reply: Ollama can abort one
                // generation with a 500 whose body never arrives, which the client sees as
                // a network failure. Both are judged by the same repeat-and-health-check
                // rule; a server that is down fails the check, so nothing is charged.
                let server = !e.is_client_timeout();
                transport_note = Some(if e.is_client_timeout() {
                    "timeout"
                } else if is_server_error(&e) {
                    "server (5xx)"
                } else {
                    "network"
                });
                if e.is_client_timeout() || server {
                    let a = attempts.entry(sel.id.clone()).or_default();
                    let (count, note, code) = if server {
                        // A repeat counts only while the server still answers a claim it
                        // answered before; otherwise the server, not this reply, is failing.
                        let healthy = a.server_errors >= 1
                            && answered.first().is_some_and(|&j| server_answers(j, &cfg));
                        if a.server_errors == 0 || healthy {
                            a.server_errors += 1;
                        } else {
                            progress(&format!(
                                "{}: a second server error, but the server failed its health check too; not recorded",
                                sel.id
                            ));
                        }
                        (a.server_errors, SERVER_ERROR_TWICE, "server_error")
                    } else {
                        a.timeouts += 1;
                        (a.timeouts, TIMED_OUT_TWICE, "timeout")
                    };
                    a.last_error = crate::error::chain(&e);
                    let last = a.last_error.clone();
                    write_json(&attempts_path, &attempts)?;
                    (count >= 2).then(|| {
                        let mut l = Line::from_error(sel, order, wall, &e);
                        l.error_code = Some(code.to_string());
                        l.error = Some(format!("{note} (last: {last})"));
                        l
                    })
                } else {
                    None
                }
            }
            Err(e) => Some(Line::from_error(sel, order, wall, &e)),
        };
        if line.is_some() {
            // A recorded outcome, even a repeated timeout or server error, is not a
            // transport failure: it does not count toward stopping the run.
            transport_note = None;
        }
        let shown = match (&line, transport_note) {
            (Some(l), _) => l.final_verdict.map_or_else(
                || format!("unusable:{}", l.error_code.as_deref().unwrap_or("?")),
                |v| v.as_str().to_string(),
            ),
            (None, n) => format!(
                "{} failure, not recorded; --resume retries it",
                n.unwrap_or("transport")
            ),
        };
        progress(&format!(
            "[{}/{}] {} gold={} -> {} ({:.1}s)",
            i + 1,
            picked.len(),
            sel.id,
            label_str(sel.label),
            shown,
            wall
        ));
        if transport_note.is_some() {
            transport_run += 1;
            if transport_run >= MAX_CONSECUTIVE_TRANSPORT {
                return Err(Error::Ollama(format!(
                    "{transport_run} transport or server (5xx) failures in a row: the server could not be reached or kept failing; stopped (resume with --resume)",
                )));
            }
        } else {
            transport_run = 0;
        }
        if let Some(l) = line {
            append_line(&mut file, &l)?;
        }
    }
    report_dir(dir, None)
}

/// Per-claim timeout and server-error history, kept beside the outcomes. Not part of the
/// manifest. A file written before server errors were counted reads them as 0.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Attempt {
    timeouts: u32,
    #[serde(default)]
    server_errors: u32,
    last_error: String,
}

fn read_attempts(path: &Path) -> Result<BTreeMap<String, Attempt>> {
    match std::fs::read_to_string(path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| Error::decode("attempts.json", e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(Error::io("reading attempts.json", e)),
    }
}

/// A failure of the server or the wire, not of the model: no outcome is recorded the
/// first time. A repeated timeout or server error on one claim is recorded (see above).
fn is_transport(e: &Error) -> bool {
    matches!(e, Error::Http { .. }) || is_server_error(e)
}

/// An HTTP 5xx answer from the model server.
fn is_server_error(e: &Error) -> bool {
    matches!(e, Error::Ollama(m) if m
        .split_once(": http ")
        .is_some_and(|(_, rest)| rest.starts_with('5')))
}

fn label_str(l: Label) -> &'static str {
    match l {
        Label::Supported => "supported",
        Label::Unsupported => "unsupported",
        Label::CannotTell => "cannot_tell",
    }
}

// ---- Scoring

/// `--report-only`: score a run directory from its manifest and outcomes.
/// `gpu_cost_hr` overrides the rate the run recorded.
pub fn report_dir(dir: &Path, gpu_cost_hr: Option<f64>) -> Result<Report> {
    let manifest = read_manifest(dir)?;
    let lines = read_lines(dir, false)?;
    let report = build_report(
        &manifest,
        &lines,
        gpu_cost_hr.unwrap_or(manifest.gpu_cost_hr),
    );
    write_json(&dir.join("metrics.json"), &report)?;
    Ok(report)
}

/// One stratum of the gold set, scored on its own.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stratum {
    pub name: String,
    pub rows: Vec<CheckMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cost {
    pub gpu_cost_hr: f64,
    pub wall_seconds: f64,
    pub claims_attempted: usize,
    pub total_usd: f64,
    pub per_claim_usd: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Criterion {
    pub name: String,
    pub value: String,
    pub threshold: String,
    pub pass: bool,
}

/// The default rule for one check type, criterion by criterion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleResult {
    pub check_type: CheckType,
    /// False for knowledge: reported, not part of the rule.
    pub counts: bool,
    /// Empty when the check type was not run.
    pub criteria: Vec<Criterion>,
    pub pass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub model: String,
    pub model_digest: Option<String>,
    pub ollama_version: String,
    pub settings: Value,
    pub claims_selected: usize,
    pub claims_attempted: usize,
    /// Per check type: calls that failed. Counted, never scored.
    pub unusable: BTreeMap<String, usize>,
    pub rows: Vec<CheckMetrics>,
    pub strata: Vec<Stratum>,
    pub cost: Cost,
    pub rule: Vec<RuleResult>,
    /// Both grounded and reasoning pass, and no selected claim is waiting.
    pub passes_default: bool,
    /// `complete`, or `incomplete` while selected claims have no recorded outcome.
    pub status: String,
    /// Selected claims with no recorded outcome (never tried, or only transport-failed).
    pub pending: usize,
    /// What to do about pending claims.
    pub hint: Option<String>,
    /// `pass`, `fail`, or `incomplete` (neither pass nor fail).
    pub default_result: String,
    /// What counts against the model in the default rule.
    pub rule_note: String,
}

fn build_report(m: &Manifest, lines: &[Line], gpu_cost_hr: f64) -> Report {
    let gold: Vec<Claim> = m.selected.iter().map(Selected::to_claim).collect();
    let outcomes: Vec<Outcome> = lines
        .iter()
        .filter(|l| l.is_ok())
        .filter_map(|l| {
            l.final_verdict.map(|v| Outcome {
                claim_id: l.claim_id.clone(),
                verdict: v,
            })
        })
        .collect();
    let rows = calibrate::metrics(&gold, &outcomes);
    let mut unusable: BTreeMap<String, usize> = BTreeMap::new();
    for l in lines.iter().filter(|l| !l.is_ok()) {
        *unusable
            .entry(l.check_type.as_str().to_string())
            .or_default() += 1;
    }

    // Strata: each value of a field scores the claims that carry it.
    let mut groups: BTreeMap<String, Vec<Claim>> = BTreeMap::new();
    for (sel, claim) in m.selected.iter().zip(&gold) {
        let mut names = vec![];
        if let Some(b) = sel.has_doc_comment {
            names.push(format!("has_doc_comment={b}"));
        }
        if let Some(b) = sel.self_referential {
            names.push(format!("self_referential={b}"));
        }
        if let Some(o) = &sel.origin {
            names.push(format!("origin={o}"));
        }
        for n in names {
            groups.entry(n).or_default().push(claim.clone());
        }
    }
    let strata = groups
        .into_iter()
        .map(|(name, g)| Stratum {
            name,
            rows: calibrate::metrics(&g, &outcomes),
        })
        .collect();

    let wall: f64 = lines.iter().map(|l| l.wall_seconds).sum();
    let total = gpu_cost_hr * wall / 3600.0;
    let cost = Cost {
        gpu_cost_hr,
        wall_seconds: wall,
        claims_attempted: lines.len(),
        total_usd: total,
        per_claim_usd: (!lines.is_empty()).then(|| total / lines.len() as f64),
        note: "gpu_cost_hr x wall seconds / 3600; boot and model pull time are not included and are the caller's to add".into(),
    };
    let rule = rule_report(&rows);
    let recorded: HashSet<&str> = lines.iter().map(|l| l.claim_id.as_str()).collect();
    let pending = m
        .selected
        .iter()
        .filter(|s| !recorded.contains(s.id.as_str()))
        .count();
    let passes = pending == 0 && calibrate::passes_default(&rows);
    let default_result = if pending > 0 {
        "incomplete"
    } else if passes {
        "pass"
    } else {
        "fail"
    };
    Report {
        status: if pending > 0 {
            "incomplete"
        } else {
            "complete"
        }
        .into(),
        pending,
        hint: (pending > 0).then(|| "rerun with --resume <dir>".to_string()),
        default_result: default_result.into(),
        rule_note: RULE_NOTE.into(),
        model: m.model.clone(),
        model_digest: m.model_digest.clone(),
        ollama_version: m.ollama_version.clone(),
        settings: m.settings.clone(),
        claims_selected: m.selected.len(),
        claims_attempted: lines.len(),
        unusable,
        passes_default: passes,
        rows,
        strata,
        cost,
        rule,
    }
}

/// The default rule, one criterion at a time, for grounded and reasoning (which count)
/// and knowledge (reported only).
pub fn rule_report(rows: &[CheckMetrics]) -> Vec<RuleResult> {
    [
        (CheckType::Grounded, true),
        (CheckType::Reasoning, true),
        (CheckType::Knowledge, false),
    ]
    .into_iter()
    .map(|(ct, counts)| {
        let Some(r) = rows.iter().find(|r| r.check_type == ct) else {
            return RuleResult {
                check_type: ct,
                counts,
                criteria: vec![],
                pass: false,
            };
        };
        let pct = |x: Option<f64>| x.map_or("n/a".to_string(), |v| format!("{v:.4}"));
        let criteria = vec![
            Criterion {
                name: "every claim answered".into(),
                value: format!("missing {}", r.missing),
                threshold: "0".into(),
                pass: r.missing == 0,
            },
            Criterion {
                name: "gold unsupported claims".into(),
                value: r.unsupported_n.to_string(),
                threshold: format!(">= {MIN_UNSUPPORTED}"),
                pass: r.unsupported_n >= MIN_UNSUPPORTED,
            },
            Criterion {
                name: "false-accept upper bound (primary)".into(),
                value: format!("{:.4}", r.false_accept.high),
                threshold: format!("< {MAX_FALSE_ACCEPT_UPPER}"),
                pass: r.false_accept.high < MAX_FALSE_ACCEPT_UPPER,
            },
            Criterion {
                name: "abstain rate".into(),
                value: pct(r.abstain.rate),
                threshold: format!("<= {MAX_ABSTAIN}"),
                pass: r.abstain.rate.is_some_and(|a| a <= MAX_ABSTAIN),
            },
            Criterion {
                name: "balanced accuracy on decided".into(),
                value: pct(r.decided_balanced_accuracy),
                threshold: format!(">= {MIN_BALANCED_ACCURACY}"),
                pass: r
                    .decided_balanced_accuracy
                    .is_some_and(|b| b >= MIN_BALANCED_ACCURACY),
            },
        ];
        let pass = criteria.iter().all(|c| c.pass);
        RuleResult {
            check_type: ct,
            counts,
            criteria,
            pass,
        }
    })
    .collect()
}

// ---- Output

fn fmt_rate(r: &calibrate::Rate) -> String {
    match r.rate {
        Some(v) => format!(
            "{}/{} {:.1}% [{:.1}-{:.1}]",
            r.hits,
            r.of,
            v * 100.0,
            r.low * 100.0,
            r.high * 100.0
        ),
        None => "n/a".to_string(),
    }
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map_or("n/a".to_string(), |v| format!("{v:.3}"))
}

fn table_row(r: &CheckMetrics) -> String {
    format!(
        "{:<10} n={} sup={} unsup={} ct={} missing={}\n  false-accept (unsup+ct): {}\n  false-accept (unsup only): {}\n  false-accept (subtle): {}\n  gold cannot_tell: accepted {}, said cannot_tell {}, said unsupported {}\n  abstain: {}   balanced accuracy (decided): {}",
        r.check_type.as_str(),
        r.n,
        r.supported_n,
        r.unsupported_n,
        r.cannot_tell_n,
        r.missing,
        fmt_rate(&r.false_accept),
        fmt_rate(&r.false_accept_unsupported_only),
        fmt_rate(&r.subtle_false_accept),
        fmt_rate(&r.cannot_tell_gold.false_accept),
        fmt_rate(&r.cannot_tell_gold.said_cannot_tell),
        r.cannot_tell_gold.said_unsupported,
        fmt_rate(&r.abstain),
        fmt_opt(r.decided_balanced_accuracy),
    )
}

/// The compact table printed at the end of a run.
pub fn render_table(r: &Report) -> String {
    let mut out = format!(
        "model {} ({} claims selected, {} attempted)\n",
        r.model, r.claims_selected, r.claims_attempted
    );
    if !r.unusable.is_empty() {
        let u: Vec<String> = r.unusable.iter().map(|(k, n)| format!("{k} {n}")).collect();
        out.push_str(&format!("unusable (not scored): {}\n", u.join(", ")));
    }
    for row in &r.rows {
        out.push_str(&table_row(row));
        out.push('\n');
    }
    if !r.strata.is_empty() {
        out.push_str("strata:\n");
        for s in &r.strata {
            for row in &s.rows {
                out.push_str(&format!(
                    "  {:<28} {:<10} n={:<4} FA {}  subtle FA {}  abstain {}  BA {}\n",
                    s.name,
                    row.check_type.as_str(),
                    row.n,
                    fmt_rate(&row.false_accept),
                    fmt_rate(&row.subtle_false_accept),
                    fmt_rate(&row.abstain),
                    fmt_opt(row.decided_balanced_accuracy),
                ));
            }
        }
    }
    let c = &r.cost;
    out.push_str(&format!(
        "cost: ${:.4} over {:.0}s wall at ${}/hr = {} per claim (boot and pull time not included)\n",
        c.total_usd,
        c.wall_seconds,
        c.gpu_cost_hr,
        c.per_claim_usd
            .map_or("n/a".to_string(), |v| format!("${v:.5}")),
    ));
    if r.pending > 0 {
        out.push_str(&format!(
            "status: incomplete, {} claims pending (no recorded outcome); rerun with --resume <dir>\n",
            r.pending
        ));
    }
    out.push_str("default rule (grounded and reasoning; knowledge reported only):\n");
    out.push_str(&format!("  note: {}\n", r.rule_note));
    for rule in &r.rule {
        let tag = if rule.counts { "" } else { " (reported only)" };
        if rule.criteria.is_empty() {
            out.push_str(&format!("  {}{tag}: not run\n", rule.check_type.as_str()));
            continue;
        }
        out.push_str(&format!(
            "  {}{tag}: {}\n",
            rule.check_type.as_str(),
            if rule.pass { "PASS" } else { "FAIL" }
        ));
        for k in &rule.criteria {
            out.push_str(&format!(
                "    {} {}: {} (need {})\n",
                if k.pass { "ok  " } else { "FAIL" },
                k.name,
                k.value,
                k.threshold
            ));
        }
    }
    out.push_str(&format!(
        "default rule overall: {}\n",
        match r.default_result.as_str() {
            "pass" => "PASS",
            "incomplete" => "INCOMPLETE (neither pass nor fail)",
            _ => "FAIL",
        }
    ));
    out
}

// ---- Output directory naming

/// `calibrate-<model>-<UTC timestamp>` under `<project>/.offrig/out`.
pub fn default_out_dir(project: &Path, model: &str, unix: i64) -> std::path::PathBuf {
    let safe: String = model
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    project
        .join(".offrig")
        .join("out")
        .join(format!("calibrate-{safe}-{}", utc_stamp(unix)))
}

/// `20261008T153000Z`, from unix seconds (civil-from-days, Hinnant).
fn utc_stamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Gold file names the report may carry: the final path component only.
pub fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "gold.jsonl".to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama::{ChatRequest, ChatResponse};
    use serde_json::json;
    use std::cell::RefCell;

    const GOLD: &str = include_str!("../tests/fixtures/calibrate-gold.jsonl");

    fn gold() -> Vec<GoldFile> {
        vec![GoldFile {
            name: "calibrate-gold.jsonl".into(),
            text: GOLD.into(),
        }]
    }

    fn reply(verdict: &str, quote: &str) -> String {
        json!({"reasoning": "r", "verdict": verdict, "evidence_quote": quote, "evidence_source": "ctx1"})
            .to_string()
    }

    fn resp(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            eval_count: Some(5),
            prompt_eval_count: Some(50),
            total_duration: Some(1000),
            ..Default::default()
        }
    }

    type Answer = Box<dyn Fn(&str, &ChatRequest) -> Result<ChatResponse>>;

    /// A model that answers from a function of the request and keeps every request.
    struct Fake {
        seen: RefCell<Vec<ChatRequest>>,
        answer: Answer,
    }

    impl Fake {
        fn new(answer: impl Fn(&str, &ChatRequest) -> Result<ChatResponse> + 'static) -> Self {
            Self {
                seen: RefCell::new(vec![]),
                answer: Box::new(answer),
            }
        }
        fn calls(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    fn claim_id(req: &ChatRequest) -> String {
        let user = &req.messages[1].content;
        let at = user.find("CLAIM (id ").expect("claim header") + "CLAIM (id ".len();
        user[at..].split(')').next().expect("id").to_string()
    }

    impl ChatBackend for Fake {
        fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
            self.seen.borrow_mut().push(req.clone());
            (self.answer)(&claim_id(req), req)
        }
    }

    const QUOTE: &str = "The upload limit is 25 MB per file.";

    /// The scripted model of the fixture: accepts g2 (a near-miss), is right on the
    /// rest, and quotes badly on r1.
    fn scripted() -> Fake {
        Fake::new(|id, _| {
            Ok(resp(&match id {
                "g1" | "g2" | "g5" => reply("supported", QUOTE),
                "g3" => reply("unsupported", ""),
                "g4" => reply("cannot_tell", ""),
                "r1" => reply("supported", "this text is not in the diff at all"),
                "r2" => reply("unsupported", "+ for attempt in 0..3 { try_send() }"),
                _ => reply("unsupported", ""),
            }))
        })
    }

    struct Srv {
        digest: String,
        remote: String,
        installed: bool,
    }

    impl Srv {
        fn new() -> Self {
            Self {
                digest: "sha256:abc".into(),
                remote: String::new(),
                installed: true,
            }
        }
    }

    impl Server for Srv {
        fn version(&self) -> Result<String> {
            Ok("0.35.0".into())
        }
        fn tags(&self) -> Result<Vec<Tag>> {
            Ok(if self.installed {
                vec![
                    Tag {
                        name: "other:1b".into(),
                        ..Default::default()
                    },
                    Tag {
                        name: "judge:latest".into(),
                        digest: self.digest.clone(),
                        remote_host: self.remote.clone(),
                        ..Default::default()
                    },
                ]
            } else {
                vec![]
            })
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("offrig-calrun-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn settings() -> Settings {
        Settings::new("judge", "http://127.0.0.1:11434")
    }

    fn go_with(
        s: &Settings,
        files: &[GoldFile],
        dir: &Path,
        resume: bool,
        chat: &Fake,
        srv: &Srv,
    ) -> Result<Report> {
        let store = Store::open_in_memory().expect("store");
        go_store(s, files, dir, resume, chat, srv, &store)
    }

    fn go_store(
        s: &Settings,
        files: &[GoldFile],
        dir: &Path,
        resume: bool,
        chat: &Fake,
        srv: &Srv,
        store: &Store,
    ) -> Result<Report> {
        let args = RunArgs {
            settings: s,
            gold: files,
            store,
            out_dir: dir,
            resume,
            keep_thinking: false,
        };
        run(&args, chat, srv, &mut |_| {})
    }

    #[test]
    fn keep_thinking_writes_each_claims_thinking_beside_the_run_and_nowhere_else() {
        let dir = tmp("thinking");
        let chat = Fake::new(|id, _| {
            Ok(ChatResponse {
                thinking: format!("working on {id}"),
                ..resp(&reply("unsupported", ""))
            })
        });
        let store = Store::open_in_memory().expect("store");
        let s = settings();
        let files = gold();
        let args = RunArgs {
            settings: &s,
            gold: &files,
            store: &store,
            out_dir: &dir,
            resume: false,
            keep_thinking: true,
        };
        let rep = run(&args, &chat, &Srv::new(), &mut |_| {}).expect("run");
        assert_eq!(rep.status.as_str(), "complete");
        let text = std::fs::read_to_string(dir.join(THINKING_FILE)).expect("thinking file");
        let rows: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("row"))
            .collect();
        assert_eq!(rows.len(), 7, "one row per claim");
        let g2 = rows.iter().find(|r| r["claim_id"] == "g2").expect("g2");
        assert_eq!(g2["thinking"], json!(["working on g2"]));
        assert_eq!(
            (g2["untrusted"].as_bool(), g2["chars"].as_u64()),
            (Some(true), Some(13))
        );
        // Nothing of it reaches the verdicts, the manifest or the metrics.
        for f in ["verdicts.jsonl", "manifest.json", "metrics.json"] {
            let t = std::fs::read_to_string(dir.join(f)).expect("file");
            assert!(!t.contains("working on"), "{f}");
            assert!(!t.contains("keep_thinking"), "{f}");
        }
        // Not part of the resume identity: a resume without it is accepted.
        let path = dir.join("verdicts.jsonl");
        let first: Vec<String> = std::fs::read_to_string(&path)
            .expect("read")
            .lines()
            .take(6)
            .map(str::to_string)
            .collect();
        std::fs::write(&path, first.join("\n") + "\n").expect("write");
        let rep = go_store(&s, &files, &dir, true, &chat, &Srv::new(), &store).expect("resume");
        assert_eq!(rep.status.as_str(), "complete");
        cleanup(&[&dir]);
    }

    #[test]
    fn keep_thinking_keeps_the_thinking_of_a_truncated_reply() {
        let dir = tmp("thinking-cut");
        let chat = Fake::new(|id, _| {
            if id == "g1" {
                return Err(Error::Truncated {
                    message: "stopped at its token limit after 4096 tokens".into(),
                    thinking: "still weighing the second passage".into(),
                });
            }
            Ok(resp(&reply("unsupported", "")))
        });
        let store = Store::open_in_memory().expect("store");
        let s = settings();
        let files = gold();
        let args = RunArgs {
            settings: &s,
            gold: &files,
            store: &store,
            out_dir: &dir,
            resume: false,
            keep_thinking: true,
        };
        run(&args, &chat, &Srv::new(), &mut |_| {}).expect("run");
        let lines = read_lines(&dir, false).expect("lines");
        let g1 = lines.iter().find(|l| l.claim_id == "g1").expect("g1");
        assert_eq!(
            (g1.status.as_str(), g1.error_code.as_deref()),
            ("unusable", Some("truncated"))
        );
        assert!(!g1.error.as_deref().unwrap_or_default().contains("weighing"));
        let text = std::fs::read_to_string(dir.join(THINKING_FILE)).expect("thinking file");
        let row: Value = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).expect("row"))
            .find(|r| r["claim_id"] == "g1")
            .expect("a row for the truncated claim");
        assert_eq!(
            row["thinking"],
            json!(["still weighing the second passage"])
        );
        cleanup(&[&dir]);
    }

    #[test]
    fn without_keep_thinking_no_thinking_file_is_written() {
        let dir = tmp("no-thinking");
        let chat = Fake::new(|_, _| {
            Ok(ChatResponse {
                thinking: "hidden".into(),
                ..resp(&reply("unsupported", ""))
            })
        });
        go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert!(!dir.join(THINKING_FILE).exists());
        cleanup(&[&dir]);
    }

    fn go(s: &Settings, dir: &Path, resume: bool, chat: &Fake, srv: &Srv) -> Result<Report> {
        go_with(s, &gold(), dir, resume, chat, srv)
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    fn net_error() -> Error {
        let e = ureq::get("http://127.0.0.1:1/")
            .call()
            .expect_err("closed port");
        Error::http("chat", e)
    }

    fn cleanup(dirs: &[&Path]) {
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn cloud_models_and_remote_servers_are_refused() {
        for m in ["gpt-oss:120b-cloud", "kimi:Cloud", "CLOUD-x"] {
            let e = guard_local(m, "http://127.0.0.1:11434").expect_err(m);
            assert!(e.to_string().contains("Ollama Cloud"), "{e}");
            assert_eq!(e.code(), "refused");
        }
        for u in [
            "http://ollama.com",
            "https://example.org:11434",
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
            "http://user@127.0.0.1",
            "http://127.0.0.1@evil.com",
            "ftp://127.0.0.1",
            "127.0.0.1:11434",
            "http://10.0.0.5:11434",
        ] {
            let e = guard_local("judge", u).expect_err(u);
            assert!(e.to_string().contains("loopback"), "{u}: {e}");
        }
        for u in [
            "http://127.0.0.1:11434",
            "http://localhost:11434/",
            "http://LOCALHOST",
            "https://127.0.0.1/x",
            "http://[::1]:11434",
        ] {
            guard_local("judge", u).unwrap_or_else(|e| panic!("{u}: {e}"));
        }
        // And the run itself refuses before it asks the server anything.
        let chat = scripted();
        let mut s = settings();
        s.model = "x:cloud".into();
        let dir = tmp("cloud");
        let e = go(&s, &dir, false, &chat, &Srv::new()).expect_err("cloud");
        assert!(e.to_string().contains("Ollama Cloud"));
        let mut s = settings();
        s.url = "http://example.org".into();
        let e = go(&s, &dir, false, &chat, &Srv::new()).expect_err("remote");
        assert!(e.to_string().contains("loopback"));
        assert_eq!(chat.calls(), 0);
        assert!(!dir.exists());
    }

    #[test]
    fn a_run_scores_the_fixture_and_writes_its_files() {
        let dir = tmp("score");
        let store = Store::open_in_memory().expect("store");
        let chat = scripted();
        let mut s = settings();
        s.gpu_cost_hr = 3.6;
        let rep = go_store(&s, &gold(), &dir, false, &chat, &Srv::new(), &store).expect("run");
        // tune split: g1 g2 g3 g4 r1 r2 k1; g5 is heldout, u1 has no label.
        assert_eq!((rep.claims_selected, rep.claims_attempted), (7, 7));
        assert_eq!(chat.calls(), 7);
        assert!(rep.unusable.is_empty());
        let stored = store.recent_verdicts(20).expect("rows");
        assert_eq!(stored.len(), 7);
        assert!(stored.iter().all(|r| r.verdict.untrusted));

        let g = &rep.rows[0];
        assert_eq!(g.check_type, CheckType::Grounded);
        // g1 supported/right, g2 unsupported/accepted, g3 right, g4 cannot_tell/right.
        assert_eq!((g.supported_n, g.unsupported_n, g.cannot_tell_n), (1, 2, 1));
        assert_eq!((g.false_accept.hits, g.false_accept.of), (1, 3));
        let u = &g.false_accept_unsupported_only;
        assert_eq!((u.hits, u.of), (1, 2));
        assert_eq!(
            (g.subtle_false_accept.hits, g.subtle_false_accept.of),
            (1, 1)
        );
        assert_eq!((g.abstain.hits, g.abstain.of), (0, 3));
        assert_eq!(g.cannot_tell_gold.said_cannot_tell.hits, 1);
        // Decided supported: g1 right. Decided not-supported: g2 wrong, g3 right.
        close(g.decided_balanced_accuracy.expect("ba"), 0.75);
        // r1's quote is not in the diff: the verdict stands down to cannot_tell.
        let r = &rep.rows[1];
        assert_eq!(r.check_type, CheckType::Reasoning);
        assert_eq!((r.abstain.hits, r.abstain.of), (1, 2));
        assert_eq!(r.decided_balanced_accuracy, None);
        let k = &rep.rows[2];
        assert_eq!(k.check_type, CheckType::Knowledge);
        assert_eq!(k.false_accept.hits, 0);
        assert!(!rep.passes_default);

        // Strata: by doc comment, self reference and origin.
        let names: Vec<&str> = rep.strata.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "has_doc_comment=false",
                "has_doc_comment=true",
                "origin=docs-2026",
                "origin=natural",
                "origin=prs-2026",
                "self_referential=false",
                "self_referential=true"
            ]
        );
        let docs = &rep.strata[2];
        assert_eq!(docs.rows.len(), 1);
        assert_eq!(docs.rows[0].false_accept.hits, 1);
        let with_doc = &rep.strata[1].rows[0];
        assert_eq!(with_doc.check_type, CheckType::Reasoning);
        assert_eq!(with_doc.abstain.hits, 1);

        // metrics.json is the report; verdicts.jsonl has a line per claim.
        let text = std::fs::read_to_string(dir.join("metrics.json")).expect("metrics");
        let m: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(m["rows"][0]["false_accept"]["hits"], 1);
        assert_eq!(m["rows"][0]["false_accept"]["of"], 3);
        assert_eq!(m["rows"][0]["false_accept_unsupported_only"]["of"], 2);
        assert_eq!(m["rows"][0]["cannot_tell_gold"]["n"], 1);
        assert_eq!(m["strata"].as_array().map(Vec::len), Some(7));
        assert_eq!(m["rule"].as_array().map(Vec::len), Some(3));
        let lines = read_lines(&dir, false).expect("lines");
        assert_eq!(lines.len(), 7);
        let find = |id: &str| lines.iter().find(|l| l.claim_id == id).expect("line");
        let r1 = find("r1");
        assert_eq!(r1.model_verdict, Some(VerdictKind::Supported));
        assert_eq!(r1.final_verdict, Some(VerdictKind::CannotTell));
        assert_eq!(r1.reason.as_deref(), Some(crate::verify::QUOTE_NOT_FOUND));
        assert_eq!(r1.quote_found, Some(false));
        let g1 = find("g1");
        assert_eq!(g1.quote_found, Some(true));
        let pins = g1.pins.as_ref().expect("pins");
        assert_eq!(pins.model_digest.as_deref(), Some("sha256:abc"));
        assert_eq!(g1.evidence_order, "file");
        assert_eq!(find("k1").quote_found, Some(false));

        // The manifest names the model, digest, server, settings and gold hashes.
        let man = read_manifest(&dir).expect("manifest");
        assert_eq!(man.model_digest.as_deref(), Some("sha256:abc"));
        assert_eq!(man.ollama_version, "0.35.0");
        assert_eq!(man.gold[0].file, "calibrate-gold.jsonl");
        assert_eq!(man.gold[0].sha256, sha256_hex(GOLD.as_bytes()));
        assert_eq!(man.skipped_unlabelled, 1);
        assert_eq!(man.settings["think"], "on");

        // Cost: 3.6 $/hr is a thousandth of a dollar a second.
        close(rep.cost.total_usd, 0.001 * rep.cost.wall_seconds);
        assert!(rep.cost.per_claim_usd.is_some());
        // --report-only reproduces the report, with a different rate if asked.
        let again = report_dir(&dir, Some(0.0)).expect("report");
        assert_eq!(again.rows, rep.rows);
        assert_eq!(again.cost.total_usd, 0.0);
        // The table says the things a person needs.
        let t = render_table(&rep);
        for want in [
            "false-accept (unsup+ct): 1/3",
            "false-accept (unsup only): 1/2",
            "origin=docs-2026",
            "default rule overall: FAIL",
            "knowledge (reported only)",
            "boot and pull time not included",
        ] {
            assert!(t.contains(want), "{want} missing from:\n{t}");
        }
        cleanup(&[&dir]);
    }

    #[test]
    fn swap_evidence_reverses_the_order_the_model_sees() {
        let order = |swap: bool| {
            let dir = tmp(if swap { "swap" } else { "noswap" });
            let chat = scripted();
            let mut s = settings();
            s.swap_evidence = swap;
            s.limit = Some(1);
            go(&s, &dir, false, &chat, &Srv::new()).expect("run");
            let user = chat.seen.borrow()[0].messages[1].content.clone();
            let line = read_lines(&dir, false).expect("lines").remove(0);
            cleanup(&[&dir]);
            let a = user.find("source: docs/a.md").expect("a");
            let b = user.find("source: src/b.rs").expect("b");
            (
                a < b,
                user.contains("[id: ctx1 | source: src/b.rs]"),
                line.evidence_order,
            )
        };
        assert_eq!(order(false), (true, false, "file".to_string()));
        assert_eq!(order(true), (false, true, "reversed".to_string()));
    }

    #[test]
    fn failed_calls_are_unusable_outcomes_and_never_verdicts() {
        let dir = tmp("unusable");
        let store = Store::open_in_memory().expect("store");
        let chat = Fake::new(|id, _| match id {
            "g1" => Err(Error::truncated("stopped at its token limit")),
            "g2" => Ok(resp("I think it is fine")),
            "g3" => Err(Error::Ollama("chat: model failed to load".into())),
            "g4" => Ok(resp(&reply("cannot_tell", ""))),
            _ => Ok(resp(&reply("unsupported", ""))),
        });
        let rep = go_store(
            &settings(),
            &gold(),
            &dir,
            false,
            &chat,
            &Srv::new(),
            &store,
        )
        .expect("run continues");
        assert_eq!(rep.claims_attempted, 7);
        assert_eq!(rep.unusable.get("grounded"), Some(&3));
        // Only four verdicts reached the store; the three failures did not.
        assert_eq!(store.recent_verdicts(20).expect("rows").len(), 4);
        let lines = read_lines(&dir, false).expect("lines");
        let code = |id: &str| {
            let l = lines.iter().find(|l| l.claim_id == id).expect("line");
            (l.status.clone(), l.error_code.clone(), l.final_verdict)
        };
        let unusable = |c: &str| ("unusable".to_string(), Some(c.to_string()), None);
        assert_eq!(code("g1"), unusable("truncated"));
        assert_eq!(code("g2"), unusable("bad_verdict"));
        assert_eq!(code("g3"), unusable("model_server"));
        // g1, g2 and g3 are missing from the scoring, so the rule cannot pass.
        let g = &rep.rows[0];
        assert_eq!((g.n, g.missing), (1, 3));
        assert!(!g.passes_default_rule);
        assert!(render_table(&rep).contains("unusable (not scored): grounded 3"));
        // g2 failed its schema twice before any claim had succeeded, so auto mode
        // dropped to plain text and gave it two more tries: 4 calls for g2, 1 each
        // for the other six.
        assert_eq!(chat.calls(), 4 + 6);
        cleanup(&[&dir]);
    }

    #[test]
    fn resume_skips_done_claims_and_refuses_changed_settings() {
        let dir = tmp("resume");
        let store = Store::open_in_memory().expect("store");
        let run1 = |s: &Settings, chat: &Fake, srv: &Srv, resume: bool| {
            go_store(s, &gold(), &dir, resume, chat, srv, &store)
        };
        run1(&settings(), &scripted(), &Srv::new(), false).expect("first");
        // Lose the last two outcomes, and keep half of a line as a crash would.
        let path = dir.join("verdicts.jsonl");
        let text = std::fs::read_to_string(&path).expect("read");
        let mut keep: Vec<&str> = text.lines().collect();
        let torn = keep[5][..20].to_string();
        keep.truncate(5);
        std::fs::write(&path, format!("{}\n{torn}", keep.join("\n"))).expect("write");

        let chat = scripted();
        let rep = run1(&settings(), &chat, &Srv::new(), true).expect("resume");
        assert_eq!(chat.calls(), 2, "only the two lost claims run again");
        assert_eq!(rep.claims_attempted, 7);
        assert_eq!(read_lines(&dir, false).expect("lines").len(), 7);
        assert_eq!(read_manifest(&dir).expect("m").resumes.len(), 1);

        // A resume with nothing left to do makes no call.
        let chat = scripted();
        run1(&settings(), &chat, &Srv::new(), true).expect("noop");
        assert_eq!(chat.calls(), 0);

        // Anything that changes what the run means is refused.
        let mut changed: Vec<(Settings, Srv, &str)> = vec![];
        let tweak = |f: &dyn Fn(&mut Settings)| {
            let mut s = settings();
            f(&mut s);
            s
        };
        changed.push((tweak(&|s| s.seed = 7), Srv::new(), "seed"));
        changed.push((
            tweak(&|s| s.model = "judge:latest".into()),
            Srv::new(),
            "model",
        ));
        changed.push((tweak(&|s| s.think = ThinkLevel::Off), Srv::new(), "think"));
        changed.push((
            tweak(&|s| s.swap_evidence = true),
            Srv::new(),
            "swap_evidence",
        ));
        changed.push((
            tweak(&|s| s.url = "http://localhost:11434".into()),
            Srv::new(),
            "url",
        ));
        let mut srv = Srv::new();
        srv.digest = "sha256:new".into();
        changed.push((settings(), srv, "digest"));
        for (s, srv, what) in changed {
            let chat = scripted();
            let e = run1(&s, &chat, &srv, true).expect_err(what);
            let msg = e.to_string();
            assert!(
                msg.contains("cannot resume") && msg.contains(what),
                "{what}: {msg}"
            );
            assert_eq!(chat.calls(), 0);
        }
        // Different gold is refused too, and a fresh run will not overwrite a run.
        let mut other = gold();
        other[0].text.push_str("\n{\"id\":\"zz\",\"check_type\":\"grounded\",\"claim\":\"c\",\"context\":[{\"source\":\"s\",\"text\":\"t\"}],\"label\":\"supported\",\"split\":\"tune\"}\n");
        let e = go_store(
            &settings(),
            &other,
            &dir,
            true,
            &scripted(),
            &Srv::new(),
            &store,
        )
        .expect_err("gold");
        assert!(e.to_string().contains("gold files"), "{e}");
        let e = run1(&settings(), &scripted(), &Srv::new(), false).expect_err("exists");
        assert!(e.to_string().contains("--resume"), "{e}");
        // Resuming a directory that holds no run is an error, not a fresh start.
        let none = tmp("none");
        let e = go(&settings(), &none, true, &scripted(), &Srv::new()).expect_err("none");
        assert!(e.to_string().contains("manifest.json"), "{e}");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_corrupt_outcome_in_the_middle_is_an_error() {
        let dir = tmp("corrupt");
        go(&settings(), &dir, false, &scripted(), &Srv::new()).expect("run");
        let path = dir.join("verdicts.jsonl");
        let text = std::fs::read_to_string(&path).expect("read");
        std::fs::write(&path, format!("garbage\n{text}")).expect("write");
        let e = report_dir(&dir, None).expect_err("corrupt");
        assert!(e.to_string().contains("line 1"), "{e}");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_server_that_cannot_be_reached_stops_the_run_without_recording() {
        let dir = tmp("down");
        let chat = Fake::new(|_, _| Err(net_error()));
        let e = go(&settings(), &dir, false, &chat, &Srv::new()).expect_err("down");
        assert!(
            e.to_string()
                .contains("transport or server (5xx) failures in a row"),
            "{e}"
        );
        assert_eq!(chat.calls(), MAX_CONSECUTIVE_TRANSPORT);
        assert!(read_lines(&dir, false).expect("lines").is_empty());

        cleanup(&[&dir]);
    }

    fn timeout_error() -> Error {
        Error::http("chat", ureq::Error::Timeout(ureq::Timeout::Global))
    }

    /// Fails the first call for each id in `fail`, answers everything else.
    fn flaky(fail: &'static [&'static str], err: fn() -> Error) -> Fake {
        let seen = RefCell::new(HashSet::new());
        Fake::new(move |id, _| {
            if fail.contains(&id) && seen.borrow_mut().insert(id.to_string()) {
                return Err(err());
            }
            Ok(resp(&reply("unsupported", "")))
        })
    }

    #[test]
    fn network_errors_are_retried_by_resume_and_never_recorded() {
        let dir = tmp("flaky");
        let chat = flaky(&["g2", "r1"], net_error);
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 2));
        assert_eq!(rep.claims_attempted, 5);
        assert!(rep.unusable.is_empty());
        assert_eq!(rep.hint.as_deref(), Some("rerun with --resume <dir>"));
        let lines = read_lines(&dir, false).expect("lines");
        assert!(
            lines
                .iter()
                .all(|l| l.claim_id != "g2" && l.claim_id != "r1")
        );
        // The same on disk, and --report-only says the same.
        let m: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("metrics.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(
            (m["status"].as_str(), m["pending"].as_u64()),
            (Some("incomplete"), Some(2))
        );
        let again = report_dir(&dir, None).expect("report only");
        assert_eq!((again.status.as_str(), again.pending), ("incomplete", 2));

        let rep = go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        assert_eq!(rep.claims_attempted, 7);
        assert!(rep.unusable.is_empty());
        assert!(rep.rows.iter().all(|r| r.missing == 0));
        assert_ne!(rep.default_result, "incomplete");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_second_timeout_on_one_claim_is_recorded_as_unusable() {
        let dir = tmp("slow");
        let chat = Fake::new(|id, _| {
            if id == "g2" {
                return Err(timeout_error());
            }
            Ok(resp(&reply("unsupported", "")))
        });
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        let att: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("attempts.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(att["g2"]["timeouts"], 1);

        let rep = go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        let lines = read_lines(&dir, false).expect("lines");
        let g2 = lines.iter().find(|l| l.claim_id == "g2").expect("g2");
        assert_eq!(
            (g2.status.as_str(), g2.error_code.as_deref()),
            ("unusable", Some("timeout"))
        );
        assert!(
            g2.error
                .as_deref()
                .expect("value")
                .contains("timed out twice; long think or stuck server")
        );
        assert_eq!(rep.unusable.get("grounded"), Some(&1));
        let att: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("attempts.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(att["g2"]["timeouts"], 2);
        assert!(att["g2"]["last_error"].as_str().is_some());
        // The recorded timeout counts against the model.
        assert_eq!(rep.rows[0].missing, 1);
        assert_eq!(rep.default_result, "fail");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_second_server_error_on_one_claim_is_recorded_as_unusable() {
        // Ollama returning 500 on the same claim every time (it cancels the task
        // mid-reply) used to leave the run incomplete forever.
        let dir = tmp("five-hundred");
        let chat = Fake::new(|id, _| {
            if id == "g2" {
                return Err(Error::Ollama("chat: http 500: cancelled".into()));
            }
            Ok(resp(&reply("unsupported", "")))
        });
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        assert!(rep.unusable.is_empty());
        let att: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("attempts.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(
            (
                att["g2"]["server_errors"].as_u64(),
                att["g2"]["timeouts"].as_u64()
            ),
            (Some(1), Some(0))
        );

        let rep = go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        let lines = read_lines(&dir, false).expect("lines");
        let g2 = lines.iter().find(|l| l.claim_id == "g2").expect("g2");
        assert_eq!(
            (g2.status.as_str(), g2.error_code.as_deref()),
            ("unusable", Some("server_error"))
        );
        let err = g2.error.as_deref().expect("value");
        assert!(
            err.contains("server error or dropped reply twice") && err.contains("http 500"),
            "{err}"
        );
        assert_eq!(rep.unusable.get("grounded"), Some(&1));
        assert_eq!(rep.rows[0].missing, 1);
        assert_eq!(rep.default_result, "fail");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_timeout_then_a_server_error_are_counted_apart() {
        // One of each is two different one-off failures, not a repeat: still retryable.
        let dir = tmp("mixed");
        let calls = RefCell::new(0u32);
        let chat = Fake::new(move |id, _| {
            if id == "g2" {
                *calls.borrow_mut() += 1;
                return Err(if *calls.borrow() == 1 {
                    timeout_error()
                } else {
                    Error::Ollama("chat: http 502: bad gateway".into())
                });
            }
            Ok(resp(&reply("unsupported", "")))
        });
        go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        let rep = go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        assert!(rep.unusable.is_empty());
        let att: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("attempts.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(
            (
                att["g2"]["timeouts"].as_u64(),
                att["g2"]["server_errors"].as_u64()
            ),
            (Some(1), Some(1))
        );
        cleanup(&[&dir]);
    }

    #[test]
    fn a_reply_dropped_twice_on_one_claim_is_recorded_once_the_server_is_healthy() {
        // Ollama aborting one generation can reach the client as a dropped connection,
        // not a 500. Two on the same claim, with the server answering others, count.
        let dir = tmp("dropped");
        let chat = Fake::new(|id, _| {
            if id == "g2" {
                return Err(net_error());
            }
            Ok(resp(&reply("unsupported", "")))
        });
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        let rep = go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        let lines = read_lines(&dir, false).expect("lines");
        let g2 = lines.iter().find(|l| l.claim_id == "g2").expect("g2");
        assert_eq!(
            (g2.status.as_str(), g2.error_code.as_deref()),
            ("unusable", Some("server_error"))
        );
        assert!(
            g2.error
                .as_deref()
                .expect("value")
                .contains("dropped reply twice")
        );
        assert_eq!(rep.default_result, "fail");
        cleanup(&[&dir]);
    }

    #[test]
    fn a_server_that_cannot_be_reached_on_resume_charges_nothing() {
        let dir = tmp("refused-later");
        let first = Fake::new(|id, _| {
            if id == "g2" {
                return Err(net_error());
            }
            Ok(resp(&reply("unsupported", "")))
        });
        go(&settings(), &dir, false, &first, &Srv::new()).expect("run");
        let down = Fake::new(|_, _| Err(net_error()));
        let rep = go(&settings(), &dir, true, &down, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        assert!(rep.unusable.is_empty());
        cleanup(&[&dir]);
    }

    #[test]
    fn a_server_that_stays_down_is_never_charged_to_the_model() {
        // Every claim gets 500 (an out-of-memory load, say): the run stops, and a resume
        // against the same broken server records nothing. No claim has an answer to
        // re-ask, so no repeat can be shown to be the model's.
        let dir = tmp("all-500");
        let chat = Fake::new(|_, _| Err(Error::Ollama("chat: http 500: out of memory".into())));
        assert!(go(&settings(), &dir, false, &chat, &Srv::new()).is_err());
        assert!(go(&settings(), &dir, true, &chat, &Srv::new()).is_err());
        let rep = report_dir(&dir, None).expect("report only");
        assert_eq!(rep.status.as_str(), "incomplete");
        assert!(rep.unusable.is_empty());
        assert!(read_lines(&dir, false).expect("lines").is_empty());
        cleanup(&[&dir]);
    }

    #[test]
    fn a_repeat_while_the_server_fails_its_health_check_is_not_recorded() {
        // g2 gets 500 in the first run. On resume the whole server is broken: the
        // health check (re-asking an answered claim) fails, so g2 is not charged.
        let dir = tmp("broken-on-resume");
        let first = Fake::new(|id, _| {
            if id == "g2" {
                return Err(Error::Ollama("chat: http 500: cancelled".into()));
            }
            Ok(resp(&reply("unsupported", "")))
        });
        go(&settings(), &dir, false, &first, &Srv::new()).expect("run");
        let broken = Fake::new(|_, _| Err(Error::Ollama("chat: http 500: out of memory".into())));
        let rep = go(&settings(), &dir, true, &broken, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        assert!(rep.unusable.is_empty());
        let att: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("attempts.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(att["g2"]["server_errors"].as_u64(), Some(1));
        // Once the server is back, the next repeat is the model's.
        let rep = go(&settings(), &dir, true, &first, &Srv::new()).expect("resume");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        assert_eq!(rep.unusable.get("grounded"), Some(&1));
        cleanup(&[&dir]);
    }

    #[test]
    fn attempts_written_before_server_errors_were_counted_still_read() {
        let dir = tmp("old-attempts");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("attempts.json");
        std::fs::write(&path, r#"{"g2":{"timeouts":1,"last_error":"x"}}"#).expect("write");
        let att = read_attempts(&path).expect("read");
        assert_eq!((att["g2"].timeouts, att["g2"].server_errors), (1, 0));
        cleanup(&[&dir]);
    }

    #[test]
    fn a_run_with_pending_claims_is_incomplete_not_failed() {
        let dir = tmp("incomplete");
        let chat = flaky(&["g1"], net_error);
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!(rep.default_result, "incomplete");
        assert!(!rep.passes_default);
        let table = render_table(&rep);
        assert!(
            table.contains("default rule overall: INCOMPLETE"),
            "{table}"
        );
        assert!(table.contains("rerun with --resume"), "{table}");
        assert!(table.contains("count as missing answers"), "{table}");
        let m: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("metrics.json")).expect("value"),
        )
        .expect("value");
        assert_eq!(m["default_result"], "incomplete");
        assert!(
            m["rule_note"]
                .as_str()
                .expect("value")
                .contains("repeated timeout")
        );
        cleanup(&[&dir]);
    }

    #[test]
    fn http_5xx_is_transport_and_a_bad_verdict_still_fails_the_model() {
        let dir = tmp("fivexx");
        let chat = flaky(&["g3"], || Error::Ollama("chat: http 503: busy".into()));
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("incomplete", 1));
        cleanup(&[&dir]);

        let dir = tmp("badverdict");
        let chat = Fake::new(|id, _| {
            Ok(resp(&if id == "g2" {
                "I think it is fine".to_string()
            } else {
                reply("unsupported", "")
            }))
        });
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert_eq!((rep.status.as_str(), rep.pending), ("complete", 0));
        assert_eq!(rep.unusable.get("grounded"), Some(&1));
        let lines = read_lines(&dir, false).expect("lines");
        let g2 = lines.iter().find(|l| l.claim_id == "g2").expect("g2");
        assert_eq!(g2.error_code.as_deref(), Some("bad_verdict"));
        assert_eq!(rep.default_result, "fail");
        assert!(!rep.passes_default);
        cleanup(&[&dir]);
    }

    #[test]
    fn auto_structured_falls_back_to_plain_text_when_the_schema_collapses() {
        let plain = "```json\n{\"reasoning\":\"r\",\"verdict\":\"unsupported\",\"evidence_quote\":\"\",\"evidence_source\":\"\"}\n```";
        let dir = tmp("auto");
        let chat =
            Fake::new(move |_, req| Ok(resp(if req.format.is_some() { "{}}" } else { plain })));
        let rep = go(&settings(), &dir, false, &chat, &Srv::new()).expect("run");
        assert!(rep.unusable.is_empty());
        // The first claim tried structured twice, then plain text; the rest went plain.
        assert_eq!(chat.calls(), 2 + 1 + 6);
        let lines = read_lines(&dir, false).expect("lines");
        assert!(
            lines
                .iter()
                .all(|l| !l.pins.as_ref().expect("pins").structured)
        );
        // A resume carries on in plain text.
        let path = dir.join("verdicts.jsonl");
        let text = std::fs::read_to_string(&path).expect("read");
        let first: Vec<&str> = text.lines().take(6).collect();
        std::fs::write(&path, first.join("\n") + "\n").expect("write");
        let chat = Fake::new(|_, req| {
            assert!(req.format.is_none());
            Ok(resp(&reply("unsupported", "")))
        });
        go(&settings(), &dir, true, &chat, &Srv::new()).expect("resume");
        assert_eq!(chat.calls(), 1);

        // structured=on never falls back: the failure is unusable.
        let dir2 = tmp("on");
        let mut s = settings();
        s.structured = Structured::On;
        let chat = Fake::new(|_, req| {
            assert!(req.format.is_some());
            Ok(resp("nope"))
        });
        let rep = go(&s, &dir2, false, &chat, &Srv::new()).expect("run");
        assert_eq!(rep.unusable.get("grounded"), Some(&4));
        // structured=off never sends a schema.
        let dir3 = tmp("off");
        s.structured = Structured::Off;
        let chat = Fake::new(|_, req| {
            assert!(req.format.is_none());
            Ok(resp(&reply("unsupported", "")))
        });
        go(&s, &dir3, false, &chat, &Srv::new()).expect("run");
        cleanup(&[&dir, &dir2, &dir3]);
    }

    #[test]
    fn the_server_must_have_the_model_and_it_must_be_local() {
        let chat = scripted();
        let mut srv = Srv::new();
        srv.installed = false;
        let e = go(&settings(), &tmp("missing"), false, &chat, &srv).expect_err("missing");
        assert!(e.to_string().contains("not installed"), "{e}");
        let mut srv = Srv::new();
        srv.remote = "https://ollama.com:443".into();
        let e = go(&settings(), &tmp("remote"), false, &chat, &srv).expect_err("remote");
        assert!(e.to_string().contains("hosted service"), "{e}");
        assert_eq!(chat.calls(), 0);
        // A model named with its tag matches exactly.
        let tags = Srv::new().tags().expect("tags");
        assert!(find_tag(&tags, "other:1b").is_some());
        assert!(find_tag(&tags, "judge").is_some());
        assert!(find_tag(&tags, "judge:2").is_none());
    }

    #[test]
    fn selection_follows_split_check_type_limit_and_labels() {
        let all = read_gold(&gold()).expect("gold");
        assert_eq!(all.len(), 9);
        assert_eq!(all[5].has_doc_comment, Some(true));
        assert_eq!(all[6].self_referential, Some(true));
        let ids = |s: &Settings| {
            let (sel, skipped) = select(all.clone(), s).expect("select");
            (
                sel.iter().map(|e| e.claim.id.clone()).collect::<Vec<_>>(),
                skipped,
            )
        };
        let mut s = settings();
        assert_eq!(ids(&s).0, ["g1", "g2", "g3", "g4", "r1", "r2", "k1"]);
        assert_eq!(ids(&s).1, 1);
        s.split = Split::Heldout;
        assert_eq!(ids(&s).0, ["g5"]);
        s.split = Split::All;
        s.check_type = Some(CheckType::Reasoning);
        assert_eq!(ids(&s).0, ["r1", "r2"]);
        s.check_type = None;
        s.limit = Some(2);
        assert_eq!(ids(&s).0, ["g1", "g2"]);
        // Nothing selected is an error for the run.
        s.limit = Some(0);
        let e = go(&s, &tmp("empty"), false, &scripted(), &Srv::new()).expect_err("empty");
        assert!(e.to_string().contains("no labelled gold claim"), "{e}");
        // A repeated id is refused, and a bad line names its file and line.
        let mut dup = all.clone();
        dup.push(all[0].clone());
        let e = select(dup, &settings()).expect_err("dup");
        assert!(e.to_string().contains("twice"));
        let bad = vec![GoldFile {
            name: "x.jsonl".into(),
            text: "\n{\"id\":1}\nnot json".into(),
        }];
        let e = read_gold(&bad).expect_err("bad").to_string();
        assert!(e.contains("x.jsonl line 2"), "{e}");
        let bad = vec![GoldFile {
            name: "y.jsonl".into(),
            text: "nope".into(),
        }];
        let e = read_gold(&bad).expect_err("bad").to_string();
        assert!(e.contains("y.jsonl line 1"), "{e}");
    }

    #[test]
    fn option_values_parse_and_say_what_was_wrong() {
        assert_eq!(parse_think("high").expect("think"), ThinkLevel::High);
        for t in ["off", "on", "low", "medium"] {
            assert_eq!(parse_think(t).expect("think").as_str(), t);
        }
        assert!(parse_think("max").is_err());
        assert_eq!(Structured::parse("auto").expect("s"), Structured::Auto);
        assert_eq!(Structured::parse("on").expect("s").as_str(), "on");
        assert_eq!(Structured::parse("off").expect("s").as_str(), "off");
        assert_eq!(Structured::Auto.as_str(), "auto");
        assert!(Structured::parse("maybe").is_err());
        for (t, want) in [
            ("tune", Split::Tune),
            ("heldout", Split::Heldout),
            ("all", Split::All),
        ] {
            assert_eq!(Split::parse(t).expect("split"), want);
            assert_eq!(want.as_str(), t);
        }
        assert!(Split::parse("dev").is_err());
        assert_eq!(parse_check_filter("all").expect("all"), None);
        assert_eq!(
            parse_check_filter("knowledge").expect("k"),
            Some(CheckType::Knowledge)
        );
        assert!(parse_check_filter("vibes").is_err());
    }

    #[test]
    fn output_names_are_safe_and_dated() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(1_709_164_800 + 3661), "20240229T010101Z");
        assert_eq!(utc_stamp(1_791_473_400), "20261008T153000Z");
        let d = default_out_dir(Path::new("proj"), "qwen3:32b/x", 0);
        assert_eq!(
            d,
            Path::new("proj")
                .join(".offrig")
                .join("out")
                .join("calibrate-qwen3-32b-x-19700101T000000Z")
        );
        assert_eq!(display_name(Path::new("a/b/gold.jsonl")), "gold.jsonl");
        assert_eq!(display_name(Path::new("")), "gold.jsonl");
    }

    #[test]
    fn the_rule_report_agrees_with_the_metrics_rule() {
        // A model that is right everywhere on 100+100 grounded and reasoning claims.
        let mut lines = vec![];
        let mut selected = vec![];
        for ct in [CheckType::Grounded, CheckType::Reasoning] {
            for i in 0..100 {
                for (tag, label) in [("u", Label::Unsupported), ("s", Label::Supported)] {
                    let sel = Selected {
                        id: format!("{}{tag}{i}", ct.as_str()),
                        check_type: ct,
                        label,
                        subtle: false,
                        has_doc_comment: None,
                        self_referential: None,
                        origin: None,
                    };
                    let mut l = Line::base(&sel, "file", 1.0);
                    l.final_verdict = Some(if tag == "s" {
                        VerdictKind::Supported
                    } else {
                        VerdictKind::Unsupported
                    });
                    lines.push(l);
                    selected.push(sel);
                }
            }
        }
        let m = Manifest {
            offrig_version: "x".into(),
            started_at: 0,
            resumes: vec![],
            model: "m".into(),
            model_digest: None,
            ollama_version: "v".into(),
            settings: json!({}),
            gpu_cost_hr: 0.0,
            gold: vec![],
            skipped_unlabelled: 0,
            selected,
        };
        let rep = build_report(&m, &lines, 0.0);
        assert!(rep.passes_default);
        assert!(rep.rule[0].pass && rep.rule[1].pass);
        assert!(rep.rule[0].criteria.iter().all(|c| c.pass));
        assert!(rep.rule[2].criteria.is_empty() && !rep.rule[2].pass && !rep.rule[2].counts);
        assert!(render_table(&rep).contains("knowledge (reported only): not run"));
        // Drop one answer: the rule fails on completeness, as the metrics rule does.
        let rep = build_report(&m, &lines[1..], 0.0);
        assert!(!rep.passes_default);
        assert!(!rep.rule[0].criteria[0].pass);
        assert_eq!(rep.rule[0].pass, rep.rows[0].passes_default_rule);
    }
}
