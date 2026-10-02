//! offrig-mcp: the side-car. An MCP server (stdio) that an agent calls as an
//! instrument: read the account and the budget, plan a GPU session, keep project
//! memory, and queue role-headed handoffs. Design and evidence: docs/sidecar-design.md.
//!
//! Phase 1 tools spend nothing. Launch, job, ask and shutdown arrive in phase 2.
//! The budget cap is set by a human with `offrig budget`; no tool here can raise it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use offrig_core::config::Config;
use offrig_core::cost;
use offrig_core::error::chain;
use offrig_core::roles;
use offrig_core::runpod::RunPod;
use offrig_core::store::{self, Kind, NewHandoff, NewPlan, NewRecord, Query, Store};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

/// A handoff in flight this long without progress is stale (shown by status, reaped
/// by the runner). One value, one predicate: `store::is_stale`.
const STALE_SECS: i64 = 1800;

#[derive(Clone)]
pub struct Sidecar {
    store: Arc<Mutex<Store>>,
    cfg: Arc<Config>,
    project: Arc<PathBuf>,
}

fn ok(mut v: Value) -> CallToolResult {
    if let Value::Object(m) = &mut v {
        m.insert("ok".into(), Value::Bool(true));
    }
    CallToolResult::structured(v)
}

/// Errors are results the agent can act on, never protocol errors.
fn fail(error: impl Into<String>, next_action: impl Into<String>) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "ok": false,
        "error": error.into(),
        "next_action": next_action.into(),
    }))
}

fn budget_json(b: &store::Budget) -> Value {
    json!({
        "cap": round2(b.cap),
        "committed": round2(b.committed),
        "spent": round2(b.spent),
        "remaining": round2(b.remaining),
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn record_json(r: &store::Record) -> Value {
    json!({
        "id": r.id,
        "kind": r.kind.as_str(),
        "body": r.body,
        "source": r.source,
        "author": r.author,
        "task_id": r.task_id,
        "date": cost::date_utc(r.created_at),
    })
}

fn handoff_json(h: &store::Handoff, now: i64) -> Value {
    json!({
        "id": h.id,
        "role": h.role_id,
        "mission": h.mission,
        "acceptance": h.acceptance,
        "scope": h.scope,
        "depends_on": h.depends_on,
        "state": h.state.as_str(),
        "stale": store::is_stale(h, now, STALE_SECS),
        "attempts": h.attempts,
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OffersArgs {
    /// GPUs per pod. 4 is the frontier tier (4x RTX PRO 6000), 1 the medium tier.
    pub gpu_count: u32,
    /// Only offers with at least this much VRAM in total, in GB.
    #[serde(default)]
    pub min_vram_gb: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanArgs {
    /// Profile name from offrig's config: small, medium or frontier.
    pub profile: String,
    /// The most hours this session may run. The plan is priced at this worst case.
    pub max_hours: f64,
    /// What the session is for, kept with the plan.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Words to look for. Identifiers, paths and error text match well.
    pub query: String,
    /// Restrict to one kind: brief, constraint, decision, fact or checkpoint.
    #[serde(default)]
    pub kind: Option<String>,
    /// Restrict to records tied to one handoff id.
    #[serde(default)]
    pub task_id: Option<i64>,
    /// At most this many results (default 8, max 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecordArgs {
    /// brief (what the project is), constraint (a binding rule, always shown to
    /// every handoff), decision (what was chosen and why), fact (something verified,
    /// with a source), or checkpoint (where a handoff stands).
    pub kind: String,
    /// The record itself: one fact, decision or rule per record.
    pub body: String,
    /// Who or what wrote it, e.g. "claude-code" or a model name.
    pub author: String,
    /// Where it came from: a file, URL, commit or handoff id.
    #[serde(default)]
    pub source: Option<String>,
    /// The handoff this belongs to, if any.
    #[serde(default)]
    pub task_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The active record this replaces. Required when it contradicts one.
    #[serde(default)]
    pub supersedes: Option<i64>,
    /// Why it replaces the old record. Required with `supersedes`.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HandoffArgs {
    /// add, list, roles (available role ids) or preview (render a role block).
    pub action: String,
    /// add, preview: the role id (Role OS id or a built-in game role).
    #[serde(default)]
    pub role: Option<String>,
    /// add: what to do, in one paragraph.
    #[serde(default)]
    pub mission: Option<String>,
    /// add: how to tell it is done (a test command or a checkable statement).
    #[serde(default)]
    pub acceptance: Option<String>,
    /// add: paths the handoff may change; anything else is a violation.
    #[serde(default)]
    pub scope: Vec<String>,
    /// add: handoff ids that must complete first.
    #[serde(default)]
    pub depends_on: Vec<i64>,
}

impl Sidecar {
    pub fn open(project: &Path, cfg: Config) -> anyhow::Result<Self> {
        let db = project.join(".offrig").join("offrig.db");
        let store = Store::open(&db)?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            cfg: Arc::new(cfg),
            project: Arc::new(project.to_path_buf()),
        })
    }

    /// A panic mid-write leaves no half state behind (SQLite rolls the transaction
    /// back), so a poisoned lock is safe to keep using.
    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn role_os(&self) -> Option<PathBuf> {
        self.cfg.role_os_dir.as_ref().map(PathBuf::from)
    }
}

#[tool_router]
impl Sidecar {
    #[tool(
        name = "offrig_status",
        description = "Where things stand: budget (cap, committed, spent, remaining), RunPod balance and runway, offrig's pods, open plans, the handoff queue with stale work flagged, and unfinished side effects. Call first in any session, and whenever unsure. Read-only; costs nothing.",
        annotations(title = "offrig status", read_only_hint = true, open_world_hint = true)
    )]
    async fn offrig_status(&self) -> CallToolResult {
        let now = cost::now_unix();
        let local = {
            let s = self.store();
            (s.budget(), s.handoffs(), s.unfinished_journal())
        };
        let (budget, handoffs, journal) = match local {
            (Ok(b), Ok(h), Ok(j)) => (b, h, j),
            (b, h, j) => {
                let e = b
                    .err()
                    .or(h.err())
                    .or(j.err())
                    .map(|e| chain(&e))
                    .unwrap_or_default();
                return fail(e, "check the project database at .offrig/offrig.db");
            }
        };
        let remote = tokio::task::spawn_blocking(|| {
            let rp = RunPod::from_env()?;
            Ok::<_, offrig_core::Error>((rp.account().ok(), rp.list_pods()?))
        })
        .await;
        let (account, pods, remote_note) = match remote {
            Ok(Ok((a, p))) => (a, p, None),
            Ok(Err(e)) => (None, Vec::new(), Some(chain(&e))),
            Err(e) => (None, Vec::new(), Some(e.to_string())),
        };
        let ours: Vec<Value> = pods
            .iter()
            .filter(|p| p.name.starts_with("offrig-"))
            .map(|p| json!({"name": p.name, "id": p.id, "status": p.desired_status, "cost_per_hr": p.cost_per_hr}))
            .collect();
        let others = pods.len() - ours.len();
        let stale = handoffs
            .iter()
            .filter(|h| store::is_stale(h, now, STALE_SECS))
            .count();
        let count = |st: store::State| handoffs.iter().filter(|h| h.state == st).count();
        let next = if budget.cap <= 0.0 {
            "no budget is set; ask the human to run `offrig budget <usd>` before planning a paid session".to_string()
        } else if !journal.is_empty() {
            format!(
                "{} side effect(s) never recorded an outcome; check pods before launching",
                journal.len()
            )
        } else if stale > 0 {
            format!("{stale} handoff(s) are past the {STALE_SECS}s timeout")
        } else if store::Store::ready(&self.store())
            .map(|r| !r.is_empty())
            .unwrap_or(false)
        {
            "handoffs are ready; plan a session or work them".into()
        } else {
            "record the project brief and constraints, then queue handoffs".into()
        };
        ok(json!({
            "project": self.project.display().to_string(),
            "budget": budget_json(&budget),
            "runpod": {
                "balance": account.as_ref().map(|a| round2(a.client_balance)),
                "spend_per_hr": account.as_ref().map(|a| round2(a.current_spend_per_hr)),
                "runway_hours": account.as_ref().and_then(|a| a.runway_hours(0.0)).map(|h| (h * 10.0).round() / 10.0),
                "offrig_pods": ours,
                "other_pods": others,
                "note": remote_note,
            },
            "handoffs": {
                "pending": count(store::State::Pending),
                "in_flight": count(store::State::Dispatched) + count(store::State::Running),
                "complete": count(store::State::Complete),
                "blocked": count(store::State::InvalidOutput) + count(store::State::OwnershipViolation),
                "failed_or_timed_out": count(store::State::Failed) + count(store::State::TimedOut),
                "stale": stale,
            },
            "unfinished_side_effects": journal.len(),
            "next_action": next,
        }))
    }

    #[tool(
        name = "offrig_offers",
        description = "Live RunPod secure-cloud offers for a GPU count: GPU type, total VRAM, total $/hr, cheapest first; GPUs with none free are left out. Use before offrig_plan to see what a session would cost now. Read-only; costs nothing.",
        annotations(title = "GPU offers", read_only_hint = true, open_world_hint = true)
    )]
    async fn offrig_offers(&self, Parameters(a): Parameters<OffersArgs>) -> CallToolResult {
        if a.gpu_count == 0 || a.gpu_count > 8 {
            return fail(
                "gpu_count must be 1 to 8",
                "use 1 for the medium tier or 4 for frontier",
            );
        }
        let n = a.gpu_count;
        let res = tokio::task::spawn_blocking(move || RunPod::from_env()?.gpu_offers(n)).await;
        let offers = match res {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return fail(chain(&e), "check RUNPOD_API_KEY and network, then retry"),
            Err(e) => return fail(e.to_string(), "retry"),
        };
        let min = a.min_vram_gb.unwrap_or(0);
        let list: Vec<Value> = offers
            .iter()
            .filter(|o| o.price_per_hr.is_some() && o.total_vram_gb() >= min)
            .take(15)
            .map(|o| json!({"gpu": o.id, "count": o.gpu_count, "vram_gb": o.total_vram_gb(), "usd_per_hr": o.price_per_hr, "stock": o.stock}))
            .collect();
        let next = if list.is_empty() {
            "nothing free at this count; a frontier plan can still wait for capacity when launched"
        } else {
            "pick a profile and call offrig_plan"
        };
        ok(json!({"offers": list, "next_action": next}))
    }

    #[tool(
        name = "offrig_plan",
        description = "Price a session before any money is spent: takes a profile and max_hours, reads the live price for the profile's GPUs, and records a plan at its worst case (price x max_hours). Refused if the worst case exceeds the budget left. Returns plan_id, which launching will require. Writes a plan; spends nothing.",
        annotations(
            title = "Plan a session",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn offrig_plan(&self, Parameters(a): Parameters<PlanArgs>) -> CallToolResult {
        let profile = match self.cfg.profile(&a.profile) {
            Ok(p) => p.clone(),
            Err(_) => {
                let names: Vec<&str> = self.cfg.profiles.iter().map(|p| p.name.as_str()).collect();
                return fail(
                    format!("no profile {:?}", a.profile),
                    format!("use one of: {}", names.join(", ")),
                );
            }
        };
        let (count, types) = (profile.gpu_count, profile.gpu_type_ids.clone());
        let res = tokio::task::spawn_blocking(move || {
            let rp = RunPod::from_env()?;
            Ok::<_, offrig_core::Error>((rp.gpu_offers(count)?, rp.account().ok()))
        })
        .await;
        let (offers, account) = match res {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return fail(
                    chain(&e),
                    "plans need live prices; check RUNPOD_API_KEY and network",
                );
            }
            Err(e) => return fail(e.to_string(), "retry"),
        };
        // The price the plan is held to: the dearest free listed GPU, because RunPod
        // takes the first type in priority order that has capacity.
        let listed: Vec<f64> = offers
            .iter()
            .filter(|o| types.contains(&o.id))
            .filter_map(|o| o.price_per_hr)
            .collect();
        let Some(max_price) = listed.iter().copied().reduce(f64::max) else {
            return fail(
                format!(
                    "none of the {} profile's GPUs has {count} free right now, so there is no price to plan against",
                    profile.name
                ),
                "check offrig_offers later, or plan a different profile",
            );
        };
        let plan = self.store().create_plan(NewPlan {
            profile: profile.name.clone(),
            gpu_count: count,
            gpu_types: types,
            max_hours: a.max_hours,
            max_price_hr: max_price,
            note: a.note,
        });
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                return fail(
                    chain(&e),
                    "shorten max_hours or choose a cheaper profile; the cap itself is the human's to change",
                );
            }
        };
        let budget = self.store().budget().ok();
        let runway = account.as_ref().and_then(|ac| ac.runway_hours(max_price));
        ok(json!({
            "plan_id": plan.id,
            "profile": plan.profile,
            "gpus": format!("{}x {}", plan.gpu_count, plan.gpu_types.join(" | ")),
            "max_price_hr": round2(plan.max_price_hr),
            "max_hours": plan.max_hours,
            "worst_case": round2(plan.worst_case),
            "budget": budget.as_ref().map(budget_json),
            "runpod_runway_hours_with_pod": runway.map(|h| (h * 10.0).round() / 10.0),
            "models": profile.models.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
            "next_action": "launching arrives in phase 2; until then, queue handoffs against this plan",
        }))
    }

    #[tool(
        name = "offrig_memory_search",
        description = "Search the project's memory: active records only (superseded and withdrawn ones are hidden), best match first, each with its source and date. Use before deciding anything the project may already have decided. Read-only.",
        annotations(
            title = "Search project memory",
            read_only_hint = true,
            open_world_hint = false
        )
    )]
    async fn offrig_memory_search(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        let kind = match a.kind.as_deref().map(Kind::parse).transpose() {
            Ok(k) => k,
            Err(e) => {
                return fail(
                    chain(&e),
                    "omit kind or use brief, constraint, decision, fact or checkpoint",
                );
            }
        };
        let q = Query {
            text: a.query,
            kind,
            task_id: a.task_id,
            limit: a.limit.unwrap_or(8),
        };
        match self.store().search(&q) {
            Ok(rs) => {
                let n = rs.len();
                ok(json!({
                    "results": rs.iter().map(record_json).collect::<Vec<_>>(),
                    "next_action": if n == 0 { "nothing recorded on this; record what you learn with offrig_memory_record" } else { "cite records by id when you rely on them" },
                }))
            }
            Err(e) => fail(chain(&e), "simplify the query to a few plain words"),
        }
    }

    #[tool(
        name = "offrig_memory_record",
        description = "Add one record to project memory: brief, constraint (binding; shown to every handoff), decision, fact (with a source) or checkpoint. Records are never edited: to change one, add a new record with supersedes=<old id> and a reason. Writes to the project database; nothing outside it.",
        annotations(
            title = "Record to project memory",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn offrig_memory_record(&self, Parameters(a): Parameters<RecordArgs>) -> CallToolResult {
        let kind = match Kind::parse(&a.kind) {
            Ok(k) => k,
            Err(e) => {
                return fail(
                    chain(&e),
                    "use brief, constraint, decision, fact or checkpoint",
                );
            }
        };
        let res = self.store().record(NewRecord {
            kind: Some(kind),
            body: a.body,
            author: a.author,
            source: a.source,
            task_id: a.task_id,
            tags: a.tags,
            supersedes: a.supersedes,
            reason: a.reason,
        });
        match res {
            Ok(id) => ok(
                json!({"id": id, "kind": kind.as_str(), "next_action": "cite it as #id; supersede it rather than contradict it"}),
            ),
            Err(e) => fail(
                chain(&e),
                "search for the record it conflicts with, then supersede it with a reason",
            ),
        }
    }

    #[tool(
        name = "offrig_handoffs",
        description = "The handoff queue. action=add queues one handoff (role, mission, acceptance check, file scope, dependencies); action=list shows the queue with stale work flagged; action=roles lists role ids; action=preview renders the role block a handoff would open with. Every handoff needs an acceptance check: a role shapes focus, the check decides done. Writes only to the project database.",
        annotations(
            title = "Handoff queue",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn offrig_handoffs(&self, Parameters(a): Parameters<HandoffArgs>) -> CallToolResult {
        let now = cost::now_unix();
        match a.action.as_str() {
            "add" => {
                let role = a.role.unwrap_or_default();
                if let Err(e) = roles::load(&role, self.role_os().as_deref()) {
                    return fail(
                        chain(&e),
                        "call offrig_handoffs with action=roles for valid ids",
                    );
                }
                let res = self.store().add_handoff(NewHandoff {
                    role_id: role,
                    mission: a.mission.unwrap_or_default(),
                    acceptance: a.acceptance.unwrap_or_default(),
                    scope: a.scope,
                    depends_on: a.depends_on,
                });
                match res {
                    Ok(id) => ok(
                        json!({"id": id, "state": "pending", "next_action": "add the rest of the queue, or preview the role block"}),
                    ),
                    Err(e) => fail(chain(&e), "fix the field named in the error and add again"),
                }
            }
            "list" => match self.store().handoffs() {
                Ok(hs) => ok(json!({
                    "handoffs": hs.iter().map(|h| handoff_json(h, now)).collect::<Vec<_>>(),
                    "next_action": "work ready handoffs in order; dependencies gate the rest",
                })),
                Err(e) => fail(chain(&e), "check the project database"),
            },
            "roles" => {
                let mut ids: Vec<String> =
                    roles::builtin_ids().into_iter().map(String::from).collect();
                if let Some(dir) = self.role_os() {
                    let ex = dir.join("dossier").join("examples");
                    if let Ok(rd) = std::fs::read_dir(&ex) {
                        let mut ros: Vec<String> = rd
                            .flatten()
                            .filter_map(|e| {
                                e.path()
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                            })
                            .collect();
                        ros.sort();
                        ids.extend(ros);
                    }
                }
                ok(
                    json!({"roles": ids, "role_os": self.cfg.role_os_dir, "next_action": "preview one before queuing work for it"}),
                )
            }
            "preview" => {
                let id = a.role.unwrap_or_default();
                match roles::load(&id, self.role_os().as_deref()) {
                    Ok(r) => {
                        let block = roles::render(&r);
                        ok(json!({
                            "role": id,
                            "origin": r.origin,
                            "ideal_source": r.dossier.ideal_source,
                            "fingerprint": roles::fingerprint(&block),
                            "block": block,
                            "next_action": "queue work for this role with action=add",
                        }))
                    }
                    Err(e) => fail(chain(&e), "call action=roles for valid ids"),
                }
            }
            other => fail(
                format!("unknown action {other:?}"),
                "use add, list, roles or preview",
            ),
        }
    }
}

#[tool_handler(
    name = "offrig",
    instructions = "offrig runs big models on rented RunPod GPUs for this project and keeps the project's memory. Start with offrig_status. Record the brief and binding constraints with offrig_memory_record, and search memory before deciding. Queue work with offrig_handoffs (every handoff needs an acceptance check). Price any paid session with offrig_plan first; the budget cap is set by the human, not by tools."
)]
impl ServerHandler for Sidecar {}

fn project_dir() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--project" {
            return args
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("--project needs a directory"));
        }
    }
    if let Ok(p) = std::env::var("OFFRIG_PROJECT")
        && !p.trim().is_empty()
    {
        return Ok(PathBuf::from(p));
    }
    Ok(std::env::current_dir()?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout carries the protocol; anything human goes to stderr.
    let project = project_dir()?;
    let cfg = Config::load()?;
    let sidecar = Sidecar::open(&project, cfg)?;
    let service = sidecar.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
