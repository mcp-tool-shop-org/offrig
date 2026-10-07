//! The project database: memory records, the budget ledger, plans, the action journal
//! and the handoff queue. One SQLite file per project; it outlives every pod.
//!
//! Rules this module enforces (see docs/sidecar-design.md for the evidence):
//! - records are never edited or deleted; a new record supersedes an old one through
//!   an explicit edge with a reason, and search returns active records only;
//! - spend is committed against a hard cap before a launch, from price x max hours;
//! - every handoff state change goes through one transition law and is logged;
//! - liveness is computed by one predicate that status and the reaper both use.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::checks::{Check, Outcome};
use crate::cost::now_unix;
use crate::error::{Error, Result};

const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS records (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('brief','constraint','decision','fact','checkpoint')),
  body          TEXT NOT NULL,
  status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','superseded','withdrawn')),
  supersedes_id INTEGER REFERENCES records(id),
  superseded_by INTEGER REFERENCES records(id),
  reason        TEXT,
  author        TEXT NOT NULL,
  source        TEXT,
  task_id       INTEGER,
  tags          TEXT NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS records_kind ON records(kind, status);
CREATE INDEX IF NOT EXISTS records_task ON records(task_id);

CREATE VIRTUAL TABLE IF NOT EXISTS records_fts USING fts5(body, tags, content='records', content_rowid='id');
CREATE TRIGGER IF NOT EXISTS records_ai AFTER INSERT ON records BEGIN
  INSERT INTO records_fts(rowid, body, tags) VALUES (new.id, new.body, new.tags);
END;

CREATE TABLE IF NOT EXISTS plans (
  id            INTEGER PRIMARY KEY,
  profile       TEXT NOT NULL,
  gpu_count     INTEGER NOT NULL,
  gpu_types     TEXT NOT NULL,
  max_hours     REAL NOT NULL,
  max_price_hr  REAL NOT NULL,
  worst_case    REAL NOT NULL,
  state         TEXT NOT NULL DEFAULT 'planned' CHECK (state IN ('planned','committed','closed','cancelled')),
  pod_id        TEXT,
  created_at    INTEGER NOT NULL,
  note          TEXT,
  committed_at  INTEGER,
  started_at    INTEGER
);

CREATE TABLE IF NOT EXISTS jobs (
  id          INTEGER PRIMARY KEY,
  plan_id     INTEGER NOT NULL REFERENCES plans(id),
  kind        TEXT NOT NULL,
  state       TEXT NOT NULL CHECK (state IN ('running','done','failed','cancelled')),
  progress    TEXT NOT NULL DEFAULT '{}',
  error       TEXT,
  started_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ledger (
  id       INTEGER PRIMARY KEY,
  plan_id  INTEGER NOT NULL REFERENCES plans(id),
  kind     TEXT NOT NULL CHECK (kind IN ('commit','actual','release')),
  amount   REAL NOT NULL,
  at       INTEGER NOT NULL,
  note     TEXT
);

CREATE TABLE IF NOT EXISTS journal (
  id       INTEGER PRIMARY KEY,
  action   TEXT NOT NULL,
  plan_id  INTEGER,
  intent   TEXT NOT NULL,
  outcome  TEXT,
  at       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS handoffs (
  id            INTEGER PRIMARY KEY,
  role_id       TEXT NOT NULL,
  mission       TEXT NOT NULL,
  acceptance    TEXT NOT NULL,
  scope         TEXT NOT NULL DEFAULT '[]',
  depends_on    TEXT NOT NULL DEFAULT '[]',
  state         TEXT NOT NULL DEFAULT 'pending',
  attempts      INTEGER NOT NULL DEFAULT 0,
  branch        TEXT,
  model         TEXT,
  role_hash     TEXT,
  prompt_hash   TEXT,
  result_record INTEGER REFERENCES records(id),
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  checks        TEXT NOT NULL DEFAULT '[]',
  accept_on_checks INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS outputs (
  id          INTEGER PRIMARY KEY,
  handoff_id  INTEGER NOT NULL REFERENCES handoffs(id),
  turn        INTEGER NOT NULL,
  body        TEXT NOT NULL,
  outcomes    TEXT NOT NULL DEFAULT '[]',
  model       TEXT NOT NULL,
  tokens      INTEGER,
  created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS outputs_handoff ON outputs(handoff_id, turn);

CREATE TABLE IF NOT EXISTS handoff_events (
  id          INTEGER PRIMARY KEY,
  handoff_id  INTEGER NOT NULL REFERENCES handoffs(id),
  from_state  TEXT NOT NULL,
  to_state    TEXT NOT NULL,
  reason      TEXT NOT NULL,
  override    INTEGER NOT NULL DEFAULT 0,
  at          INTEGER NOT NULL
);
"#;

pub struct Store {
    conn: Connection,
}

/// True when `table` exists and has no column `col` (a migration should add it).
fn table_lacks(conn: &Connection, table: &str, col: &str) -> Result<bool> {
    let mut st = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(db("reading a table's columns"))?;
    let cols: Vec<String> = st
        .query_map([], |r| r.get(1))
        .map_err(db("reading a table's columns"))?
        .collect::<rusqlite::Result<_>>()
        .map_err(db("reading a table's columns"))?;
    Ok(!cols.is_empty() && !cols.iter().any(|c| c == col))
}

fn db(what: &str) -> impl FnOnce(rusqlite::Error) -> Error + '_ {
    move |source| Error::Db {
        what: what.to_string(),
        source,
    }
}

// ---------------------------------------------------------------- records

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Brief,
    Constraint,
    Decision,
    Fact,
    Checkpoint,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Brief => "brief",
            Kind::Constraint => "constraint",
            Kind::Decision => "decision",
            Kind::Fact => "fact",
            Kind::Checkpoint => "checkpoint",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "brief" => Kind::Brief,
            "constraint" => Kind::Constraint,
            "decision" => Kind::Decision,
            "fact" => Kind::Fact,
            "checkpoint" => Kind::Checkpoint,
            other => {
                return Err(Error::Refused(format!(
                    "unknown record kind {other:?}; use brief, constraint, decision, fact or checkpoint"
                )));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    pub kind: Kind,
    pub body: String,
    pub status: String,
    pub supersedes_id: Option<i64>,
    pub superseded_by: Option<i64>,
    pub reason: Option<String>,
    pub author: String,
    pub source: Option<String>,
    pub task_id: Option<i64>,
    pub tags: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewRecord {
    pub kind: Option<Kind>,
    pub body: String,
    pub author: String,
    pub source: Option<String>,
    pub task_id: Option<i64>,
    pub tags: Vec<String>,
    /// The active record this one replaces. Requires `reason`.
    pub supersedes: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    pub kind: Option<Kind>,
    pub task_id: Option<i64>,
    pub limit: usize,
}

const RECORD_COLS: &str = "id, kind, body, status, supersedes_id, superseded_by, reason, author, source, task_id, tags, created_at";

fn record_row(r: &Row<'_>) -> rusqlite::Result<Record> {
    let kind: String = r.get(1)?;
    Ok(Record {
        id: r.get(0)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Fact),
        body: r.get(2)?,
        status: r.get(3)?,
        supersedes_id: r.get(4)?,
        superseded_by: r.get(5)?,
        reason: r.get(6)?,
        author: r.get(7)?,
        source: r.get(8)?,
        task_id: r.get(9)?,
        tags: r.get(10)?,
        created_at: r.get(11)?,
    })
}

/// Turn free text into an FTS5 query that cannot be a syntax error: each word is
/// quoted, and words are OR-ed so BM25 ranks by how many match.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ':'))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

// ---------------------------------------------------------------- plans and budget

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub id: i64,
    pub profile: String,
    pub gpu_count: u32,
    pub gpu_types: Vec<String>,
    pub max_hours: f64,
    pub max_price_hr: f64,
    pub worst_case: f64,
    pub state: String,
    pub pod_id: Option<String>,
    pub created_at: i64,
    pub note: Option<String>,
    /// When the worst case was committed (the plan's clock starts here).
    pub committed_at: Option<i64>,
    /// When the pod was created.
    pub started_at: Option<i64>,
}

impl Plan {
    /// The moment the plan's time is up: committed + max_hours. The watchdog
    /// terminates the pod at this time whether or not any agent is still around.
    pub fn deadline(&self) -> Option<i64> {
        self.committed_at
            .map(|t| t + (self.max_hours * 3600.0).round() as i64)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub plan_id: i64,
    pub kind: String,
    pub state: String,
    pub progress: serde_json::Value,
    pub error: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewPlan {
    pub profile: String,
    pub gpu_count: u32,
    pub gpu_types: Vec<String>,
    pub max_hours: f64,
    pub max_price_hr: f64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub cap: f64,
    /// Committed by open plans and not yet released.
    pub committed: f64,
    /// Recorded actual spend of closed plans.
    pub spent: f64,
    pub remaining: f64,
}

/// A journaled side effect whose outcome was never written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: i64,
    pub action: String,
    pub plan_id: Option<i64>,
    /// The intent as written before the side effect (JSON).
    pub intent: String,
}

// ---------------------------------------------------------------- handoffs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Dispatched,
    Running,
    Complete,
    Failed,
    TimedOut,
    InvalidOutput,
    OwnershipViolation,
    /// The runner finished its turns; the orchestrating agent judges the output.
    Review,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Dispatched => "dispatched",
            State::Running => "running",
            State::Complete => "complete",
            State::Failed => "failed",
            State::TimedOut => "timed_out",
            State::InvalidOutput => "invalid_output",
            State::OwnershipViolation => "ownership_violation",
            State::Review => "review",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => State::Pending,
            "dispatched" => State::Dispatched,
            "running" => State::Running,
            "complete" => State::Complete,
            "failed" => State::Failed,
            "timed_out" => State::TimedOut,
            "invalid_output" => State::InvalidOutput,
            "ownership_violation" => State::OwnershipViolation,
            "review" => State::Review,
            other => return Err(Error::Refused(format!("unknown handoff state {other:?}"))),
        })
    }

    /// The transition law, mirroring dogfood-swarm's `lib/state-machine.js`.
    pub fn allowed(self) -> &'static [State] {
        use State::*;
        match self {
            Pending => &[Dispatched],
            Dispatched => &[
                Running,
                Complete,
                Failed,
                TimedOut,
                InvalidOutput,
                OwnershipViolation,
            ],
            Running => &[
                Complete,
                Failed,
                TimedOut,
                InvalidOutput,
                OwnershipViolation,
                Review,
            ],
            // Approve, reject, or send back with feedback.
            Review => &[Complete, InvalidOutput, Dispatched],
            Complete => &[],
            Failed | TimedOut => &[Dispatched],
            InvalidOutput | OwnershipViolation => &[],
        }
    }

    /// Blocked states move only by override with a reason; they never auto-retry.
    pub fn blocked(self) -> bool {
        matches!(self, State::InvalidOutput | State::OwnershipViolation)
    }

    pub fn terminal(self) -> bool {
        self == State::Complete
    }

    pub fn in_flight(self) -> bool {
        matches!(self, State::Dispatched | State::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    pub id: i64,
    pub role_id: String,
    pub mission: String,
    pub acceptance: String,
    pub scope: Vec<String>,
    pub depends_on: Vec<i64>,
    pub state: State,
    pub attempts: i64,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub role_hash: Option<String>,
    pub prompt_hash: Option<String>,
    pub result_record: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Deterministic checks the runner evaluates on every turn's output.
    pub checks: Vec<Check>,
    /// The checks cover the acceptance check: passing them completes the handoff
    /// without review.
    pub accept_on_checks: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NewHandoff {
    pub role_id: String,
    pub mission: String,
    /// How to tell it is done: a test command or a checkable statement. Required.
    pub acceptance: String,
    /// Paths the handoff may change. Anything else is an ownership violation.
    pub scope: Vec<String>,
    pub depends_on: Vec<i64>,
    pub checks: Vec<Check>,
    /// Requires `checks`.
    pub accept_on_checks: bool,
}

const HANDOFF_COLS: &str = "id, role_id, mission, acceptance, scope, depends_on, state, attempts, branch, model, role_hash, prompt_hash, result_record, created_at, updated_at, checks, accept_on_checks";

fn handoff_row(r: &Row<'_>) -> rusqlite::Result<Handoff> {
    let scope: String = r.get(4)?;
    let deps: String = r.get(5)?;
    let state: String = r.get(6)?;
    Ok(Handoff {
        id: r.get(0)?,
        role_id: r.get(1)?,
        mission: r.get(2)?,
        acceptance: r.get(3)?,
        scope: serde_json::from_str(&scope).unwrap_or_default(),
        depends_on: serde_json::from_str(&deps).unwrap_or_default(),
        state: State::parse(&state).unwrap_or(State::Failed),
        attempts: r.get(7)?,
        branch: r.get(8)?,
        model: r.get(9)?,
        role_hash: r.get(10)?,
        prompt_hash: r.get(11)?,
        result_record: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        checks: serde_json::from_str(&r.get::<_, String>(15)?).unwrap_or_default(),
        accept_on_checks: r.get(16)?,
    })
}

/// The one liveness predicate. `status` reports with it and the reaper acts on it,
/// so what is shown as stale and what gets reaped are the same set by construction.
pub fn is_stale(h: &Handoff, now: i64, timeout_secs: i64) -> bool {
    h.state.in_flight() && now - h.updated_at > timeout_secs
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub id: i64,
    pub handoff_id: i64,
    /// 1 is the draft; each revision adds one.
    pub turn: i64,
    pub body: String,
    pub outcomes: Vec<Outcome>,
    pub model: String,
    pub tokens: Option<i64>,
    pub created_at: i64,
}

pub struct NewOutput<'a> {
    pub handoff_id: i64,
    pub turn: i64,
    pub body: &'a str,
    pub outcomes: &'a [Outcome],
    pub model: &'a str,
    pub tokens: Option<i64>,
}

// ---------------------------------------------------------------- the store

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        }
        let conn = Connection::open(path).map_err(db("opening the project database"))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(db("opening an in-memory database"))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        // The side-car and its watchdog share this file from two processes.
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;",
        )
        .map_err(db("setting pragmas"))?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db("reading the schema version"))?;
        if version > SCHEMA_VERSION {
            return Err(Error::Refused(format!(
                "the project database is schema v{version}; this offrig knows v{SCHEMA_VERSION}. Update offrig."
            )));
        }
        if version == 1 {
            // v1 -> v2: plans gain their clock; the jobs table is new (created below).
            conn.execute_batch(
                "ALTER TABLE plans ADD COLUMN committed_at INTEGER;
                 ALTER TABLE plans ADD COLUMN started_at INTEGER;",
            )
            .map_err(db("migrating the schema to v2"))?;
        }
        if (1..3).contains(&version) {
            // v2 -> v3: handoffs gain deterministic checks; the outputs table is new.
            for (col, ty) in [
                ("checks", "TEXT NOT NULL DEFAULT '[]'"),
                ("accept_on_checks", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                if table_lacks(&conn, "handoffs", col)? {
                    conn.execute_batch(&format!("ALTER TABLE handoffs ADD COLUMN {col} {ty};"))
                        .map_err(db("migrating the schema to v3"))?;
                }
            }
        }
        conn.execute_batch(SCHEMA)
            .map_err(db("creating the schema"))?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .map_err(db("writing the schema version"))?;
        Ok(Self { conn })
    }

    // ---- settings

    pub fn set_budget_cap(&self, usd: f64) -> Result<()> {
        if !(usd.is_finite() && usd >= 0.0) {
            return Err(Error::Refused(format!(
                "budget cap {usd} must be a non-negative amount"
            )));
        }
        self.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES ('budget_cap', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![usd.to_string()],
            )
            .map_err(db("setting the budget cap"))?;
        Ok(())
    }

    fn budget_cap(&self) -> Result<f64> {
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'budget_cap'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(db("reading the budget cap"))?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0.0))
    }

    // ---- records

    pub fn record(&self, r: NewRecord) -> Result<i64> {
        let kind = r
            .kind
            .ok_or_else(|| Error::Refused("a record needs a kind".into()))?;
        if r.body.trim().is_empty() {
            return Err(Error::Refused("a record needs a body".into()));
        }
        if r.author.trim().is_empty() {
            return Err(Error::Refused(
                "a record needs an author (who or what wrote it)".into(),
            ));
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a record write"))?;
        if let Some(old) = r.supersedes {
            let reason = r.reason.as_deref().map(str::trim).unwrap_or("");
            if reason.is_empty() {
                return Err(Error::Refused(format!(
                    "superseding record {old} needs a reason, so later readers know why it changed"
                )));
            }
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM records WHERE id = ?1",
                    params![old],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db("reading the superseded record"))?;
            match status.as_deref() {
                Some("active") => {}
                Some(s) => {
                    return Err(Error::Refused(format!(
                        "record {old} is {s}, not active; supersede the record that replaced it"
                    )));
                }
                None => return Err(Error::Refused(format!("no record {old} to supersede"))),
            }
        }
        tx.execute(
            "INSERT INTO records(kind, body, author, source, task_id, tags, supersedes_id, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                kind.as_str(),
                r.body.trim(),
                r.author.trim(),
                r.source,
                r.task_id,
                r.tags.join(" "),
                r.supersedes,
                r.reason,
                now_unix()
            ],
        )
        .map_err(db("inserting a record"))?;
        let id = tx.last_insert_rowid();
        if let Some(old) = r.supersedes {
            tx.execute(
                "UPDATE records SET status = 'superseded', superseded_by = ?1 WHERE id = ?2",
                params![id, old],
            )
            .map_err(db("marking the record superseded"))?;
        }
        tx.commit().map_err(db("committing a record write"))?;
        Ok(id)
    }

    pub fn withdraw(&self, id: i64, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return Err(Error::Refused("withdrawing a record needs a reason".into()));
        }
        let n = self
            .conn
            .execute(
                "UPDATE records SET status = 'withdrawn', reason = ?1 WHERE id = ?2 AND status = 'active'",
                params![reason.trim(), id],
            )
            .map_err(db("withdrawing a record"))?;
        if n == 0 {
            return Err(Error::Refused(format!("record {id} is not active")));
        }
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Record>> {
        self.conn
            .query_row(
                &format!("SELECT {RECORD_COLS} FROM records WHERE id = ?1"),
                params![id],
                record_row,
            )
            .optional()
            .map_err(db("reading a record"))
    }

    /// Every active record of a kind, oldest first (briefs and constraints are
    /// always injected whole).
    pub fn active(&self, kind: Kind) -> Result<Vec<Record>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT {RECORD_COLS} FROM records WHERE kind = ?1 AND status = 'active' ORDER BY id"
            ))
            .map_err(db("preparing an active-records read"))?;
        st.query_map(params![kind.as_str()], record_row)
            .map_err(db("reading active records"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("reading active records"))
    }

    /// Full-text search over active records, best BM25 match first.
    pub fn search(&self, q: &Query) -> Result<Vec<Record>> {
        let limit = if q.limit == 0 { 8 } else { q.limit.min(50) } as i64;
        let Some(fts) = fts_query(&q.text) else {
            return Ok(Vec::new());
        };
        let mut sql = format!(
            "SELECT {} FROM records_fts f JOIN records r ON r.id = f.rowid
             WHERE records_fts MATCH ?1 AND r.status = 'active'",
            RECORD_COLS
                .split(", ")
                .map(|c| format!("r.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        sql.push_str(" AND (?3 IS NULL OR r.kind = ?3) AND (?4 IS NULL OR r.task_id = ?4)");
        sql.push_str(" ORDER BY bm25(records_fts) LIMIT ?2");
        let mut st = self.conn.prepare(&sql).map_err(db("preparing a search"))?;
        let kind = q.kind.map(Kind::as_str);
        st.query_map(params![fts, limit, kind, q.task_id], record_row)
            .map_err(db("searching records"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("searching records"))
    }

    pub fn latest_checkpoint(&self, task_id: i64) -> Result<Option<Record>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {RECORD_COLS} FROM records
                     WHERE kind = 'checkpoint' AND task_id = ?1 AND status = 'active'
                     ORDER BY id DESC LIMIT 1"
                ),
                params![task_id],
                record_row,
            )
            .optional()
            .map_err(db("reading the latest checkpoint"))
    }

    // ---- plans, budget, journal

    pub fn budget(&self) -> Result<Budget> {
        let cap = self.budget_cap()?;
        let sum = |kind: &str| -> Result<f64> {
            self.conn
                .query_row(
                    "SELECT COALESCE(SUM(amount), 0) FROM ledger WHERE kind = ?1",
                    params![kind],
                    |r| r.get(0),
                )
                .map_err(db("summing the ledger"))
        };
        let (commits, actuals, releases) = (sum("commit")?, sum("actual")?, sum("release")?);
        // A closed plan releases its whole commit and records its actual spend.
        let committed = (commits - releases).max(0.0);
        let spent = actuals;
        Ok(Budget {
            cap,
            committed,
            spent,
            remaining: cap - committed - spent,
        })
    }

    /// A plan prices the worst case (max price x max hours) and is refused if that
    /// would exceed what the budget has left. Planning spends nothing.
    pub fn create_plan(&self, p: NewPlan) -> Result<Plan> {
        if !(p.max_hours > 0.0 && p.max_hours.is_finite()) {
            return Err(Error::Refused("max_hours must be a positive number".into()));
        }
        if !(p.max_price_hr > 0.0 && p.max_price_hr.is_finite()) {
            return Err(Error::Refused(
                "max_price_hr must be positive; read it from offrig_offers".into(),
            ));
        }
        let worst = (p.max_price_hr * p.max_hours * 100.0).ceil() / 100.0;
        let b = self.budget()?;
        if worst > b.remaining + 1e-9 {
            return Err(Error::Refused(format!(
                "worst case ${worst:.2} ({}h at ${:.2}/hr) exceeds the ${:.2} left of the ${:.2} budget; \
                 shorten max_hours, pick a cheaper profile, or raise the cap",
                p.max_hours, p.max_price_hr, b.remaining, b.cap
            )));
        }
        self.conn
            .execute(
                "INSERT INTO plans(profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, created_at, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    p.profile,
                    p.gpu_count,
                    serde_json::to_string(&p.gpu_types).unwrap_or_else(|_| "[]".into()),
                    p.max_hours,
                    p.max_price_hr,
                    worst,
                    now_unix(),
                    p.note
                ],
            )
            .map_err(db("saving a plan"))?;
        let id = self.conn.last_insert_rowid();
        self.plan(id)?
            .ok_or_else(|| Error::Refused("plan vanished after insert".into()))
    }

    pub fn plan(&self, id: i64) -> Result<Option<Plan>> {
        self.conn
            .query_row(
                "SELECT id, profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, state, pod_id, created_at, note,
                        committed_at, started_at
                 FROM plans WHERE id = ?1",
                params![id],
                |r| {
                    let types: String = r.get(3)?;
                    Ok(Plan {
                        id: r.get(0)?,
                        profile: r.get(1)?,
                        gpu_count: r.get(2)?,
                        gpu_types: serde_json::from_str(&types).unwrap_or_default(),
                        max_hours: r.get(4)?,
                        max_price_hr: r.get(5)?,
                        worst_case: r.get(6)?,
                        state: r.get(7)?,
                        pod_id: r.get(8)?,
                        created_at: r.get(9)?,
                        note: r.get(10)?,
                        committed_at: r.get(11)?,
                        started_at: r.get(12)?,
                    })
                },
            )
            .optional()
            .map_err(db("reading a plan"))
    }

    /// Commit a plan's worst case against the budget, immediately before renting.
    /// Re-checks the budget, because other plans may have committed since.
    /// Idempotent: committing a committed plan returns it unchanged.
    pub fn commit_plan(&self, id: i64) -> Result<Plan> {
        let plan = self
            .plan(id)?
            .ok_or_else(|| Error::Refused(format!("no plan {id}; make one with offrig_plan")))?;
        match plan.state.as_str() {
            "committed" => return Ok(plan),
            "planned" => {}
            s => return Err(Error::Refused(format!("plan {id} is {s}; make a new plan"))),
        }
        let b = self.budget()?;
        if plan.worst_case > b.remaining + 1e-9 {
            return Err(Error::Refused(format!(
                "plan {id} needs ${:.2} but only ${:.2} of the budget is left",
                plan.worst_case, b.remaining
            )));
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a plan commit"))?;
        tx.execute(
            "INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (?1, 'commit', ?2, ?3, 'worst case')",
            params![id, plan.worst_case, now_unix()],
        )
        .map_err(db("writing a commit"))?;
        tx.execute(
            "UPDATE plans SET state = 'committed', committed_at = ?2 WHERE id = ?1",
            params![id, now_unix()],
        )
        .map_err(db("marking the plan committed"))?;
        tx.commit().map_err(db("committing a plan"))?;
        self.plan(id)?
            .ok_or_else(|| Error::Refused("plan vanished after commit".into()))
    }

    /// Record the pod a plan rented and when, so cost and the deadline can be
    /// computed later by anyone (the side-car, the watchdog, a restarted session).
    pub fn attach_pod(&self, plan_id: i64, pod_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE plans SET pod_id = ?1, started_at = COALESCE(started_at, ?3) WHERE id = ?2",
                params![pod_id, plan_id, now_unix()],
            )
            .map_err(db("attaching a pod to a plan"))?;
        Ok(())
    }

    /// Committed plans: the ones that may be renting right now.
    pub fn open_plans(&self) -> Result<Vec<Plan>> {
        let ids: Vec<i64> = {
            let mut st = self
                .conn
                .prepare("SELECT id FROM plans WHERE state = 'committed' ORDER BY id")
                .map_err(db("preparing an open-plan read"))?;
            st.query_map([], |r| r.get(0))
                .map_err(db("reading open plans"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(db("reading open plans"))?
        };
        ids.into_iter()
            .filter_map(|id| self.plan(id).transpose())
            .collect()
    }

    // ---- settings (small key/value facts, e.g. watchdog heartbeats)

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(db("writing a setting"))?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(db("reading a setting"))
    }

    // ---- lanes (which project lane a plan's pod lives in)

    /// The lane a plan recorded (`offrig_plan` or the launch writes it). `None` is the
    /// plain lane: every plan made before lanes existed. Kept in settings, not a plans
    /// column, so the schema stays v3 and an older offrig or a watchdog still running
    /// from one keeps reading this database.
    pub fn plan_lane(&self, plan_id: i64) -> Result<Option<String>> {
        self.setting(&format!("plan_lane:{plan_id}"))
    }

    pub fn set_plan_lane(&self, plan_id: i64, tag: &str) -> Result<()> {
        self.set_setting(&format!("plan_lane:{plan_id}"), tag)
    }

    /// The host CUDA floor a plan was made with (`offrig_plan` writes it from the
    /// profile). `None` for a plan made without one, and for plans from before this
    /// existed: the launch then uses the profile's own. Kept in settings for the same
    /// reason as the lane: the schema stays v3.
    pub fn plan_min_cuda(&self, plan_id: i64) -> Result<Option<String>> {
        self.setting(&format!("plan_min_cuda:{plan_id}"))
    }

    pub fn set_plan_min_cuda(&self, plan_id: i64, version: &str) -> Result<()> {
        self.set_setting(&format!("plan_min_cuda:{plan_id}"), version)
    }

    /// How long the plan's launch may wait for capacity, in minutes (`offrig_plan`
    /// `wait_minutes`). `None` means the profile's `wait_for_gpu_minutes`. Kept in
    /// settings like the lane and the CUDA floor, so the schema stays v3.
    pub fn plan_wait_minutes(&self, plan_id: i64) -> Result<Option<u32>> {
        Ok(self
            .setting(&format!("plan_wait_minutes:{plan_id}"))?
            .and_then(|v| v.parse().ok()))
    }

    pub fn set_plan_wait_minutes(&self, plan_id: i64, minutes: u32) -> Result<()> {
        self.set_setting(
            &format!("plan_wait_minutes:{plan_id}"),
            &minutes.to_string(),
        )
    }

    /// The container disk size (GB) a plan asked for, overriding the profile's
    /// `container_disk_gb` (`offrig_plan` `container_disk_gb`). `None` means the profile's.
    /// Kept in settings like the lane and the CUDA floor, so the schema stays v3.
    pub fn plan_container_disk_gb(&self, plan_id: i64) -> Result<Option<u32>> {
        Ok(self
            .setting(&format!("plan_container_disk_gb:{plan_id}"))?
            .and_then(|v| v.parse().ok()))
    }

    pub fn set_plan_container_disk_gb(&self, plan_id: i64, gb: u32) -> Result<()> {
        self.set_setting(
            &format!("plan_container_disk_gb:{plan_id}"),
            &gb.to_string(),
        )
    }

    // ---- jobs (long work run in the background; state survives restarts)

    pub fn create_job(&self, plan_id: i64, kind: &str) -> Result<i64> {
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO jobs(plan_id, kind, state, started_at, updated_at) VALUES (?1, ?2, 'running', ?3, ?3)",
                params![plan_id, kind, now],
            )
            .map_err(db("creating a job"))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_job(
        &self,
        id: i64,
        state: &str,
        progress: &serde_json::Value,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE jobs SET state = ?1, progress = ?2, error = ?3, updated_at = ?4 WHERE id = ?5",
                params![state, progress.to_string(), error, now_unix(), id],
            )
            .map_err(db("updating a job"))?;
        Ok(())
    }

    fn job_row(r: &Row<'_>) -> rusqlite::Result<Job> {
        let progress: String = r.get(4)?;
        Ok(Job {
            id: r.get(0)?,
            plan_id: r.get(1)?,
            kind: r.get(2)?,
            state: r.get(3)?,
            progress: serde_json::from_str(&progress).unwrap_or(serde_json::Value::Null),
            error: r.get(5)?,
            started_at: r.get(6)?,
            updated_at: r.get(7)?,
        })
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        self.conn
            .query_row(
                "SELECT id, plan_id, kind, state, progress, error, started_at, updated_at FROM jobs WHERE id = ?1",
                params![id],
                Self::job_row,
            )
            .optional()
            .map_err(db("reading a job"))
    }

    /// The newest job for a plan: how a retried launch finds the one already running.
    pub fn job_for_plan(&self, plan_id: i64, kind: &str) -> Result<Option<Job>> {
        self.conn
            .query_row(
                "SELECT id, plan_id, kind, state, progress, error, started_at, updated_at
                 FROM jobs WHERE plan_id = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1",
                params![plan_id, kind],
                Self::job_row,
            )
            .optional()
            .map_err(db("reading a plan's job"))
    }

    /// Close a plan with its actual spend: release the whole commit, record actual.
    pub fn close_plan(&self, id: i64, actual: f64) -> Result<Budget> {
        let plan = self
            .plan(id)?
            .ok_or_else(|| Error::Refused(format!("no plan {id}")))?;
        if plan.state == "closed" || plan.state == "cancelled" {
            return self.budget();
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a plan close"))?;
        let now = now_unix();
        if plan.state == "committed" {
            tx.execute(
                "INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (?1, 'release', ?2, ?3, 'plan closed')",
                params![id, plan.worst_case, now],
            )
            .map_err(db("writing a release"))?;
        }
        tx.execute(
            "INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (?1, 'actual', ?2, ?3, 'measured')",
            params![id, actual.max(0.0), now],
        )
        .map_err(db("writing actual spend"))?;
        let state = if plan.state == "committed" {
            "closed"
        } else {
            "cancelled"
        };
        tx.execute(
            "UPDATE plans SET state = ?1 WHERE id = ?2",
            params![state, id],
        )
        .map_err(db("closing the plan"))?;
        tx.commit().map_err(db("committing a plan close"))?;
        self.budget()
    }

    /// Write intent before a side effect; fill in the outcome after.
    pub fn journal(
        &self,
        action: &str,
        plan_id: Option<i64>,
        intent: &serde_json::Value,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO journal(action, plan_id, intent, at) VALUES (?1, ?2, ?3, ?4)",
                params![action, plan_id, intent.to_string(), now_unix()],
            )
            .map_err(db("writing the journal"))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn journal_outcome(&self, id: i64, outcome: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE journal SET outcome = ?1 WHERE id = ?2",
                params![outcome, id],
            )
            .map_err(db("writing a journal outcome"))?;
        Ok(())
    }

    /// Journal entries with no outcome: side effects that may have happened while
    /// offrig was not watching. Reconcile reads these on start.
    pub fn unfinished_journal(&self) -> Result<Vec<JournalEntry>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, action, plan_id, intent FROM journal WHERE outcome IS NULL ORDER BY id",
            )
            .map_err(db("preparing a journal read"))?;
        st.query_map([], |r| {
            Ok(JournalEntry {
                id: r.get(0)?,
                action: r.get(1)?,
                plan_id: r.get(2)?,
                intent: r.get(3)?,
            })
        })
        .map_err(db("reading the journal"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db("reading the journal"))
    }

    // ---- handoffs

    pub fn add_handoff(&self, h: NewHandoff) -> Result<i64> {
        if h.role_id.trim().is_empty() || h.mission.trim().is_empty() {
            return Err(Error::Refused(
                "a handoff needs a role and a mission".into(),
            ));
        }
        if h.acceptance.trim().is_empty() {
            return Err(Error::Refused(
                "a handoff needs an acceptance check (a test command or a checkable statement); \
                 a role does not make a model correct, the check does"
                    .into(),
            ));
        }
        if h.accept_on_checks && h.checks.is_empty() {
            return Err(Error::Refused(
                "accept_on_checks needs checks: without them nothing can be accepted by code"
                    .into(),
            ));
        }
        for d in &h.depends_on {
            if self.handoff(*d)?.is_none() {
                return Err(Error::Refused(format!(
                    "depends_on names handoff {d}, which does not exist"
                )));
            }
        }
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO handoffs(role_id, mission, acceptance, scope, depends_on, created_at, updated_at, checks, accept_on_checks)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
                params![
                    h.role_id.trim(),
                    h.mission.trim(),
                    h.acceptance.trim(),
                    serde_json::to_string(&h.scope).unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&h.depends_on).unwrap_or_else(|_| "[]".into()),
                    now,
                    serde_json::to_string(&h.checks).unwrap_or_else(|_| "[]".into()),
                    h.accept_on_checks
                ],
            )
            .map_err(db("adding a handoff"))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn handoff(&self, id: i64) -> Result<Option<Handoff>> {
        self.conn
            .query_row(
                &format!("SELECT {HANDOFF_COLS} FROM handoffs WHERE id = ?1"),
                params![id],
                handoff_row,
            )
            .optional()
            .map_err(db("reading a handoff"))
    }

    pub fn handoffs(&self) -> Result<Vec<Handoff>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {HANDOFF_COLS} FROM handoffs ORDER BY id"))
            .map_err(db("preparing a handoff list"))?;
        st.query_map([], handoff_row)
            .map_err(db("listing handoffs"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("listing handoffs"))
    }

    /// Pending handoffs whose dependencies are all complete, in queue order.
    pub fn ready(&self) -> Result<Vec<Handoff>> {
        let all = self.handoffs()?;
        let done = |id: &i64| {
            all.iter()
                .any(|h| h.id == *id && h.state == State::Complete)
        };
        Ok(all
            .iter()
            .filter(|h| h.state == State::Pending && h.depends_on.iter().all(done))
            .cloned()
            .collect())
    }

    /// Move a handoff through the transition law. Blocked states move only with
    /// `override_reason`; every move is logged with its reason.
    pub fn transition(
        &self,
        id: i64,
        to: State,
        reason: &str,
        override_reason: Option<&str>,
    ) -> Result<Handoff> {
        let h = self
            .handoff(id)?
            .ok_or_else(|| Error::Refused(format!("no handoff {id}")))?;
        let from = h.state;
        let lawful = from.allowed().contains(&to);
        let overridden = override_reason.is_some_and(|r| !r.trim().is_empty());
        if !lawful && !(from.blocked() && overridden) {
            let hint = if from.blocked() {
                " (blocked: pass an override reason to move it)".to_string()
            } else if from.terminal() {
                " (complete is terminal)".to_string()
            } else {
                format!(
                    " (allowed: {})",
                    from.allowed()
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            return Err(Error::Transition {
                id,
                from: from.as_str().into(),
                to: to.as_str().into(),
                hint,
            });
        }
        let why = if overridden && !lawful {
            format!("OVERRIDE: {}", override_reason.unwrap_or_default().trim())
        } else {
            reason.trim().to_string()
        };
        if why.is_empty() {
            return Err(Error::Refused("a transition needs a reason".into()));
        }
        let now = now_unix();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a transition"))?;
        let bump = i64::from(to == State::Dispatched);
        tx.execute(
            "UPDATE handoffs SET state = ?1, updated_at = ?2, attempts = attempts + ?3 WHERE id = ?4",
            params![to.as_str(), now, bump, id],
        )
        .map_err(db("updating a handoff"))?;
        tx.execute(
            "INSERT INTO handoff_events(handoff_id, from_state, to_state, reason, override, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                from.as_str(),
                to.as_str(),
                why,
                overridden && !lawful,
                now
            ],
        )
        .map_err(db("logging a transition"))?;
        tx.commit().map_err(db("committing a transition"))?;
        self.handoff(id)?
            .ok_or_else(|| Error::Refused("handoff vanished after transition".into()))
    }

    /// Record that an in-flight handoff is alive (its runner reported progress).
    pub fn heartbeat(&self, id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE handoffs SET updated_at = ?1 WHERE id = ?2 AND state IN ('dispatched','running')",
                params![now_unix(), id],
            )
            .map_err(db("recording a heartbeat"))?;
        Ok(())
    }

    pub fn set_run_details(
        &self,
        id: i64,
        model: &str,
        role_hash: &str,
        prompt_hash: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE handoffs SET model = ?1, role_hash = ?2, prompt_hash = ?3 WHERE id = ?4",
                params![model, role_hash, prompt_hash, id],
            )
            .map_err(db("recording run details"))?;
        Ok(())
    }

    /// Time out every stale in-flight handoff, using `is_stale`. Returns their ids.
    pub fn reap_stale(&self, now: i64, timeout_secs: i64) -> Result<Vec<i64>> {
        let stale: Vec<i64> = self
            .handoffs()?
            .into_iter()
            .filter(|h| is_stale(h, now, timeout_secs))
            .map(|h| h.id)
            .collect();
        for id in &stale {
            self.transition(
                *id,
                State::TimedOut,
                &format!("no progress for over {timeout_secs}s"),
                None,
            )?;
        }
        Ok(stale)
    }

    /// Store one turn's output with its check outcomes. Returns the output id.
    pub fn add_output(&self, o: NewOutput<'_>) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO outputs(handoff_id, turn, body, outcomes, model, tokens, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    o.handoff_id,
                    o.turn,
                    o.body,
                    serde_json::to_string(o.outcomes).unwrap_or_else(|_| "[]".into()),
                    o.model,
                    o.tokens,
                    now_unix()
                ],
            )
            .map_err(db("storing an output"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every stored turn of a handoff, oldest first.
    pub fn outputs(&self, handoff_id: i64) -> Result<Vec<Output>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, handoff_id, turn, body, outcomes, model, tokens, created_at
                 FROM outputs WHERE handoff_id = ?1 ORDER BY id",
            )
            .map_err(db("preparing an output read"))?;
        st.query_map(params![handoff_id], |r| {
            Ok(Output {
                id: r.get(0)?,
                handoff_id: r.get(1)?,
                turn: r.get(2)?,
                body: r.get(3)?,
                outcomes: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
                model: r.get(5)?,
                tokens: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(db("reading outputs"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db("reading outputs"))
    }

    /// The latest output of a handoff: its result once it is complete or in review.
    pub fn latest_output(&self, handoff_id: i64) -> Result<Option<Output>> {
        Ok(self.outputs(handoff_id)?.pop())
    }

    pub fn events(&self, handoff_id: i64) -> Result<Vec<(String, String, String, bool)>> {
        let mut st = self
            .conn
            .prepare("SELECT from_state, to_state, reason, override FROM handoff_events WHERE handoff_id = ?1 ORDER BY id")
            .map_err(db("preparing an event read"))?;
        st.query_map(params![handoff_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(db("reading events"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db("reading events"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    fn rec(kind: Kind, body: &str) -> NewRecord {
        NewRecord {
            kind: Some(kind),
            body: body.into(),
            author: "test".into(),
            ..Default::default()
        }
    }

    #[test]
    fn records_need_kind_body_and_author() {
        let s = store();
        assert!(
            s.record(NewRecord {
                body: "x".into(),
                author: "a".into(),
                ..Default::default()
            })
            .is_err()
        );
        assert!(s.record(rec(Kind::Fact, "  ")).is_err());
        assert!(
            s.record(NewRecord {
                author: " ".into(),
                ..rec(Kind::Fact, "x")
            })
            .is_err()
        );
        assert!(
            s.record(rec(Kind::Fact, "ratatui 0.29 removed Frame::size"))
                .is_ok()
        );
    }

    #[test]
    fn supersession_needs_a_reason_and_hides_the_old_record() {
        let s = store();
        let old = s
            .record(rec(Kind::Decision, "combat uses a speed stat"))
            .expect("insert");
        let no_reason = NewRecord {
            supersedes: Some(old),
            ..rec(Kind::Decision, "combat uses initiative bands")
        };
        assert!(s.record(no_reason).is_err(), "a reason is required");
        let new = s
            .record(NewRecord {
                supersedes: Some(old),
                reason: Some("bands read better at a glance".into()),
                ..rec(Kind::Decision, "combat uses initiative bands")
            })
            .expect("supersede");
        let o = s.get(old).expect("read").expect("exists");
        assert_eq!(o.status, "superseded");
        assert_eq!(o.superseded_by, Some(new));
        let hits = s
            .search(&Query {
                text: "combat".into(),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(
            hits.iter().map(|r| r.id).collect::<Vec<_>>(),
            [new],
            "only the active decision"
        );
        let again = NewRecord {
            supersedes: Some(old),
            reason: Some("r".into()),
            ..rec(Kind::Decision, "x")
        };
        assert!(
            s.record(again).is_err(),
            "cannot supersede a superseded record"
        );
    }

    #[test]
    fn withdrawn_records_drop_out_of_search_and_active() {
        let s = store();
        let c = s
            .record(rec(Kind::Constraint, "never touch the solver crate"))
            .expect("insert");
        assert_eq!(s.active(Kind::Constraint).expect("read").len(), 1);
        assert!(s.withdraw(c, "").is_err());
        s.withdraw(c, "solver is open for work now")
            .expect("withdraw");
        assert!(s.active(Kind::Constraint).expect("read").is_empty());
        assert!(
            s.search(&Query {
                text: "solver".into(),
                ..Default::default()
            })
            .expect("search")
            .is_empty()
        );
    }

    #[test]
    fn search_filters_by_kind_and_task_and_survives_fts_syntax() {
        let s = store();
        s.record(NewRecord {
            task_id: Some(1),
            ..rec(Kind::Fact, "the parser rejects tabs")
        })
        .expect("insert");
        s.record(NewRecord {
            task_id: Some(2),
            ..rec(Kind::Decision, "the parser keeps tabs")
        })
        .expect("insert");
        let facts = s
            .search(&Query {
                text: "parser".into(),
                kind: Some(Kind::Fact),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(facts.len(), 1);
        let t2 = s
            .search(&Query {
                text: "parser".into(),
                task_id: Some(2),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(t2.len(), 1);
        for nasty in ["\"", "AND OR NOT", "a*", "(tabs", "NEAR(x y)", ""] {
            s.search(&Query {
                text: nasty.into(),
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{nasty:?}: {e}"));
        }
    }

    #[test]
    fn plans_are_priced_at_worst_case_and_held_to_the_cap() {
        let s = store();
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "frontier".into(),
                gpu_count: 4,
                gpu_types: vec!["NVIDIA RTX PRO 6000 Blackwell Server Edition".into()],
                max_hours: 1.5,
                max_price_hr: 8.36,
                note: None,
            })
            .expect("fits");
        assert!((p.worst_case - 12.54).abs() < 1e-9);
        assert_eq!(p.state, "planned");
        assert!(
            (s.budget().expect("b").remaining - 15.0).abs() < 1e-9,
            "planning spends nothing"
        );
        let too_big = NewPlan {
            max_hours: 2.0,
            ..NewPlan {
                profile: "frontier".into(),
                gpu_count: 4,
                gpu_types: vec![],
                max_hours: 2.0,
                max_price_hr: 8.36,
                note: None,
            }
        };
        let err = s.create_plan(too_big).expect_err("16.72 > 15");
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[test]
    fn commit_is_idempotent_and_close_releases_down_to_actual_spend() {
        let s = store();
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "medium".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 3.0,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        s.commit_plan(p.id).expect("commit again is a no-op");
        let b = s.budget().expect("b");
        assert!((b.committed - 6.27).abs() < 1e-9, "{b:?}");
        assert!((b.remaining - 8.73).abs() < 1e-9, "{b:?}");
        // A second plan cannot spend the committed part.
        let second = s.create_plan(NewPlan {
            profile: "medium".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 5.0,
            max_price_hr: 2.09,
            note: None,
        });
        assert!(second.is_err(), "10.45 > 8.73 left");
        let after = s.close_plan(p.id, 1.10).expect("close");
        assert!((after.spent - 1.10).abs() < 1e-9);
        assert!((after.committed).abs() < 1e-9);
        assert!((after.remaining - 13.90).abs() < 1e-9, "{after:?}");
        assert!(
            s.commit_plan(p.id).is_err(),
            "a closed plan cannot be recommitted"
        );
    }

    #[test]
    fn journal_tracks_unfinished_side_effects() {
        let s = store();
        let j = s
            .journal(
                "create_pod",
                Some(1),
                &serde_json::json!({"profile": "frontier"}),
            )
            .expect("journal");
        assert_eq!(s.unfinished_journal().expect("read").len(), 1);
        s.journal_outcome(j, "created pod abc").expect("outcome");
        assert!(s.unfinished_journal().expect("read").is_empty());
    }

    fn hand(s: &Store, deps: Vec<i64>) -> i64 {
        s.add_handoff(NewHandoff {
            role_id: "builder".into(),
            mission: "add a parser test".into(),
            acceptance: "cargo test parser".into(),
            scope: vec!["src/parser.rs".into()],
            depends_on: deps,
            ..Default::default()
        })
        .expect("add")
    }

    #[test]
    fn handoffs_need_an_acceptance_check_and_real_dependencies() {
        let s = store();
        let no_check = NewHandoff {
            role_id: "builder".into(),
            mission: "m".into(),
            ..Default::default()
        };
        assert!(s.add_handoff(no_check).is_err());
        let bad_dep = NewHandoff {
            role_id: "builder".into(),
            mission: "m".into(),
            acceptance: "a".into(),
            depends_on: vec![99],
            ..Default::default()
        };
        assert!(s.add_handoff(bad_dep).is_err());
    }

    #[test]
    fn ready_respects_dependencies() {
        let s = store();
        let a = hand(&s, vec![]);
        let b = hand(&s, vec![a]);
        assert_eq!(
            s.ready()
                .expect("ready")
                .iter()
                .map(|h| h.id)
                .collect::<Vec<_>>(),
            [a]
        );
        s.transition(a, State::Dispatched, "start", None)
            .expect("dispatch");
        s.transition(a, State::Complete, "tests pass", None)
            .expect("complete");
        assert_eq!(
            s.ready()
                .expect("ready")
                .iter()
                .map(|h| h.id)
                .collect::<Vec<_>>(),
            [b]
        );
    }

    #[test]
    fn transition_law_matches_the_swarm_control_plane() {
        let s = store();
        let h = hand(&s, vec![]);
        assert!(
            s.transition(h, State::Complete, "skip", None).is_err(),
            "pending cannot complete"
        );
        s.transition(h, State::Dispatched, "start", None)
            .expect("dispatch");
        s.transition(h, State::Running, "runner picked it up", None)
            .expect("run");
        s.transition(h, State::InvalidOutput, "acceptance check failed", None)
            .expect("block");
        let err = s
            .transition(h, State::Dispatched, "retry", None)
            .expect_err("blocked");
        assert!(err.to_string().contains("override"), "{err}");
        assert!(
            s.transition(h, State::Dispatched, "retry", Some("  "))
                .is_err(),
            "blank override is no override"
        );
        let moved = s
            .transition(
                h,
                State::Dispatched,
                "retry",
                Some("check was wrong; fixed it"),
            )
            .expect("override");
        assert_eq!(moved.state, State::Dispatched);
        assert_eq!(moved.attempts, 2);
        let ev = s.events(h).expect("events");
        assert_eq!(ev.len(), 4);
        assert!(ev[3].2.starts_with("OVERRIDE:") && ev[3].3, "{ev:?}");
        s.transition(h, State::Complete, "done", None)
            .expect("complete");
        assert!(
            s.transition(h, State::Dispatched, "again", Some("x"))
                .is_err(),
            "complete is terminal"
        );
    }

    #[test]
    fn status_and_reaper_share_one_liveness_predicate() {
        let s = store();
        let h = hand(&s, vec![]);
        s.transition(h, State::Dispatched, "start", None)
            .expect("dispatch");
        let row = s.handoff(h).expect("read").expect("exists");
        let later = row.updated_at + 1801;
        assert!(!is_stale(&row, row.updated_at + 10, 1800));
        assert!(is_stale(&row, later, 1800));
        assert_eq!(s.reap_stale(later, 1800).expect("reap"), [h]);
        assert_eq!(
            s.handoff(h).expect("read").expect("exists").state,
            State::TimedOut
        );
        let pending = hand(&s, vec![]);
        let p = s.handoff(pending).expect("read").expect("exists");
        assert!(
            !is_stale(&p, later + 99_999, 1800),
            "pending work is not in flight"
        );
    }

    #[test]
    fn a_plan_remembers_its_cuda_floor_and_its_gpu_list() {
        let s = store();
        s.set_budget_cap(50.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec!["NVIDIA RTX PRO 6000 Blackwell Server Edition".into()],
                max_hours: 3.5,
                max_price_hr: 2.09,
                note: None,
            })
            .expect("plan");
        assert_eq!(s.plan_min_cuda(p.id).expect("read"), None, "none until set");
        s.set_plan_min_cuda(p.id, "13.0").expect("set");
        assert_eq!(
            s.plan_min_cuda(p.id).expect("read").as_deref(),
            Some("13.0")
        );
        assert_eq!(s.plan_min_cuda(p.id + 1).expect("read"), None, "per plan");
        assert_eq!(
            s.plan_wait_minutes(p.id).expect("read"),
            None,
            "none until set"
        );
        s.set_plan_wait_minutes(p.id, 20).expect("set");
        assert_eq!(s.plan_wait_minutes(p.id).expect("read"), Some(20));
        assert_eq!(
            s.plan_wait_minutes(p.id + 1).expect("read"),
            None,
            "per plan"
        );
        assert_eq!(s.plan_container_disk_gb(p.id).expect("read"), None);
        s.set_plan_container_disk_gb(p.id, 200).expect("set");
        assert_eq!(s.plan_container_disk_gb(p.id).expect("read"), Some(200));
        assert_eq!(s.plan_container_disk_gb(p.id + 1).expect("read"), None);
        let back = s.plan(p.id).expect("read").expect("plan");
        assert_eq!(back.gpu_types, p.gpu_types);
        assert_eq!(back.worst_case, 7.32, "3.5 h at the plan's own 2.09");
    }

    #[test]
    fn v1_databases_migrate_to_v2_without_losing_rows() {
        let dir = std::env::temp_dir().join(format!("offrig-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("v1.db");
        {
            // A v1 plans table, as phase 1 wrote it.
            let c = Connection::open(&path).expect("open");
            c.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY, profile TEXT NOT NULL, gpu_count INTEGER NOT NULL,
                   gpu_types TEXT NOT NULL, max_hours REAL NOT NULL, max_price_hr REAL NOT NULL, worst_case REAL NOT NULL,
                   state TEXT NOT NULL DEFAULT 'planned', pod_id TEXT, created_at INTEGER NOT NULL, note TEXT);
                 INSERT INTO plans(profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, created_at)
                   VALUES ('medium', 1, '[]', 2.0, 2.09, 4.18, 1);
                 PRAGMA user_version = 1;",
            )
            .expect("v1 schema");
        }
        let s = Store::open(&path).expect("migrates");
        let p = s.plan(1).expect("read").expect("kept");
        assert_eq!(p.profile, "medium");
        assert_eq!(p.committed_at, None);
        assert!(
            s.create_job(1, "launch").is_ok(),
            "jobs table exists after migration"
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_handoffs_migrate_to_v3_with_checks_and_outputs() {
        let dir = std::env::temp_dir().join(format!("offrig-migrate3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("v2.db");
        {
            let c = Connection::open(&path).expect("open");
            c.execute_batch(
                "CREATE TABLE handoffs (id INTEGER PRIMARY KEY, role_id TEXT NOT NULL, mission TEXT NOT NULL,
                   acceptance TEXT NOT NULL, scope TEXT NOT NULL DEFAULT '[]', depends_on TEXT NOT NULL DEFAULT '[]',
                   state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0, branch TEXT, model TEXT,
                   role_hash TEXT, prompt_hash TEXT, result_record INTEGER, created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL);
                 INSERT INTO handoffs(role_id, mission, acceptance, created_at, updated_at)
                   VALUES ('game-designer', 'duel verbs', 'three verbs', 1, 1);
                 PRAGMA user_version = 2;",
            )
            .expect("v2 schema");
        }
        let s = Store::open(&path).expect("migrates");
        let h = s.handoff(1).expect("read").expect("kept");
        assert_eq!(h.mission, "duel verbs");
        assert!(h.checks.is_empty() && !h.accept_on_checks);
        let id = s
            .add_output(NewOutput {
                handoff_id: 1,
                turn: 1,
                body: "draft",
                outcomes: &[],
                model: "qwen3:4b",
                tokens: Some(12),
            })
            .expect("outputs table exists");
        assert_eq!(s.latest_output(1).expect("read").expect("one").id, id);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_closes_by_approval_rejection_or_feedback() {
        let s = store();
        let h = s
            .add_handoff(NewHandoff {
                role_id: "game-designer".into(),
                mission: "duel verbs".into(),
                acceptance: "three verbs".into(),
                checks: vec![Check::Heading {
                    text: "Verbs".into(),
                }],
                ..Default::default()
            })
            .expect("add");
        for (to, why) in [
            (State::Dispatched, "runner"),
            (State::Running, "drafting"),
            (State::Review, "turns done"),
        ] {
            s.transition(h, to, why, None).expect("lawful");
        }
        let row = s.handoff(h).expect("read").expect("exists");
        assert!(!row.state.in_flight() && !row.state.blocked() && !row.state.terminal());
        assert_eq!(row.checks.len(), 1);
        let back = s
            .transition(h, State::Dispatched, "add a failure state", None)
            .expect("feedback");
        assert_eq!(back.attempts, 2, "a send-back is a new attempt");
        assert!(
            s.add_handoff(NewHandoff {
                role_id: "x".into(),
                mission: "m".into(),
                acceptance: "a".into(),
                accept_on_checks: true,
                ..Default::default()
            })
            .is_err(),
            "accept_on_checks without checks is refused"
        );
    }

    #[test]
    fn committing_starts_the_clock_and_jobs_track_progress() {
        let s = store();
        s.set_budget_cap(15.0).expect("cap");
        let p = s
            .create_plan(NewPlan {
                profile: "small".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 2.0,
                max_price_hr: 0.25,
                note: None,
            })
            .expect("plan");
        assert_eq!(p.deadline(), None, "no clock until committed");
        let c = s.commit_plan(p.id).expect("commit");
        let start = c.committed_at.expect("clock started");
        assert_eq!(c.deadline(), Some(start + 7200));
        s.attach_pod(p.id, "pod1").expect("attach");
        assert_eq!(
            s.plan(p.id).expect("r").expect("e").pod_id.as_deref(),
            Some("pod1")
        );
        assert_eq!(s.open_plans().expect("open").len(), 1);
        let j = s.create_job(p.id, "launch").expect("job");
        s.update_job(
            j,
            "running",
            &serde_json::json!({"step": "waiting for GPUs"}),
            None,
        )
        .expect("update");
        let job = s
            .job_for_plan(p.id, "launch")
            .expect("read")
            .expect("exists");
        assert_eq!(job.id, j);
        assert_eq!(job.progress["step"], "waiting for GPUs");
        s.close_plan(p.id, 0.1).expect("close");
        assert!(s.open_plans().expect("open").is_empty());
    }

    #[test]
    fn database_on_disk_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("offrig-store-{}", std::process::id()));
        let path = dir.join(".offrig").join("offrig.db");
        {
            let s = Store::open(&path).expect("open");
            s.set_budget_cap(15.0).expect("cap");
            s.record(rec(Kind::Brief, "Saint's Mile is a frontier JRPG in Rust"))
                .expect("insert");
        }
        let s = Store::open(&path).expect("reopen");
        assert_eq!(s.active(Kind::Brief).expect("read").len(), 1);
        assert!((s.budget().expect("b").cap - 15.0).abs() < 1e-9);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
