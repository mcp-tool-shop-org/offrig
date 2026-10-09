//! The account view: caps are per project, money is per provider account.
//!
//! Each project's store holds its own caps, so two projects can each promise the same
//! RunPod balance. This module reads across projects, strictly read-only, to show what
//! all the caps together promise ([`account_totals`]) and to refuse a commitment the
//! account could not pay once the other projects' committed money is counted
//! ([`AccountGuard`], used by `Store::commit_plan_with_account` and
//! `Store::commit_completion_with_account`).
//!
//! Which projects are known: the registry `<config dir>/budget-projects.toml` (a project
//! is added whenever `offrig budget` sets a cap there), the lane registry's projects and
//! the current one.
//!
//! Two different jobs, two different failure rules:
//! - Display ([`account_totals`]): a project whose store can't be read is a note and is
//!   left out of the sums.
//! - Guard ([`AccountGuard`]): a known project with a store that is present but can't be
//!   read (busy after one retry, newer or older schema, corrupt) makes the check refuse,
//!   because its committed money is unknown. A project with no store holds 0.
//!
//! The guard is atomic across projects. The account lock (`budget-projects.lock`, the
//! registry's own lock) is taken first, the other projects are read under it, then the
//! caller's store runs `BEGIN IMMEDIATE`, checks, inserts and commits, and only then is
//! the account lock released. Two projects committing at once therefore cannot both pass
//! on the same balance. The live balance is read before the lock (it is a network call).
//!
//! Output that must not carry paths or free text (JSON, MCP status) uses the codes of
//! [`ReadSkip`] and [`crate::balances::unknown_code`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::balances::{self, Balance};
use crate::config::config_dir;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::lanes::{self, project_key};
use crate::store::{Budget, Provider, ReadSkip, Store, fits};

pub const PROJECTS_FILE: &str = "budget-projects.toml";
const LOCK_FILE: &str = "budget-projects.lock";

/// What the caps and totals do not include, as a machine-readable entry.
pub const UNCOUNTED_MANUAL: &str = "manual pods (offrig up, the app)";
/// The same, as the last line of the text report.
pub const UNCOUNTED_NOTE: &str = "Note: pods started by hand with `offrig up` or the offrig app are not counted against these caps or in the account totals.";
/// Printed by `offrig up` before it launches.
pub const MANUAL_POD_NOTICE: &str = "This pod is started by hand: it is not counted against any project's cap. The one-hour runway guard still applies.";

/// A registry that could not be read, as a code.
pub const NOTE_BUDGET_REGISTRY: &str = "budget_registry_unreadable";
pub const NOTE_LANE_REGISTRY: &str = "lane_registry_unreadable";

/// The sentence for a registry note code.
pub fn registry_note_text(code: &str) -> &'static str {
    match code {
        NOTE_LANE_REGISTRY => "the lane registry could not be read",
        _ => "the budget projects registry could not be read",
    }
}

/// The last path component of a project key: a folder name, never a path.
pub fn folder_name(key: &str) -> String {
    Path::new(key)
        .file_name()
        .map_or_else(|| "a project".into(), |n| n.to_string_lossy().into_owned())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    project: Vec<String>,
}

/// The registry of projects that have set a cap: canonical project paths.
#[derive(Debug, Clone)]
pub struct ProjectsRegistry {
    dir: PathBuf,
    lock_wait: Duration,
    lock_stale: Duration,
}

impl ProjectsRegistry {
    /// A registry in `dir` (the config directory, or a temp dir in tests).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        ProjectsRegistry {
            dir: dir.into(),
            lock_wait: Duration::from_secs(10),
            lock_stale: Duration::from_secs(30),
        }
    }

    pub fn open_default() -> Result<Self> {
        Ok(Self::at(config_dir()?))
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(PROJECTS_FILE)
    }

    /// The account lock: it guards this registry and, held across a guarded commit,
    /// the whole account.
    pub(crate) fn lock(&self) -> Result<fsutil::LockGuard> {
        fsutil::lock_file(
            &self.dir,
            LOCK_FILE,
            self.lock_wait,
            self.lock_stale,
            "the budget projects lock",
        )
    }

    fn read(&self) -> Result<Vec<String>> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(format!("reading {}", path.display()), e)),
        };
        let file: File =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        Ok(file.project)
    }

    /// Record a project (canonical path), once. Returns true when it was new. Runs under
    /// a lock file and replaces the registry atomically; a malformed registry is an
    /// error and is never replaced.
    pub fn add(&self, project: &Path) -> Result<bool> {
        let key = project_key(project);
        let _lock = self.lock()?;
        let mut projects = self.read()?;
        if projects.contains(&key) {
            return Ok(false);
        }
        projects.push(key);
        let mut text = String::from(
            "# offrig budget projects: every project that has set a spending cap. Do not edit.\n",
        );
        text.push_str(
            &toml::to_string_pretty(&File { project: projects })
                .map_err(|e| Error::Config(e.to_string()))?,
        );
        fsutil::write_atomic(&self.path(), text.as_bytes())?;
        Ok(true)
    }

    /// Every recorded project. A registry that does not exist yet means none, and is left
    /// alone (no folder, no lock file).
    pub fn all_if_present(&self) -> Result<Vec<String>> {
        if !self.path().exists() {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        self.read()
    }

    /// As [`ProjectsRegistry::all_if_present`] for a caller that already holds the lock.
    fn all_if_present_held(&self) -> Result<Vec<String>> {
        if !self.path().exists() {
            return Ok(Vec::new());
        }
        self.read()
    }
}

/// Where the account view looks: the current project and the two registries.
#[derive(Debug, Clone)]
pub struct Scope {
    pub current: PathBuf,
    pub projects: ProjectsRegistry,
    pub lanes: lanes::Registry,
}

impl Scope {
    /// The default registries (offrig's config directory) for `current`.
    pub fn default_for(current: &Path) -> Result<Self> {
        Ok(Scope {
            current: current.to_path_buf(),
            projects: ProjectsRegistry::open_default()?,
            lanes: lanes::Registry::open_default()?,
        })
    }

    /// The projects registry that sits in the same directory as the lane registry.
    pub fn beside(current: &Path, lanes: &lanes::Registry) -> Self {
        Scope {
            current: current.to_path_buf(),
            projects: ProjectsRegistry::at(lanes.dir()),
            lanes: lanes.clone(),
        }
    }

    fn collect(
        &self,
        projects: Result<Vec<String>>,
        lane_projects: Result<Vec<(String, lanes::Lane)>>,
    ) -> (Vec<String>, Vec<String>) {
        let mut notes = Vec::new();
        let mut keys = vec![project_key(&self.current)];
        let mut push = |k: String| {
            if !keys.contains(&k) {
                keys.push(k);
            }
        };
        match projects {
            Ok(v) => {
                for p in v {
                    push(project_key(Path::new(&p)));
                }
            }
            Err(_) => notes.push(NOTE_BUDGET_REGISTRY.to_string()),
        }
        match lane_projects {
            Ok(v) => {
                for (p, _) in v {
                    push(p);
                }
            }
            Err(_) => notes.push(NOTE_LANE_REGISTRY.to_string()),
        }
        (keys, notes)
    }

    /// Every known project key, current first, without duplicates, plus note codes for a
    /// registry that could not be read.
    pub fn known(&self) -> (Vec<String>, Vec<String>) {
        self.collect(self.projects.all_if_present(), self.lanes.all_if_present())
    }

    /// As [`Scope::known`] for a caller that already holds the account lock.
    fn known_held(&self) -> (Vec<String>, Vec<String>) {
        self.collect(
            self.projects.all_if_present_held(),
            self.lanes.all_if_present(),
        )
    }
}

/// What one provider's caps promise across the known projects.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountTotals {
    pub provider: Provider,
    /// Projects whose store was read.
    pub projects: usize,
    pub caps: f64,
    pub committed: f64,
    pub spent: f64,
    /// Cap minus committed minus spent, summed: what the caps still promise.
    pub unspent: f64,
    /// Projects that could not be read, as `(folder, reason)`; left out of the sums.
    pub skipped: Vec<(String, ReadSkip)>,
}

impl AccountTotals {
    /// Human notes for the skipped projects (folder names only).
    pub fn notes(&self) -> Vec<String> {
        self.skipped
            .iter()
            .map(|(f, why)| format!("project {f}: not counted ({})", why.text()))
            .collect()
    }

    /// The skipped projects as `<folder>: <code>`, for JSON.
    pub fn note_codes(&self) -> Vec<String> {
        self.skipped
            .iter()
            .map(|(f, why)| format!("{f}: {}", why.code()))
            .collect()
    }

    /// `account runpod  caps $X across N projects  committed $C  unspent $U  balance $B`.
    pub fn line(&self, balance: &Balance) -> String {
        format!(
            "account {:<10}  caps ${:.2} across {} project{}  committed ${:.2}  unspent ${:.2}  balance {}",
            self.provider.as_str(),
            self.caps,
            self.projects,
            if self.projects == 1 { "" } else { "s" },
            self.committed,
            self.unspent,
            balance.text()
        )
    }

    /// The caps promise more than the account holds.
    pub fn warning(&self, balance: &Balance) -> Option<String> {
        let have = balance.known()?;
        (self.unspent > have + 1e-9).then(|| {
            format!(
                "WARNING: {} caps across projects promise ${:.2} unspent but the account holds ${have:.2}",
                self.provider, self.unspent
            )
        })
    }
}

/// One project's budget for a provider, read from its own store without changing it.
/// A busy store is retried once.
fn read_project(key: &str, provider: Provider) -> std::result::Result<Budget, ReadSkip> {
    let db = Path::new(key).join(".offrig").join("offrig.db");
    let mut attempt = 0;
    loop {
        let got = Store::probe_read_only(&db).and_then(|store| {
            store
                .budget_for(provider)
                .map_err(|e| ReadSkip::of_error(&e))
        });
        match got {
            Err(ReadSkip::Busy) if attempt == 0 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(150));
            }
            other => return other,
        }
    }
}

/// Sum the provider's cap, committed and spent over `keys`. The `current` project (its
/// key and its open store) is read through that store; every other store is opened
/// read-only.
pub fn account_totals(
    provider: Provider,
    keys: &[String],
    current: Option<(&str, &Store)>,
) -> AccountTotals {
    let mut t = AccountTotals {
        provider,
        projects: 0,
        caps: 0.0,
        committed: 0.0,
        spent: 0.0,
        unspent: 0.0,
        skipped: Vec::new(),
    };
    for key in keys {
        let got = match current {
            Some((ck, store)) if ck == key => store
                .budget_for(provider)
                .map_err(|e| ReadSkip::of_error(&e)),
            _ => read_project(key, provider),
        };
        match got {
            Ok(b) => {
                t.projects += 1;
                t.caps += b.cap;
                t.committed += b.committed;
                t.spent += b.spent;
                t.unspent += b.remaining;
            }
            Err(why) => t.skipped.push((folder_name(key), why)),
        }
    }
    t
}

/// The account check for guarded commits: the live balance (read before any lock) and
/// where to find the other projects.
pub struct AccountGuard {
    balance: Balance,
    scope: Scope,
    notes: Mutex<Vec<String>>,
    /// Test hook: runs while the account lock is held, after the other projects are read.
    #[cfg(test)]
    pause: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

impl AccountGuard {
    /// A guard for `scope` and an already-read balance.
    pub fn new(scope: Scope, balance: Balance) -> Self {
        AccountGuard {
            balance,
            scope,
            notes: Mutex::new(Vec::new()),
            #[cfg(test)]
            pause: None,
        }
    }

    /// Read the balance with `balance` (the live read, injected so tests never reach the
    /// network). Do this before the commit: it is a network call.
    pub fn read(provider: Provider, scope: Scope, balance: &dyn Fn(Provider) -> Balance) -> Self {
        Self::new(scope, balance(provider))
    }

    pub fn balance(&self) -> &Balance {
        &self.balance
    }

    fn note(&self, n: String) {
        let mut g = self.notes.lock().unwrap_or_else(|e| e.into_inner());
        if !g.contains(&n) {
            g.push(n);
        }
    }

    /// What the guard has to report after a commit: the account check was skipped, or a
    /// registry could not be read. Codes only, no paths.
    pub fn notes(&self) -> Vec<String> {
        self.notes.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// When the balance could not be read the account check is skipped: say so.
    pub fn skipped_note(&self, provider: Provider) -> Option<String> {
        match &self.balance {
            Balance::Known(_) => None,
            Balance::Unknown(why) => Some(format!(
                "account check skipped: the {} balance could not be read ({}); proceeding under the per-project caps",
                provider.label(),
                balances::unknown_code(why)
            )),
        }
    }

    /// Take the account lock and read the other projects under it. `None` (and a note)
    /// when the balance is unknown: nothing is blocked then. A known project whose store
    /// is present but unreadable refuses the commit. The returned [`Held`] keeps the
    /// lock until it is dropped, which the store does after its own `COMMIT`.
    pub(crate) fn enter(&self, provider: Provider) -> Result<Option<Held>> {
        let Some(have) = self.balance.known() else {
            if let Some(n) = self.skipped_note(provider) {
                self.note(n);
            }
            return Ok(None);
        };
        let lock = self.scope.projects.lock()?;
        let (keys, registry_notes) = self.scope.known_held();
        for n in registry_notes {
            self.note(format!("{} ({n})", registry_note_text(&n)));
        }
        let me = project_key(&self.scope.current);
        let mut others = 0.0;
        for key in keys.iter().filter(|k| **k != me) {
            match read_project(key, provider) {
                Ok(b) => others += b.committed,
                Err(ReadSkip::NoStore) => {}
                Err(skip) => {
                    return Err(Error::Budget(format!(
                        "the {} account check cannot count project {}: {}. Fix that project's store or remove it from the budget projects list; nothing was committed",
                        provider.label(),
                        folder_name(key),
                        skip.text()
                    )));
                }
            }
        }
        #[cfg(test)]
        if let Some(p) = &self.pause {
            p();
        }
        Ok(Some(Held {
            _lock: lock,
            provider,
            balance: have,
            others_committed: others,
        }))
    }
}

/// An account lock held across one commit, with what the other projects have committed.
pub struct Held {
    _lock: fsutil::LockGuard,
    provider: Provider,
    balance: f64,
    others_committed: f64,
}

impl Held {
    /// The commitment of `amount` (a `what`, "plan" or "completion") plus what this
    /// project has already committed (`own_committed`, read by the store under its write
    /// lock) plus the other projects' committed money must fit the live balance: the
    /// account holds all of it.
    pub fn check(&self, amount: f64, own_committed: f64, what: &str) -> Result<()> {
        if fits(amount + own_committed + self.others_committed, self.balance) {
            return Ok(());
        }
        Err(Error::Budget(format!(
            "the {} account holds ${:.2}; this project holds ${own_committed:.2} committed; other projects hold ${:.2} committed; this {what} needs ${amount:.2}. Wait for those to finish, or lower the amount",
            self.provider.label(),
            self.balance,
            self.others_committed
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::{NewCompletion, NewPlan};
    use std::sync::Arc;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "offrig-account-{name}-{}-{}",
            std::process::id(),
            crate::cost::now_unix()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        d
    }

    fn new_plan(price: f64) -> NewPlan {
        NewPlan {
            profile: "p".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 1.0,
            max_price_hr: price,
            note: None,
        }
    }

    fn db(p: &Path) -> PathBuf {
        p.join(".offrig").join("offrig.db")
    }

    fn project_at(p: &Path) -> Store {
        std::fs::create_dir_all(p.join(".offrig")).expect("dir");
        Store::open(&db(p)).expect("store")
    }

    /// A project folder with a store: RunPod cap `cap`, and `commit` committed.
    fn project(root: &Path, name: &str, cap: f64, commit: f64) -> PathBuf {
        let p = root.join(name);
        let s = project_at(&p);
        s.set_provider_cap(Provider::RunPod, cap).expect("cap");
        if commit > 0.0 {
            let plan = s.create_plan(new_plan(commit)).expect("plan");
            s.commit_plan(plan.id).expect("commit");
        }
        p
    }

    fn scope(root: &Path, current: &Path) -> Scope {
        Scope {
            current: current.to_path_buf(),
            projects: ProjectsRegistry::at(root.join("cfg")),
            lanes: lanes::Registry::at(root.join("cfg")),
        }
    }

    fn guard(sc: &Scope, balance: Balance) -> AccountGuard {
        AccountGuard::new(sc.clone(), balance)
    }

    #[test]
    fn the_registry_appends_dedupes_and_keeps_what_it_had() {
        let root = tmp("registry");
        let reg = ProjectsRegistry::at(root.join("cfg"));
        assert!(reg.all_if_present().expect("none").is_empty());
        assert!(!root.join("cfg").exists(), "looking creates nothing");
        let (a, b) = (project(&root, "a", 5.0, 0.0), project(&root, "b", 5.0, 0.0));
        assert!(reg.add(&a).expect("add"));
        assert!(!reg.add(&a).expect("again"), "no duplicate");
        assert!(reg.add(&b).expect("add b"));
        // A second handle merges with what is on disk.
        let other = ProjectsRegistry::at(root.join("cfg"));
        assert!(!other.add(&b).expect("dup b"));
        let all = other.all_if_present().expect("all");
        assert_eq!(all, vec![project_key(&a), project_key(&b)]);
        assert!(!root.join("cfg").join(LOCK_FILE).exists(), "lock released");
    }

    #[test]
    fn a_malformed_registry_is_an_error_and_is_not_replaced() {
        let root = tmp("malformed");
        let reg = ProjectsRegistry::at(root.join("cfg"));
        std::fs::create_dir_all(root.join("cfg")).expect("dir");
        std::fs::write(reg.path(), "project = [").expect("write");
        assert!(reg.add(&root).is_err());
        assert_eq!(
            std::fs::read_to_string(reg.path()).expect("read"),
            "project = ["
        );
    }

    #[test]
    fn known_projects_merge_the_registries_and_the_current_one() {
        let root = tmp("known");
        let (a, b, c) = (
            project(&root, "a", 5.0, 0.0),
            project(&root, "b", 5.0, 0.0),
            project(&root, "c", 5.0, 0.0),
        );
        let sc = scope(&root, &a);
        sc.projects.add(&b).expect("b");
        sc.projects.add(&a).expect("a again");
        // c is only in the lane registry.
        sc.lanes
            .lane_for_project(&c, &Config::default())
            .expect("lane");
        let (keys, notes) = sc.known();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys,
            vec![project_key(&a), project_key(&b), project_key(&c)]
        );
    }

    #[test]
    fn totals_sum_projects_and_note_the_unreadable_ones_by_code() {
        let root = tmp("totals");
        let a = project(&root, "a", 30.0, 10.0);
        let b = project(&root, "b", 20.0, 0.0);
        let broken = root.join("broken");
        std::fs::create_dir_all(broken.join(".offrig")).expect("dir");
        std::fs::write(db(&broken), "not a database").expect("junk");
        let missing = root.join("missing");
        let keys = vec![
            project_key(&a),
            project_key(&b),
            project_key(&broken),
            project_key(&missing),
        ];
        let cur = Store::open(&db(&a)).expect("open");
        let t = account_totals(Provider::RunPod, &keys, Some((&keys[0], &cur)));
        assert_eq!(t.projects, 2);
        assert!((t.caps - 50.0).abs() < 1e-9 && (t.committed - 10.0).abs() < 1e-9);
        assert!((t.unspent - 40.0).abs() < 1e-9, "{t:?}");
        assert_eq!(
            t.skipped,
            vec![
                ("broken".to_string(), ReadSkip::Unreadable),
                ("missing".to_string(), ReadSkip::NoStore)
            ]
        );
        assert_eq!(
            t.note_codes(),
            vec!["broken: unreadable", "missing: no_store"]
        );
        assert!(t.notes()[1].contains("project missing: not counted (it has no offrig store)"));
        let line = t.line(&Balance::Known(100.0));
        assert!(line.starts_with("account runpod"), "{line}");
        assert!(line.contains("across 2 projects"), "{line}");
        assert!(
            line.contains("caps $50.00") && line.contains("unspent $40.00"),
            "{line}"
        );
    }

    #[test]
    fn a_corrupt_cap_and_a_newer_schema_are_classified() {
        let root = tmp("classify");
        let a = project(&root, "a", 30.0, 0.0);
        let b = project(&root, "b", 20.0, 0.0);
        let c = project(&root, "c", 20.0, 0.0);
        let s = Store::open(&db(&b)).expect("store");
        s.set_setting("budget_cap.runpod", "garbage").expect("set");
        drop(s);
        rusqlite::Connection::open(db(&c))
            .and_then(|c| c.execute_batch("PRAGMA user_version = 99;"))
            .expect("v99");
        let keys = vec![project_key(&a), project_key(&b), project_key(&c)];
        let t = account_totals(Provider::RunPod, &keys, None);
        assert_eq!(t.projects, 1);
        assert_eq!(
            t.skipped,
            vec![
                ("b".to_string(), ReadSkip::Unreadable),
                ("c".to_string(), ReadSkip::SchemaNewer)
            ]
        );
    }

    #[test]
    fn unspent_above_the_balance_warns_and_unknown_does_not() {
        let t = AccountTotals {
            provider: Provider::RunPod,
            projects: 3,
            caps: 90.0,
            committed: 10.0,
            spent: 5.0,
            unspent: 75.0,
            skipped: vec![],
        };
        let w = t.warning(&Balance::Known(40.0)).expect("warns");
        assert_eq!(
            w,
            "WARNING: runpod caps across projects promise $75.00 unspent but the account holds $40.00"
        );
        assert!(t.warning(&Balance::Known(75.0)).is_none());
        assert!(t.warning(&Balance::Unknown("no key".into())).is_none());
    }

    #[test]
    fn the_guard_refuses_when_other_projects_committed_plus_this_exceeds_the_balance() {
        let root = tmp("guard");
        let me = project(&root, "me", 100.0, 0.0);
        let other = project(&root, "other", 100.0, 30.0);
        let sc = scope(&root, &me);
        sc.projects.add(&other).expect("reg");
        let store = Store::open(&db(&me)).expect("store");
        // Balance 40, the other project holds 30: a 20 plan does not fit.
        let g = guard(&sc, Balance::Known(40.0));
        let big = store.create_plan(new_plan(20.0)).expect("plan");
        let msg = store
            .commit_plan_with_account(big.id, &g)
            .expect_err("refused")
            .to_string();
        assert!(
            msg.contains("RunPod account holds $40.00")
                && msg.contains("this project holds $0.00 committed")
                && msg.contains("other projects hold $30.00 committed")
                && msg.contains("needs $20.00"),
            "{msg}"
        );
        assert_eq!(store.plan(big.id).expect("p").expect("p").state, "planned");
        assert_eq!(
            store.budget_for(Provider::RunPod).expect("b").committed,
            0.0
        );
        assert!(!root.join("cfg").join(LOCK_FILE).exists(), "lock released");
        // A 10 plan fits exactly.
        let small = store.create_plan(new_plan(10.0)).expect("plan");
        store.commit_plan_with_account(small.id, &g).expect("fits");
    }

    #[test]
    fn the_guard_counts_this_projects_own_committed_money_too() {
        let root = tmp("own");
        let me = project(&root, "me", 100.0, 20.0);
        let other = project(&root, "other", 100.0, 10.0);
        let sc = scope(&root, &me);
        sc.projects.add(&other).expect("reg");
        let store = Store::open(&db(&me)).expect("store");
        let g = guard(&sc, Balance::Known(50.0));
        // 20 own + 10 other + 25 new = 55 > 50: refused, though 25 + 10 alone would fit.
        let big = store.create_plan(new_plan(25.0)).expect("plan");
        let msg = store
            .commit_plan_with_account(big.id, &g)
            .expect_err("refused")
            .to_string();
        assert!(
            msg.contains("account holds $50.00")
                && msg.contains("this project holds $20.00 committed")
                && msg.contains("other projects hold $10.00 committed")
                && msg.contains("needs $25.00"),
            "{msg}"
        );
        // 20 + 10 + 20 = 50 fits exactly.
        let ok = store.create_plan(new_plan(20.0)).expect("plan");
        store.commit_plan_with_account(ok.id, &g).expect("fits");
        // Completions count the project's own OpenRouter commitments the same way.
        let og = guard(&sc, Balance::Known(5.0));
        store
            .set_provider_cap(Provider::OpenRouter, 50.0)
            .expect("cap");
        let c = |worst: f64| NewCompletion {
            model: "m".into(),
            lane: "plain".into(),
            input_bound: 1,
            max_tokens: 1,
            price_in_m: 1.0,
            price_out_m: 1.0,
            worst_case: worst,
        };
        store
            .commit_completion_with_account(c(3.0), &og)
            .expect("3 fits 5");
        let err = store
            .commit_completion_with_account(c(3.0), &og)
            .expect_err("3 + 3 > 5");
        assert!(
            err.to_string()
                .contains("this project holds $3.00 committed"),
            "{err}"
        );
    }

    #[test]
    fn the_guard_fails_closed_on_a_known_project_it_cannot_read_but_not_on_a_missing_one() {
        let root = tmp("closed");
        let me = project(&root, "me", 100.0, 0.0);
        let sc = scope(&root, &me);
        let store = Store::open(&db(&me)).expect("store");
        let g = guard(&sc, Balance::Known(500.0));
        // No store at all: it holds 0, and the commit goes through.
        sc.projects.add(&root.join("nowhere")).expect("reg");
        let p = store.create_plan(new_plan(1.0)).expect("plan");
        store
            .commit_plan_with_account(p.id, &g)
            .expect("no store is 0");

        type Make = Box<dyn Fn(&Path)>;
        let cases: [(&str, Make, &str); 3] = [
            (
                "junk",
                Box::new(|p| {
                    std::fs::create_dir_all(p.join(".offrig")).expect("dir");
                    std::fs::write(db(p), "not a database at all, just text long enough")
                        .expect("junk");
                }),
                "could not be read",
            ),
            (
                "newer",
                Box::new(|p| {
                    drop(project_at(p));
                    rusqlite::Connection::open(db(p))
                        .and_then(|c| c.execute_batch("PRAGMA user_version = 99;"))
                        .expect("v99");
                }),
                "newer offrig",
            ),
            (
                "badcap",
                Box::new(|p| {
                    let s = project_at(p);
                    s.set_setting("budget_cap.runpod", "garbage").expect("set");
                }),
                "could not be read",
            ),
        ];
        for (name, make, want) in cases {
            let bad = root.join(name);
            make(&bad);
            sc.projects.add(&bad).expect("reg");
            let p = store.create_plan(new_plan(1.0)).expect("plan");
            let msg = store
                .commit_plan_with_account(p.id, &g)
                .expect_err("refused")
                .to_string();
            assert!(
                msg.contains(&format!("project {name}")) && msg.contains(want),
                "{name}: {msg}"
            );
            assert!(
                !msg.contains(root.to_string_lossy().as_ref()),
                "folder name only: {msg}"
            );
            assert_eq!(store.plan(p.id).expect("p").expect("p").state, "planned");
            // Take it out again so the next case is judged alone.
            let text = std::fs::read_to_string(sc.projects.path()).expect("read");
            let key = project_key(&bad);
            let kept: Vec<&str> = text.lines().filter(|l| !l.contains(&key)).collect();
            std::fs::write(sc.projects.path(), kept.join("\n")).expect("rewrite");
        }
        // An unknown balance never blocks, even with unreadable projects around.
        let none = guard(&sc, Balance::Unknown("no key".into()));
        let p = store.create_plan(new_plan(1.0)).expect("plan");
        store
            .commit_plan_with_account(p.id, &none)
            .expect("not blocked");
    }

    #[test]
    fn two_projects_committing_at_once_cannot_together_exceed_the_balance() {
        let root = tmp("race");
        let a = project(&root, "a", 100.0, 0.0);
        let b = project(&root, "b", 100.0, 0.0);
        let sc_a = scope(&root, &a);
        sc_a.projects.add(&a).expect("reg a");
        sc_a.projects.add(&b).expect("reg b");
        let sc_b = scope(&root, &b);
        // Balance 30; each project wants 20. Without the account lock held across the
        // read and the commit, both would read 0 for the other and both would pass.
        // Project A sleeps while holding the lock, long enough for B to arrive.
        let run = |sc: Scope, dir: PathBuf, pause: bool| {
            std::thread::spawn(move || {
                let store = Store::open(&db(&dir)).expect("store");
                let plan = store.create_plan(new_plan(20.0)).expect("plan");
                let mut g = guard(&sc, Balance::Known(30.0));
                if pause {
                    g.pause = Some(Arc::new(|| {
                        std::thread::sleep(Duration::from_millis(400));
                    }));
                }
                store.commit_plan_with_account(plan.id, &g).is_ok()
            })
        };
        let ta = run(sc_a, a.clone(), true);
        std::thread::sleep(Duration::from_millis(100));
        let tb = run(sc_b, b.clone(), false);
        let (ok_a, ok_b) = (ta.join().expect("a"), tb.join().expect("b"));
        assert!(ok_a, "the first to arrive commits");
        assert!(!ok_b, "the second sees the first's 20 and is refused");
        let total: f64 = [&a, &b]
            .iter()
            .map(|p| {
                Store::open(&db(p))
                    .expect("s")
                    .budget_for(Provider::RunPod)
                    .expect("b")
                    .committed
            })
            .sum();
        assert!(total <= 30.0 + 1e-9, "{total}");
        assert!(!root.join("cfg").join(LOCK_FILE).exists(), "lock released");
    }

    #[test]
    fn the_guard_proceeds_with_a_note_when_the_balance_is_unknown() {
        let root = tmp("unknown");
        let me = project(&root, "me", 100.0, 0.0);
        let other = project(&root, "other", 100.0, 30.0);
        let sc = scope(&root, &me);
        sc.projects.add(&other).expect("reg");
        let store = Store::open(&db(&me)).expect("store");
        let plan = store.create_plan(new_plan(50.0)).expect("plan");
        let g = guard(&sc, Balance::Unknown("RUNPOD_API_KEY is not set".into()));
        store
            .commit_plan_with_account(plan.id, &g)
            .expect("not blocked");
        let notes = g.notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("skipped") && notes[0].contains("(no_key)"),
            "{notes:?}"
        );
        assert!(!notes[0].contains("RUNPOD_API_KEY"), "code, not text");
        let known = guard(&sc, Balance::Known(1.0));
        assert!(known.skipped_note(Provider::RunPod).is_none());
    }

    #[test]
    fn the_guard_covers_openrouter_completions_and_project_caps_still_apply() {
        let root = tmp("or");
        let me = project(&root, "me", 100.0, 0.0);
        let store = Store::open(&db(&me)).expect("store");
        store
            .set_provider_cap(Provider::OpenRouter, 50.0)
            .expect("cap");
        let sc = scope(&root, &me);
        let g = guard(&sc, Balance::Known(3.0));
        let c = |worst: f64| NewCompletion {
            model: "m".into(),
            lane: "plain".into(),
            input_bound: 10,
            max_tokens: 10,
            price_in_m: 1.0,
            price_out_m: 1.0,
            worst_case: worst,
        };
        let err = store
            .commit_completion_with_account(c(5.0), &g)
            .expect_err("refused");
        assert!(
            err.to_string().contains("OpenRouter account holds $3.00"),
            "{err}"
        );
        store
            .commit_completion_with_account(c(2.5), &g)
            .expect("fits");
        // The per-project cap still refuses first.
        let big = guard(&sc, Balance::Known(500.0));
        assert!(store.commit_completion_with_account(c(60.0), &big).is_err());
    }
}
