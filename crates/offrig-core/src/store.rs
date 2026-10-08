//! The project database: memory records, the budget ledger, plans, the action journal
//! and the handoff queue. One SQLite file per project; it outlives every pod.
//!
//! Rules this module enforces (see docs/sidecar-design.md for the evidence):
//! - records are never edited or deleted; a new record supersedes an old one through
//!   an explicit edge with a reason, and search returns active records only;
//! - spend is committed against a hard cap before a launch, from price x max hours;
//! - every handoff state change goes through one transition law and is logged;
//! - liveness is computed by one predicate that status and the reaper both use.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::checks::{Check, Outcome};
use crate::cost::now_unix;
use crate::error::{Error, Result};
use crate::verify::{CheckType, Verdict, VerdictKind};

const SCHEMA_VERSION: i64 = 6;

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

CREATE TABLE IF NOT EXISTS completions (
  id            INTEGER PRIMARY KEY,
  model         TEXT NOT NULL,
  lane          TEXT NOT NULL,
  input_bound   INTEGER NOT NULL,
  max_tokens    INTEGER NOT NULL,
  price_in_m    REAL NOT NULL,
  price_out_m   REAL NOT NULL,
  worst_case    REAL NOT NULL,
  state         TEXT NOT NULL DEFAULT 'committed' CHECK (state IN ('committed','done','failed')),
  generation_id TEXT,
  provider      TEXT,
  cost          REAL,
  cost_source   TEXT,
  tokens_in     INTEGER,
  tokens_out    INTEGER,
  reasoning_tokens INTEGER,
  out_path      TEXT,
  error         TEXT,
  created_at    INTEGER NOT NULL,
  finished_at   INTEGER
);

CREATE TABLE IF NOT EXISTS ledger (
  id            INTEGER PRIMARY KEY,
  plan_id       INTEGER REFERENCES plans(id),
  completion_id INTEGER REFERENCES completions(id),
  kind          TEXT NOT NULL CHECK (kind IN ('commit','actual','release')),
  amount        REAL NOT NULL,
  at            INTEGER NOT NULL,
  note          TEXT,
  CHECK ((plan_id IS NULL) <> (completion_id IS NULL))
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

-- The project index (v5). Unlike records, chunks can be replaced: re-indexing a changed
-- source swaps its chunks, so the FTS table has delete triggers too.
CREATE TABLE IF NOT EXISTS chunks (
  id         INTEGER PRIMARY KEY,
  source     TEXT NOT NULL,
  kind       TEXT NOT NULL CHECK (kind IN ('doc','code','log','record')),
  title      TEXT NOT NULL,
  ordinal    INTEGER NOT NULL,
  body       TEXT NOT NULL,
  sha256     TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS chunks_source ON chunks(source, ordinal);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(title, body, content='chunks', content_rowid='id');
CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
  INSERT INTO chunks_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;
CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, title, body) VALUES ('delete', old.id, old.title, old.body);
END;
CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
  INSERT INTO chunks_fts(chunks_fts, rowid, title, body) VALUES ('delete', old.id, old.title, old.body);
  INSERT INTO chunks_fts(rowid, title, body) VALUES (new.id, new.title, new.body);
END;

CREATE TABLE IF NOT EXISTS embeddings (
  chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
  model    TEXT NOT NULL,
  dim      INTEGER NOT NULL,
  vec      BLOB NOT NULL,
  scale    REAL NOT NULL
);

-- Verifier verdicts (v6). Append-only: a verdict is never edited, and the model's own
-- verdict is kept beside the one that stood after the quote rule. All of it is
-- untrusted model output.
CREATE TABLE IF NOT EXISTS verdicts (
  id             INTEGER PRIMARY KEY,
  claim_id       TEXT NOT NULL,
  claim_sha256   TEXT NOT NULL,
  check_type     TEXT NOT NULL CHECK (check_type IN ('grounded','reasoning','knowledge')),
  verdict        TEXT NOT NULL CHECK (verdict IN ('supported','unsupported','cannot_tell')),
  model_verdict  TEXT NOT NULL CHECK (model_verdict IN ('supported','unsupported','cannot_tell')),
  reason         TEXT,
  reasoning      TEXT NOT NULL,
  quote          TEXT NOT NULL,
  source         TEXT NOT NULL,
  needs_human    INTEGER NOT NULL,
  model          TEXT NOT NULL,
  plan_id        INTEGER,
  pins           TEXT NOT NULL,
  timing         TEXT NOT NULL,
  untrusted      INTEGER NOT NULL DEFAULT 1 CHECK (untrusted = 1),
  created_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS verdicts_claim ON verdicts(claim_id, id);
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

/// `file:` URI for a database path, percent-encoding everything but unreserved
/// characters and `/`, `:`, so a path with spaces, `?` or `#` still names the file.
fn plain_uri(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:");
    // A drive path (`C:/...`) needs the empty authority: `file:///C:/...`.
    if !s.starts_with('/') {
        out.push_str("///");
    }
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(char::from(b));
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn immutable_uri(path: &Path) -> String {
    format!("{}?immutable=1", plain_uri(path))
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

// ---------------------------------------------------------------- project index

/// One chunk to store for a source. `body` already begins with the header line.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChunk {
    pub ordinal: i64,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChunkRow {
    pub id: i64,
    pub source: String,
    pub kind: String,
    pub title: String,
    pub ordinal: i64,
    pub body: String,
}

/// A verdict as stored: the record, its row id and when it was written.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StoredVerdict {
    pub id: i64,
    pub created_at: i64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexStats {
    pub chunks_by_kind: Vec<(String, i64)>,
    pub sources: i64,
    pub embedded: i64,
    pub model: Option<String>,
    pub dim: Option<i64>,
    pub last_indexed: Option<i64>,
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
    /// Committed by open plans and held OpenRouter completions, not yet released.
    pub committed: f64,
    /// Recorded actual spend of closed plans and ended completions.
    pub spent: f64,
    pub remaining: f64,
}

impl Budget {
    /// The lowest the cap can be set: spent plus committed. At the floor nothing is
    /// left for a new plan, and every open plan keeps the money it was given.
    pub fn floor(&self) -> f64 {
        self.spent + self.committed
    }
}

/// One OpenRouter completion: its worst case committed against the budget before the
/// call, its real charge recorded after (see docs/sidecar-design.md, "The OpenRouter lane").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub id: i64,
    pub model: String,
    pub lane: String,
    pub input_bound: u64,
    pub max_tokens: u64,
    /// The priced rates, $ per million tokens.
    pub price_in_m: f64,
    pub price_out_m: f64,
    pub worst_case: f64,
    /// `committed` (the worst case is held), `done` or `failed`.
    pub state: String,
    pub generation_id: Option<String>,
    pub provider: Option<String>,
    pub cost: Option<f64>,
    /// Where the charge came from: `usage` (the stream), `generation` (asked after a
    /// failure) or `none` (no generation started, nothing charged).
    pub cost_source: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub out_path: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewCompletion {
    pub model: String,
    pub lane: String,
    pub input_bound: u64,
    pub max_tokens: u64,
    pub price_in_m: f64,
    pub price_out_m: f64,
    pub worst_case: f64,
}

/// How a completion ended. `cost` is the real charge; `None` keeps the commitment held
/// (the charge could not be read yet) and the completion stays `committed`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CompletionEnd {
    pub failed: bool,
    pub cost: Option<f64>,
    pub cost_source: Option<String>,
    pub provider: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub out_path: Option<String>,
    pub error: Option<String>,
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

    /// Open an existing store for reading only, and change nothing on disk.
    ///
    /// The file is opened with SQLite's read-only flag (it is never created) and
    /// `query_only` is on, so a write fails in SQLite itself. Nothing here sets a pragma
    /// that writes (no journal mode, no migration): a store from an older offrig is
    /// reported, never upgraded. A store in WAL mode that no one has open has no `-wal`
    /// or `-shm` file, and a plain read-only open would create both; so in that case it
    /// is opened `immutable` instead, which takes no locks and writes no side file. A
    /// store someone has open (its `-wal` or `-shm` exist) is read normally, through the
    /// files that are already there. This is how one lane reads another project's plans
    /// (issue #26).
    pub fn open_read_only(path: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let side = |ext: &str| {
            let mut o = path.as_os_str().to_os_string();
            o.push(ext);
            PathBuf::from(o)
        };
        let idle = !side("-wal").exists() && !side("-shm").exists();
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let target = if idle {
            immutable_uri(path)
        } else {
            plain_uri(path)
        };
        let conn = Connection::open_with_flags(&target, flags)
            .map_err(db("opening the project database read-only"))?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 2000;")
            .map_err(db("setting read-only pragmas"))?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db("reading the schema version"))?;
        if version > SCHEMA_VERSION {
            return Err(Error::Refused(format!(
                "the project database is schema v{version}; this offrig knows v{SCHEMA_VERSION}"
            )));
        }
        if version < 2 {
            return Err(Error::Refused(format!(
                "the project database is schema v{version}; its own offrig upgrades it, a read-only view does not"
            )));
        }
        Ok(Self { conn })
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
        // v3 -> v4: OpenRouter completions share the ledger, so its plan_id may be NULL.
        // SQLite cannot relax NOT NULL in place: rename the table, let the schema create
        // the new one, copy every row across and drop the old one, in one transaction.
        let rebuild_ledger = version < 4 && table_lacks(&conn, "ledger", "completion_id")?;
        if rebuild_ledger {
            conn.execute_batch("BEGIN; ALTER TABLE ledger RENAME TO ledger_v3;")
                .map_err(db("migrating the ledger to v4"))?;
        }
        conn.execute_batch(SCHEMA)
            .map_err(db("creating the schema"))?;
        if rebuild_ledger {
            conn.execute_batch(
                "INSERT INTO ledger(id, plan_id, kind, amount, at, note)
                   SELECT id, plan_id, kind, amount, at, note FROM ledger_v3;
                 DROP TABLE ledger_v3;
                 COMMIT;",
            )
            .map_err(db("copying the ledger to v4"))?;
        }
        let store = Self { conn };
        if (1..5).contains(&version) {
            // v4 -> v5: the index tables were just created; active records join it so
            // memory search ranks them. Their vectors fill in at the next embed pass.
            store.backfill_record_chunks()?;
        }
        store
            .conn
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .map_err(db("writing the schema version"))?;
        Ok(store)
    }

    fn backfill_record_chunks(&self) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting the record backfill"))?;
        let mut st = tx
            .prepare(&format!(
                "SELECT {RECORD_COLS} FROM records WHERE status = 'active' ORDER BY id"
            ))
            .map_err(db("reading records for the index"))?;
        let recs = st
            .query_map([], record_row)
            .map_err(db("reading records for the index"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("reading records for the index"))?;
        drop(st);
        for r in &recs {
            Self::insert_record_chunk(&tx, r.id, r.kind, &r.body, &r.tags)?;
        }
        tx.commit().map_err(db("committing the record backfill"))
    }

    /// An active record as one chunk, `source = record:<id>`, so memory search ranks it
    /// by keywords and, once embedded, by meaning.
    fn insert_record_chunk(
        conn: &Connection,
        id: i64,
        kind: Kind,
        body: &str,
        tags: &str,
    ) -> Result<()> {
        let source = format!("record:{id}");
        let mut text = crate::index::header(&source, "record", kind.as_str());
        text.push('\n');
        text.push_str(body.trim());
        if !tags.trim().is_empty() {
            text.push_str("\ntags: ");
            text.push_str(tags.trim());
        }
        conn.execute(
            "INSERT INTO chunks(source, kind, title, ordinal, body, sha256, created_at)
             VALUES (?1, 'record', ?2, 0, ?3, ?4, ?5)",
            params![
                source,
                kind.as_str(),
                text,
                crate::index::sha256_hex(text.as_bytes()),
                now_unix()
            ],
        )
        .map_err(db("indexing a record"))?;
        Ok(())
    }

    // ---- settings

    /// Set the cap. It can never go below what is already spent plus what is
    /// committed to open plans: money allocated to a running plan stays allocated, so
    /// a cap change can't strand a training run mid-way. The lowest cap
    /// ([`Budget::floor`]) leaves nothing for a new plan.
    pub fn set_budget_cap(&self, usd: f64) -> Result<()> {
        if !(usd.is_finite() && usd >= 0.0) {
            return Err(Error::Refused(format!(
                "budget cap {usd} must be a non-negative amount"
            )));
        }
        let b = self.budget()?;
        if usd + 1e-9 < b.floor() {
            return Err(Error::Refused(format!(
                "the cap can't go below ${:.2}: ${:.2} is spent and ${:.2} is committed to open plans and completions. ${:.2} stops new paid sessions and leaves running ones alone",
                b.floor(),
                b.spent,
                b.committed,
                b.floor()
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
            // A superseded record leaves the index, so hybrid search cannot return it.
            tx.execute(
                "DELETE FROM chunks WHERE source = ?1",
                params![format!("record:{old}")],
            )
            .map_err(db("removing the superseded record from the index"))?;
        }
        Self::insert_record_chunk(&tx, id, kind, r.body.trim(), &r.tags.join(" "))?;
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
        self.conn
            .execute(
                "DELETE FROM chunks WHERE source = ?1",
                params![format!("record:{id}")],
            )
            .map_err(db("removing the withdrawn record from the index"))?;
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

    // ---- the project index

    /// The hash recorded for an indexed source, `None` when it has no chunks.
    pub fn source_sha(&self, source: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT sha256 FROM chunks WHERE source = ?1 LIMIT 1",
                params![source],
                |r| r.get(0),
            )
            .optional()
            .map_err(db("reading a source hash"))
    }

    /// Swap a source's chunks for `chunks` in one transaction; the old ones (and their
    /// vectors) go. Returns the new chunk ids.
    pub fn replace_source(
        &self,
        source: &str,
        kind: &str,
        sha256: &str,
        chunks: &[NewChunk],
    ) -> Result<Vec<i64>> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a source replace"))?;
        tx.execute("DELETE FROM chunks WHERE source = ?1", params![source])
            .map_err(db("removing a source's old chunks"))?;
        let mut ids = Vec::with_capacity(chunks.len());
        for c in chunks {
            tx.execute(
                "INSERT INTO chunks(source, kind, title, ordinal, body, sha256, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![source, kind, c.title, c.ordinal, c.body, sha256, now_unix()],
            )
            .map_err(db("inserting a chunk"))?;
            ids.push(tx.last_insert_rowid());
        }
        tx.commit().map_err(db("committing a source replace"))?;
        Ok(ids)
    }

    /// Chunks that have no vector yet, oldest first: `(id, text to embed)`.
    pub fn unembedded(&self, limit: usize) -> Result<Vec<(i64, String)>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT c.id, c.body FROM chunks c LEFT JOIN embeddings e ON e.chunk_id = c.id
                 WHERE e.chunk_id IS NULL ORDER BY c.id LIMIT ?1",
            )
            .map_err(db("preparing the unembedded read"))?;
        st.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db("reading unembedded chunks"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("reading unembedded chunks"))
    }

    /// Store int8 vectors for chunks, and pin the index's model and dimension.
    pub fn put_embeddings(
        &self,
        model: &str,
        dim: usize,
        rows: &[(i64, Vec<i8>, f32)],
    ) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting an embedding write"))?;
        for (id, vec, scale) in rows {
            let bytes: Vec<u8> = vec.iter().map(|b| *b as u8).collect();
            tx.execute(
                "INSERT OR REPLACE INTO embeddings(chunk_id, model, dim, vec, scale)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, model, dim as i64, bytes, f64::from(*scale)],
            )
            .map_err(db("storing an embedding"))?;
        }
        for (k, v) in [
            ("embed_model", model.to_string()),
            ("embed_dim", dim.to_string()),
            ("index_updated_at", now_unix().to_string()),
        ] {
            tx.execute(
                "INSERT INTO settings(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v],
            )
            .map_err(db("writing the index settings"))?;
        }
        tx.commit().map_err(db("committing an embedding write"))
    }

    /// Forget every vector and the index's model, so a different model can be used.
    /// Chunks stay; the next embed pass refills them.
    pub fn clear_embeddings(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "DELETE FROM embeddings;
                 DELETE FROM settings WHERE key IN ('embed_model', 'embed_dim');",
            )
            .map_err(db("clearing embeddings"))
    }

    pub fn has_embeddings(&self) -> Result<bool> {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM embeddings)", [], |r| r.get(0))
            .map_err(db("checking for embeddings"))
    }

    /// Chunk ids matching `text` by BM25, best first, optionally of one kind.
    pub fn fts_chunks(&self, text: &str, kind: Option<&str>, limit: usize) -> Result<Vec<i64>> {
        let Some(fts) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut st = self
            .conn
            .prepare(
                "SELECT c.id FROM chunks_fts f JOIN chunks c ON c.id = f.rowid
                 WHERE chunks_fts MATCH ?1 AND (?2 IS NULL OR c.kind = ?2)
                 ORDER BY bm25(chunks_fts) LIMIT ?3",
            )
            .map_err(db("preparing a chunk search"))?;
        st.query_map(params![fts, kind, limit as i64], |r| r.get(0))
            .map_err(db("searching chunks"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("searching chunks"))
    }

    /// Every stored vector `(chunk id, scale, int8 bytes)`, optionally of one chunk kind.
    pub fn embedding_rows(&self, kind: Option<&str>) -> Result<Vec<(i64, f32, Vec<u8>)>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT e.chunk_id, e.scale, e.vec FROM embeddings e JOIN chunks c ON c.id = e.chunk_id
                 WHERE (?1 IS NULL OR c.kind = ?1)",
            )
            .map_err(db("preparing the vector read"))?;
        st.query_map(params![kind], |r| {
            Ok((r.get(0)?, r.get::<_, f64>(1)? as f32, r.get(2)?))
        })
        .map_err(db("reading vectors"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db("reading vectors"))
    }

    /// The chunks with these ids, in the order asked; ids that no longer exist are
    /// left out.
    pub fn chunks_by_ids(&self, ids: &[i64]) -> Result<Vec<ChunkRow>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let row = self
                .conn
                .query_row(
                    "SELECT id, source, kind, title, ordinal, body FROM chunks WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok(ChunkRow {
                            id: r.get(0)?,
                            source: r.get(1)?,
                            kind: r.get(2)?,
                            title: r.get(3)?,
                            ordinal: r.get(4)?,
                            body: r.get(5)?,
                        })
                    },
                )
                .optional()
                .map_err(db("reading a chunk"))?;
            out.extend(row);
        }
        Ok(out)
    }

    // ---- Verifier verdicts

    /// Store a verdict. Rows are only ever added.
    pub fn save_verdict(&self, v: &Verdict) -> Result<i64> {
        let pins =
            serde_json::to_string(&v.pins).map_err(|e| Error::decode("a verdict's pins", e))?;
        let timing =
            serde_json::to_string(&v.timing).map_err(|e| Error::decode("a verdict's timing", e))?;
        self.conn
            .execute(
                "INSERT INTO verdicts(claim_id, claim_sha256, check_type, verdict, model_verdict, reason,
                                      reasoning, quote, source, needs_human, model, plan_id, pins, timing, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    v.claim_id,
                    v.claim_sha256,
                    v.check_type.as_str(),
                    v.verdict.as_str(),
                    v.model_verdict.as_str(),
                    v.reason,
                    v.reasoning,
                    v.evidence_quote,
                    v.evidence_source,
                    v.needs_human,
                    v.pins.model,
                    v.pins.plan_id,
                    pins,
                    timing,
                    now_unix()
                ],
            )
            .map_err(db("saving a verdict"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every stored verdict for a claim id, oldest first.
    pub fn verdicts_for_claim(&self, claim_id: &str) -> Result<Vec<StoredVerdict>> {
        self.verdicts_where("claim_id = ?1 ORDER BY id", params![claim_id])
    }

    /// The most recent verdicts, newest first.
    pub fn recent_verdicts(&self, n: usize) -> Result<Vec<StoredVerdict>> {
        self.verdicts_where("1 ORDER BY id DESC LIMIT ?1", params![n as i64])
    }

    fn verdicts_where(
        &self,
        clause: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<StoredVerdict>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT id, created_at, claim_id, claim_sha256, check_type, verdict, model_verdict, reason,
                        reasoning, quote, source, needs_human, pins, timing
                 FROM verdicts WHERE {clause}"
            ))
            .map_err(db("reading verdicts"))?;
        let rows = st
            .query_map(args, |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    [
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ],
                    r.get::<_, Option<String>>(7)?,
                    [
                        r.get::<_, String>(8)?,
                        r.get::<_, String>(9)?,
                        r.get::<_, String>(10)?,
                    ],
                    r.get::<_, bool>(11)?,
                    [r.get::<_, String>(12)?, r.get::<_, String>(13)?],
                ))
            })
            .map_err(db("reading verdicts"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("reading verdicts"))?;
        rows.into_iter()
            .map(
                |(
                    id,
                    created_at,
                    [claim_id, sha, ct, vd, mv],
                    reason,
                    [reasoning, quote, source],
                    needs_human,
                    [pins, timing],
                )| {
                    let bad = |what: &str| Error::Refused(format!("verdict {id} has a bad {what}"));
                    Ok(StoredVerdict {
                        id,
                        created_at,
                        verdict: Verdict {
                            claim_id,
                            claim_sha256: sha,
                            check_type: CheckType::parse(&ct).ok_or_else(|| bad("check type"))?,
                            verdict: VerdictKind::parse(&vd).ok_or_else(|| bad("verdict"))?,
                            model_verdict: VerdictKind::parse(&mv)
                                .ok_or_else(|| bad("model verdict"))?,
                            reason,
                            reasoning,
                            evidence_quote: quote,
                            evidence_source: source,
                            needs_human,
                            pins: serde_json::from_str(&pins)
                                .map_err(|e| Error::decode("a verdict's pins", e))?,
                            timing: serde_json::from_str(&timing)
                                .map_err(|e| Error::decode("a verdict's timing", e))?,
                            untrusted: true,
                        },
                    })
                },
            )
            .collect()
    }

    pub fn index_stats(&self) -> Result<IndexStats> {
        let count = |sql: &str| -> Result<i64> {
            self.conn
                .query_row(sql, [], |r| r.get(0))
                .map_err(db("counting the index"))
        };
        let mut st = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM chunks GROUP BY kind ORDER BY kind")
            .map_err(db("counting chunks"))?;
        let by_kind = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db("counting chunks"))?
            .collect::<rusqlite::Result<Vec<(String, i64)>>>()
            .map_err(db("counting chunks"))?;
        Ok(IndexStats {
            chunks_by_kind: by_kind,
            sources: count("SELECT COUNT(DISTINCT source) FROM chunks")?,
            embedded: count("SELECT COUNT(*) FROM embeddings")?,
            model: self.setting("embed_model")?,
            dim: self.setting("embed_dim")?.and_then(|d| d.parse().ok()),
            last_indexed: self
                .setting("index_updated_at")?
                .and_then(|d| d.parse().ok()),
        })
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
            return Err(Error::Budget(format!(
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

    // ---- OpenRouter completions

    /// Commit a completion's worst case against the budget, refused when it exceeds what
    /// is left. The check and the commit run under one write lock (`BEGIN IMMEDIATE`), so
    /// two side-cars sharing this file cannot both commit past the cap.
    pub fn commit_completion(&self, c: NewCompletion) -> Result<Completion> {
        if !(c.worst_case.is_finite() && c.worst_case >= 0.0) {
            return Err(Error::Refused(format!(
                "worst case {} must be a non-negative amount",
                c.worst_case
            )));
        }
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(db("starting a completion commit"))?;
        let res = self.commit_completion_locked(&c);
        match res {
            Ok(id) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(db("committing a completion"))?;
                self.completion(id)?
                    .ok_or_else(|| Error::Refused("completion vanished after commit".into()))
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn commit_completion_locked(&self, c: &NewCompletion) -> Result<i64> {
        let b = self.budget()?;
        if c.worst_case > b.remaining + 1e-9 {
            return Err(Error::Budget(format!(
                "worst case ${:.2} (at most {} tokens in and {} out, at ${}/M in and ${}/M out) \
                 exceeds the ${:.2} left of the ${:.2} budget; lower max_tokens or ask Mike to raise the cap",
                c.worst_case,
                c.input_bound,
                c.max_tokens,
                c.price_in_m,
                c.price_out_m,
                b.remaining,
                b.cap
            )));
        }
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO completions(model, lane, input_bound, max_tokens, price_in_m, price_out_m, worst_case, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    c.model,
                    c.lane,
                    c.input_bound as i64,
                    c.max_tokens as i64,
                    c.price_in_m,
                    c.price_out_m,
                    c.worst_case,
                    now
                ],
            )
            .map_err(db("saving a completion"))?;
        let id = self.conn.last_insert_rowid();
        self.conn
            .execute(
                "INSERT INTO ledger(completion_id, kind, amount, at, note) VALUES (?1, 'commit', ?2, ?3, 'worst case')",
                params![id, c.worst_case, now],
            )
            .map_err(db("writing a completion commit"))?;
        Ok(id)
    }

    /// Note the generation id as soon as the stream names it, so the charge can be looked
    /// up later even if this process dies mid-stream.
    pub fn set_completion_generation(&self, id: i64, generation_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE completions SET generation_id = ?1 WHERE id = ?2",
                params![generation_id, id],
            )
            .map_err(db("recording a generation id"))?;
        Ok(())
    }

    /// End a completion. With a known cost: release the whole commitment and record the
    /// cost as actual spend, like `close_plan`. Without one: record what is known and keep
    /// the commitment held. A closed completion is left as it is.
    pub fn close_completion(&self, id: i64, end: CompletionEnd) -> Result<Budget> {
        let c = self
            .completion(id)?
            .ok_or_else(|| Error::Refused(format!("no completion {id}")))?;
        if c.state != "committed" {
            return self.budget();
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db("starting a completion close"))?;
        let now = now_unix();
        let state = match (end.cost, end.failed) {
            (None, _) => "committed",
            (Some(_), true) => "failed",
            (Some(_), false) => "done",
        };
        if let Some(cost) = end.cost {
            tx.execute(
                "INSERT INTO ledger(completion_id, kind, amount, at, note) VALUES (?1, 'release', ?2, ?3, 'completion ended')",
                params![id, c.worst_case, now],
            )
            .map_err(db("writing a completion release"))?;
            tx.execute(
                "INSERT INTO ledger(completion_id, kind, amount, at, note) VALUES (?1, 'actual', ?2, ?3, ?4)",
                params![
                    id,
                    cost.max(0.0),
                    now,
                    end.cost_source.as_deref().unwrap_or("measured")
                ],
            )
            .map_err(db("writing a completion's charge"))?;
        }
        tx.execute(
            "UPDATE completions SET state = ?2, cost = ?3, cost_source = COALESCE(?4, cost_source),
                    provider = COALESCE(?5, provider), tokens_in = COALESCE(?6, tokens_in),
                    tokens_out = COALESCE(?7, tokens_out), reasoning_tokens = COALESCE(?8, reasoning_tokens),
                    out_path = COALESCE(?9, out_path), error = COALESCE(?10, error),
                    finished_at = CASE WHEN ?3 IS NULL THEN finished_at ELSE ?11 END
             WHERE id = ?1",
            params![
                id,
                state,
                end.cost,
                end.cost_source,
                end.provider,
                end.tokens_in.map(|v| v as i64),
                end.tokens_out.map(|v| v as i64),
                end.reasoning_tokens.map(|v| v as i64),
                end.out_path,
                end.error,
                now
            ],
        )
        .map_err(db("closing a completion"))?;
        tx.commit().map_err(db("committing a completion close"))?;
        self.budget()
    }

    pub fn completion(&self, id: i64) -> Result<Option<Completion>> {
        self.conn
            .query_row(
                &format!("SELECT {COMPLETION_COLS} FROM completions WHERE id = ?1"),
                params![id],
                completion_row,
            )
            .optional()
            .map_err(db("reading a completion"))
    }

    /// Completions whose worst case is still held: in flight, or ended with a charge
    /// that could not be read yet.
    pub fn open_completions(&self) -> Result<Vec<Completion>> {
        self.completions_where("state = 'committed' ORDER BY id", 1000)
    }

    /// The most recent completions, newest first.
    pub fn recent_completions(&self, n: usize) -> Result<Vec<Completion>> {
        self.completions_where("1 ORDER BY id DESC", n)
    }

    fn completions_where(&self, clause: &str, n: usize) -> Result<Vec<Completion>> {
        let mut st = self
            .conn
            .prepare(&format!(
                "SELECT {COMPLETION_COLS} FROM completions WHERE {clause} LIMIT {n}"
            ))
            .map_err(db("preparing a completion read"))?;
        st.query_map([], completion_row)
            .map_err(db("reading completions"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db("reading completions"))
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

const COMPLETION_COLS: &str = "id, model, lane, input_bound, max_tokens, price_in_m, price_out_m, worst_case, \
     state, generation_id, provider, cost, cost_source, tokens_in, tokens_out, reasoning_tokens, out_path, error, \
     created_at, finished_at";

fn completion_row(r: &Row<'_>) -> rusqlite::Result<Completion> {
    let u = |v: Option<i64>| v.map(|n| n.max(0) as u64);
    Ok(Completion {
        id: r.get(0)?,
        model: r.get(1)?,
        lane: r.get(2)?,
        input_bound: r.get::<_, i64>(3)?.max(0) as u64,
        max_tokens: r.get::<_, i64>(4)?.max(0) as u64,
        price_in_m: r.get(5)?,
        price_out_m: r.get(6)?,
        worst_case: r.get(7)?,
        state: r.get(8)?,
        generation_id: r.get(9)?,
        provider: r.get(10)?,
        cost: r.get(11)?,
        cost_source: r.get(12)?,
        tokens_in: u(r.get(13)?),
        tokens_out: u(r.get(14)?),
        reasoning_tokens: u(r.get(15)?),
        out_path: r.get(16)?,
        error: r.get(17)?,
        created_at: r.get(18)?,
        finished_at: r.get(19)?,
    })
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
    fn the_cap_never_drops_below_money_already_given_to_runs() {
        let s = store();
        s.set_budget_cap(20.0).expect("cap");
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
        let b = s.budget().expect("b");
        assert!((b.floor() - 6.27).abs() < 1e-9, "{b:?}");
        let low = s.set_budget_cap(0.0).expect_err("below the committed run");
        assert!(low.to_string().contains("can't go below $6.27"), "{low}");
        assert!(s.set_budget_cap(5.0).is_err());
        assert_eq!(
            s.budget().expect("b").cap,
            20.0,
            "a refused cap changes nothing"
        );
        // The floor itself is allowed: nothing left for a new plan, the run keeps its money.
        s.set_budget_cap(b.floor()).expect("the floor");
        let at = s.budget().expect("b");
        assert!(
            at.remaining.abs() < 1e-9 && (at.committed - 6.27).abs() < 1e-9,
            "{at:?}"
        );
        // After the run closes, only what it actually spent holds the floor up.
        s.close_plan(p.id, 1.10).expect("close");
        s.set_budget_cap(1.10).expect("down to actual spend");
        assert!(s.set_budget_cap(1.0).is_err(), "below spent");
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

    fn new_completion(worst: f64) -> NewCompletion {
        NewCompletion {
            model: "moonshotai/kimi-k3".into(),
            lane: "ai-jam-sessions".into(),
            input_bound: 2_000,
            max_tokens: 200_000,
            price_in_m: 0.72,
            price_out_m: 15.0,
            worst_case: worst,
        }
    }

    #[test]
    fn a_completion_commits_its_worst_case_and_records_the_real_charge() {
        let s = store();
        s.set_budget_cap(4.50).expect("cap");
        let c = s.commit_completion(new_completion(3.01)).expect("commit");
        assert_eq!(c.state, "committed");
        let b = s.budget().expect("budget");
        assert_eq!((b.committed, b.spent), (3.01, 0.0));
        assert!((b.remaining - 1.49).abs() < 1e-9);
        // Not a plan: the lane rule and the watchdog never see it.
        assert!(s.open_plans().expect("plans").is_empty());
        assert_eq!(s.open_completions().expect("open").len(), 1);

        s.set_completion_generation(c.id, "gen-1").expect("gen");
        let b = s
            .close_completion(
                c.id,
                CompletionEnd {
                    cost: Some(0.30),
                    cost_source: Some("usage".into()),
                    provider: Some("Wafer".into()),
                    tokens_in: Some(1_700),
                    tokens_out: Some(19_500),
                    out_path: Some(".offrig/out/completion-1.md".into()),
                    ..Default::default()
                },
            )
            .expect("close");
        assert_eq!(b.committed, 0.0, "the whole commitment is released");
        assert!((b.spent - 0.30).abs() < 1e-9 && (b.remaining - 4.20).abs() < 1e-9);
        let done = s.completion(c.id).expect("read").expect("row");
        assert_eq!(done.state, "done");
        assert_eq!(done.generation_id.as_deref(), Some("gen-1"));
        assert_eq!(done.provider.as_deref(), Some("Wafer"));
        assert_eq!(done.cost, Some(0.30));
        assert!(done.finished_at.is_some());
        // Closing twice changes nothing.
        let again = s
            .close_completion(
                c.id,
                CompletionEnd {
                    cost: Some(9.0),
                    ..Default::default()
                },
            )
            .expect("again");
        assert!((again.spent - 0.30).abs() < 1e-9);
        assert!(s.open_completions().expect("open").is_empty());
        assert_eq!(s.recent_completions(5).expect("recent")[0].id, c.id);
    }

    #[test]
    fn a_held_completion_counts_in_the_cap_floor() {
        let s = store();
        s.set_budget_cap(4.50).expect("cap");
        let c = s.commit_completion(new_completion(3.01)).expect("commit");
        let b = s.budget().expect("budget");
        assert!(
            (b.floor() - 3.01).abs() < 1e-9,
            "the held worst case is in the floor"
        );
        let err = s.set_budget_cap(3.00).expect_err("below the floor");
        assert!(matches!(err, Error::Refused(_)), "{err}");
        assert!(err.to_string().contains("$3.01"), "{err}");
        s.set_budget_cap(3.01).expect("at the floor");
        // Ended: the floor is the real charge, and the rest of the commitment is free.
        s.close_completion(
            c.id,
            CompletionEnd {
                cost: Some(0.30),
                cost_source: Some("usage".into()),
                ..Default::default()
            },
        )
        .expect("close");
        assert!((s.budget().expect("budget").floor() - 0.30).abs() < 1e-9);
        s.set_budget_cap(0.30).expect("down to what was spent");
        assert!(s.set_budget_cap(0.29).is_err());
    }

    #[test]
    fn a_completion_over_the_budget_left_is_refused_and_commits_nothing() {
        let s = store();
        s.set_budget_cap(4.50).expect("cap");
        s.commit_completion(new_completion(3.01)).expect("first");
        let err = s
            .commit_completion(new_completion(3.01))
            .expect_err("over budget");
        assert!(matches!(err, Error::Budget(_)), "{err}");
        assert!(err.to_string().contains("$1.49 left"), "{err}");
        assert_eq!(s.open_completions().expect("open").len(), 1);
        assert!((s.budget().expect("b").committed - 3.01).abs() < 1e-9);
        assert!(matches!(
            s.commit_completion(new_completion(f64::NAN)),
            Err(Error::Refused(_))
        ));
        // Pods and completions draw on one cap.
        let plan = s.create_plan(NewPlan {
            profile: "jam".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 4.0,
            max_price_hr: 0.49,
            note: None,
        });
        assert!(matches!(plan, Err(Error::Budget(_))), "1.96 > 1.49 left");
    }

    #[test]
    fn an_unread_charge_keeps_the_commitment_held_and_a_failure_records_its_charge() {
        let s = store();
        s.set_budget_cap(10.0).expect("cap");
        let c = s.commit_completion(new_completion(3.01)).expect("commit");
        s.close_completion(
            c.id,
            CompletionEnd {
                failed: true,
                error: Some("stream broke".into()),
                ..Default::default()
            },
        )
        .expect("held");
        let held = s.completion(c.id).expect("read").expect("row");
        assert_eq!(held.state, "committed");
        assert_eq!(held.error.as_deref(), Some("stream broke"));
        assert!((s.budget().expect("b").committed - 3.01).abs() < 1e-9);
        let b = s
            .close_completion(
                c.id,
                CompletionEnd {
                    failed: true,
                    cost: Some(0.12),
                    cost_source: Some("generation".into()),
                    ..Default::default()
                },
            )
            .expect("settled");
        assert!((b.spent - 0.12).abs() < 1e-9 && b.committed == 0.0);
        let row = s.completion(c.id).expect("read").expect("row");
        assert_eq!(row.state, "failed");
        assert_eq!(row.error.as_deref(), Some("stream broke"), "kept");
        assert_eq!(row.cost_source.as_deref(), Some("generation"));
        assert!(matches!(
            s.close_completion(999, CompletionEnd::default()),
            Err(Error::Refused(_))
        ));
    }

    #[test]
    fn v3_ledgers_migrate_to_v4_without_losing_rows() {
        let dir = std::env::temp_dir().join(format!("offrig-migrate4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("v3.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).expect("open");
            c.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY, profile TEXT NOT NULL, gpu_count INTEGER NOT NULL,
                   gpu_types TEXT NOT NULL, max_hours REAL NOT NULL, max_price_hr REAL NOT NULL, worst_case REAL NOT NULL,
                   state TEXT NOT NULL DEFAULT 'planned', pod_id TEXT, created_at INTEGER NOT NULL, note TEXT,
                   committed_at INTEGER, started_at INTEGER);
                 CREATE TABLE ledger (id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL REFERENCES plans(id),
                   kind TEXT NOT NULL CHECK (kind IN ('commit','actual','release')), amount REAL NOT NULL,
                   at INTEGER NOT NULL, note TEXT);
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO settings VALUES ('budget_cap', '12');
                 INSERT INTO plans(profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, state, created_at)
                   VALUES ('small', 1, '[]', 1.0, 0.24, 0.24, 'closed', 1);
                 INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (1, 'commit', 0.24, 1, 'worst case');
                 INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (1, 'release', 0.24, 2, 'plan closed');
                 INSERT INTO ledger(plan_id, kind, amount, at, note) VALUES (1, 'actual', 0.08, 2, 'measured');
                 PRAGMA user_version = 3;",
            )
            .expect("v3 schema");
        }
        let s = Store::open(&path).expect("migrates");
        let b = s.budget().expect("budget");
        assert_eq!((b.cap, b.committed), (12.0, 0.0));
        assert!((b.spent - 0.08).abs() < 1e-9, "every ledger row survived");
        let rows: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM ledger WHERE plan_id = 1", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(rows, 3);
        assert!(!table_lacks(&s.conn, "ledger", "completion_id").expect("cols"));
        let old: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'ledger_v3'",
                [],
                |r| r.get(0),
            )
            .expect("master");
        assert_eq!(old, 0, "the old table is gone");
        s.commit_completion(new_completion(1.0))
            .expect("a completion row");
        // A ledger row belongs to exactly one plan or completion.
        assert!(
            s.conn
                .execute(
                    "INSERT INTO ledger(kind, amount, at) VALUES ('commit', 1.0, 3)",
                    []
                )
                .is_err()
        );
        assert!(
            s.conn
                .execute(
                    "INSERT INTO ledger(plan_id, completion_id, kind, amount, at) VALUES (1, 1, 'commit', 1.0, 3)",
                    []
                )
                .is_err()
        );
        drop(s);
        let v: i64 = Connection::open(&path)
            .expect("reopen")
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(v, SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v4_databases_migrate_to_v5_and_index_their_active_records() {
        let dir = std::env::temp_dir().join(format!("offrig-migrate5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("v4.db");
        let _ = std::fs::remove_file(&path);
        {
            // A v5 file with the index torn out is what a v4 binary left behind.
            let s = Store::open(&path).expect("open");
            let old = s
                .record(rec(Kind::Fact, "the solver is slow"))
                .expect("old");
            let mut next = rec(Kind::Fact, "the solver is fast now");
            next.supersedes = Some(old);
            next.reason = Some("measured".into());
            s.record(next).expect("new");
            s.conn
                .execute_batch(
                    "DROP TABLE embeddings; DROP TABLE chunks_fts; DROP TABLE chunks;
                     PRAGMA user_version = 4;",
                )
                .expect("v4 shape");
        }
        let s = Store::open(&path).expect("migrates");
        let stats = s.index_stats().expect("stats");
        assert_eq!(stats.chunks_by_kind, vec![("record".to_string(), 1)]);
        // Only the active record is searchable, by its chunk.
        let ids = s.fts_chunks("solver", Some("record"), 10).expect("fts");
        let rows = s.chunks_by_ids(&ids).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "record:2");
        assert!(
            rows[0]
                .body
                .starts_with("[record:2 \u{b7} record \u{b7} fact]\n")
        );
        drop(s);
        let v: i64 = Connection::open(&path)
            .expect("reopen")
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(v, SCHEMA_VERSION);
        // A read-only view still opens a current store.
        assert!(Store::open_read_only(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v5_databases_migrate_to_v6_and_gain_the_verdicts_table() {
        let dir = std::env::temp_dir().join(format!("offrig-migrate6-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("v5.db");
        let _ = std::fs::remove_file(&path);
        {
            let s = Store::open(&path).expect("open");
            let old = s
                .record(rec(Kind::Fact, "kept across the migration"))
                .expect("rec");
            s.conn
                .execute_batch("DROP TABLE verdicts; PRAGMA user_version = 5;")
                .expect("v5 shape");
            assert_eq!(old, 1);
        }
        let s = Store::open(&path).expect("migrates");
        assert!(s.recent_verdicts(5).expect("empty table exists").is_empty());
        s.save_verdict(&crate::verify::sample_verdict("c1", VerdictKind::Supported))
            .expect("writes");
        assert_eq!(
            s.index_stats().expect("stats").chunks_by_kind,
            vec![("record".to_string(), 1)]
        );
        drop(s);
        let v: i64 = Connection::open(&path)
            .expect("reopen")
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(v, 6);
        assert!(Store::open_read_only(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verdicts_are_appended_and_read_back_whole() {
        use crate::verify::sample_verdict;
        let s = store();
        let mut v = sample_verdict("claim-a", VerdictKind::Supported);
        v.pins.plan_id = Some(3);
        v.pins.model_digest = Some("sha256:abc".into());
        v.needs_human = true;
        let first = s.save_verdict(&v).expect("save");
        let mut downgraded = sample_verdict("claim-a", VerdictKind::CannotTell);
        downgraded.model_verdict = VerdictKind::Supported;
        downgraded.reason = Some("quote_not_found".into());
        let second = s.save_verdict(&downgraded).expect("save again");
        s.save_verdict(&sample_verdict("claim-b", VerdictKind::Unsupported))
            .expect("other claim");
        assert!(second > first);
        let rows = s.verdicts_for_claim("claim-a").expect("rows");
        assert_eq!(rows.len(), 2);
        // The whole record, pins included, survives the round trip.
        assert_eq!(rows[0].verdict, v);
        assert_eq!(rows[0].id, first);
        assert!(rows[0].created_at > 0);
        assert_eq!(rows[1].verdict.verdict, VerdictKind::CannotTell);
        assert_eq!(rows[1].verdict.model_verdict, VerdictKind::Supported);
        assert_eq!(rows[1].verdict.reason.as_deref(), Some("quote_not_found"));
        let recent = s.recent_verdicts(2).expect("recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].verdict.claim_id, "claim-b");
        assert!(s.verdicts_for_claim("nobody").expect("none").is_empty());
        // Stored rows are always untrusted; the column refuses anything else.
        let e = s.conn.execute("UPDATE verdicts SET untrusted = 0", []);
        assert!(e.is_err());
        // A corrupted row is reported, not guessed at.
        s.conn
            .execute("UPDATE verdicts SET pins = 'nope' WHERE id = ?1", [first])
            .expect("corrupt");
        assert!(s.verdicts_for_claim("claim-a").is_err());
    }

    #[test]
    fn records_are_chunks_until_superseded_or_withdrawn() {
        let s = store();
        let a = s
            .record(rec(Kind::Decision, "use the quartz scheduler"))
            .expect("a");
        let b = s
            .record(rec(Kind::Fact, "quartz needs a clock"))
            .expect("b");
        let sources = |s: &Store| {
            let ids = s.fts_chunks("quartz", Some("record"), 10).expect("fts");
            let mut v: Vec<String> = s
                .chunks_by_ids(&ids)
                .expect("rows")
                .into_iter()
                .map(|c| c.source)
                .collect();
            v.sort();
            v
        };
        assert_eq!(sources(&s), [format!("record:{a}"), format!("record:{b}")]);
        let mut next = rec(Kind::Decision, "use the cron scheduler");
        next.supersedes = Some(a);
        next.reason = Some("quartz is gone".into());
        let c = s.record(next).expect("c");
        assert_eq!(sources(&s), [format!("record:{b}")]);
        s.withdraw(b, "wrong").expect("withdraw");
        assert!(sources(&s).is_empty());
        assert_eq!(s.fts_chunks("cron", None, 10).expect("fts").len(), 1);
        assert_eq!(
            s.index_stats().expect("stats").chunks_by_kind,
            vec![("record".to_string(), 1)]
        );
        let _ = c;
    }

    #[test]
    fn replacing_a_source_swaps_chunks_their_vectors_and_the_fts_rows() {
        let s = store();
        let chunk = |t: &str| NewChunk {
            ordinal: 0,
            title: "t".into(),
            body: t.into(),
        };
        let ids = s
            .replace_source("a.md", "doc", "h1", &[chunk("alpha beta")])
            .expect("one");
        assert_eq!(s.source_sha("a.md").expect("sha").as_deref(), Some("h1"));
        assert_eq!(s.unembedded(10).expect("todo").len(), 1);
        s.put_embeddings("m", 2, &[(ids[0], vec![127, 0], 0.01)])
            .expect("put");
        assert!(s.has_embeddings().expect("has"));
        assert!(s.unembedded(10).expect("todo").is_empty());
        assert_eq!(s.embedding_rows(Some("doc")).expect("rows").len(), 1);
        assert!(s.embedding_rows(Some("code")).expect("rows").is_empty());
        let st = s.index_stats().expect("stats");
        assert_eq!(
            (st.model.as_deref(), st.dim, st.embedded),
            (Some("m"), Some(2), 1)
        );
        // New text replaces the old: the vector cascades away, FTS follows.
        s.replace_source("a.md", "doc", "h2", &[chunk("gamma")])
            .expect("two");
        assert!(!s.has_embeddings().expect("has"));
        assert!(s.fts_chunks("alpha", None, 5).expect("fts").is_empty());
        assert_eq!(s.fts_chunks("gamma", None, 5).expect("fts").len(), 1);
        s.clear_embeddings().expect("clear");
        assert_eq!(s.setting("embed_model").expect("setting"), None);
        assert!(s.replace_source("b", "bogus", "h", &[chunk("x")]).is_err());
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
    fn ro_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("offrig-ro-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|e| {
                let e = e.expect("entry");
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).expect("read"),
                )
            })
            .collect();
        v.sort();
        v
    }

    fn committed_plan(s: &Store) -> Plan {
        let p = s
            .create_plan(NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 1.0,
                max_price_hr: 2.0,
                note: Some("n".into()),
            })
            .unwrap_or_else(|e| panic!("{e}"));
        s.commit_plan(p.id).expect("commit")
    }

    #[test]
    fn a_read_only_open_reads_plans_and_changes_nothing_on_disk() {
        let dir = ro_dir("idle");
        let path = dir.join("a b#c").join("offrig.db"); // a path that needs URI escaping
        {
            let s = Store::open(&path).expect("open");
            s.set_budget_cap(50.0).expect("cap");
            committed_plan(&s);
        }
        let folder = path.parent().expect("folder").to_path_buf();
        let before = files(&folder);
        {
            let ro = Store::open_read_only(&path).expect("read-only");
            let open = ro.open_plans().expect("plans");
            assert_eq!(open.len(), 1);
            assert_eq!(open[0].note.as_deref(), Some("n"));
            // SQLite itself refuses a write.
            assert!(ro.set_budget_cap(1.0).is_err());
            assert!(ro.set_setting("k", "v").is_err());
        }
        assert_eq!(files(&folder), before, "no byte and no side file changed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_only_open_sees_a_live_writers_committed_rows() {
        let dir = ro_dir("live");
        let path = dir.join("offrig.db");
        let writer = Store::open(&path).expect("open");
        writer.set_budget_cap(50.0).expect("cap");
        committed_plan(&writer);
        // The writer is still open, so its -wal/-shm exist: the read goes through them.
        let ro = Store::open_read_only(&path).expect("read-only");
        assert_eq!(ro.open_plans().expect("plans").len(), 1);
        assert!(ro.set_setting("k", "v").is_err(), "still read-only");
        drop(ro);
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_only_open_never_creates_migrates_or_reads_a_foreign_version() {
        let dir = ro_dir("refuse");
        // Missing: not created.
        let missing = dir.join("nope").join("offrig.db");
        assert!(Store::open_read_only(&missing).is_err());
        assert!(!missing.exists() && !missing.parent().expect("p").exists());
        // Not a database.
        let junk = dir.join("junk.db");
        std::fs::write(
            &junk,
            b"definitely not sqlite, but long enough to be read as a header",
        )
        .expect("junk");
        let before = std::fs::read(&junk).expect("read");
        assert!(Store::open_read_only(&junk).is_err());
        assert_eq!(std::fs::read(&junk).expect("read"), before);
        // A v1 store is reported, not upgraded.
        let v1 = dir.join("v1.db");
        {
            let c = Connection::open(&v1).expect("open");
            c.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY); PRAGMA user_version = 1;",
            )
            .expect("v1");
        }
        let before = std::fs::read(&v1).expect("read");
        let e = Store::open_read_only(&v1).err().expect("refused");
        assert!(e.to_string().contains("schema v1"), "{e}");
        assert_eq!(std::fs::read(&v1).expect("read"), before, "not migrated");
        // A newer schema is refused too.
        let v9 = dir.join("v9.db");
        {
            let c = Connection::open(&v9).expect("open");
            c.execute_batch("PRAGMA user_version = 9;").expect("v9");
        }
        let e = Store::open_read_only(&v9).err().expect("refused");
        assert!(e.to_string().contains("schema v9"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_uris_escape_what_would_end_a_path() {
        let u = plain_uri(Path::new("/tmp/a b/c#d?e%f.db"));
        assert!(u.starts_with("file:"), "{u}");
        assert!(u.ends_with("/tmp/a%20b/c%23d%3Fe%25f.db"), "{u}");
        assert!(immutable_uri(Path::new("/x.db")).ends_with("/x.db?immutable=1"));
        // A drive path gets the empty authority.
        if cfg!(windows) {
            let u = plain_uri(Path::new("C:\\data\\x.db"));
            assert_eq!(u, "file:///C:/data/x.db");
        }
    }
}
