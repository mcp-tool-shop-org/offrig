//! The account view: caps are per project, money is per provider account.
//!
//! Each project's store holds its own caps, so two projects can each promise the same
//! RunPod balance. This module reads across projects, strictly read-only, to show what
//! all the caps together promise ([`account_totals`]) and to refuse a commitment the
//! account could not pay once the other projects' committed money is counted
//! ([`AccountView`], used by `Store::commit_plan_with_account` and
//! `Store::commit_completion_with_account`).
//!
//! Which projects are known: the registry `<config dir>/budget-projects.toml` (a project
//! is added whenever `offrig budget` sets a cap there), the lane registry's projects and
//! the current one. A project whose store is missing, locked, corrupt or from another
//! offrig becomes a note and is left out; it is never an error.
//!
//! The guard lives in the store's commit (so it sits next to the per-project checks and
//! shares their write lock) but takes a view that the caller read beforehand: the
//! balance is a network read, and the store must not wait on the network while holding
//! the write lock. The balance reader is a parameter, so tests never touch the network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::balances::Balance;
use crate::config::config_dir;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::lanes::{self, project_key};
use crate::store::{Budget, Provider, Store, fits};

pub const PROJECTS_FILE: &str = "budget-projects.toml";
const LOCK_FILE: &str = "budget-projects.lock";

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

    fn lock(&self) -> Result<fsutil::LockGuard> {
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

    /// Every known project key, current first, without duplicates, plus notes for a
    /// registry that could not be read.
    pub fn known(&self) -> (Vec<String>, Vec<String>) {
        let mut notes = Vec::new();
        let mut keys = vec![project_key(&self.current)];
        let mut push = |k: String| {
            if !keys.contains(&k) {
                keys.push(k);
            }
        };
        match self.projects.all_if_present() {
            Ok(v) => {
                for p in v {
                    push(project_key(Path::new(&p)));
                }
            }
            Err(e) => notes.push(format!(
                "the budget projects registry could not be read: {e}"
            )),
        }
        match self.lanes.all_if_present() {
            Ok(v) => {
                for (p, _) in v {
                    push(p);
                }
            }
            Err(e) => notes.push(format!("the lane registry could not be read: {e}")),
        }
        (keys, notes)
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
    /// Projects that could not be read, and why; they are left out of the sums.
    pub notes: Vec<String>,
    /// The same projects as `(project key, reason)`, for output that must not carry text.
    pub skipped: Vec<(String, String)>,
}

impl AccountTotals {
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
fn read_project(key: &str, provider: Provider) -> std::result::Result<Budget, String> {
    let db = Path::new(key).join(".offrig").join("offrig.db");
    if !db.is_file() {
        return Err("it has no offrig store".into());
    }
    let store = Store::open_read_only(&db).map_err(|e| e.to_string())?;
    store.budget_for(provider).map_err(|e| e.to_string())
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
        notes: Vec::new(),
        skipped: Vec::new(),
    };
    for key in keys {
        let got = match current {
            Some((ck, store)) if ck == key => store.budget_for(provider).map_err(|e| e.to_string()),
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
            Err(why) => {
                t.notes.push(format!("project {key}: not counted ({why})"));
                t.skipped.push((key.clone(), why));
            }
        }
    }
    t
}

/// What the account check needs, read before a commit: the provider's live balance and
/// the committed money of every other known project.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    pub balance: Balance,
    /// Committed by the other known projects (not this one).
    pub others_committed: f64,
    /// Other projects that could not be read, so their committed money is not counted.
    pub notes: Vec<String>,
}

impl AccountView {
    /// Read the view for `provider` as seen from `scope.current`. `balance` is the live
    /// read, injected so tests never reach the network.
    pub fn read(
        provider: Provider,
        scope: &Scope,
        balance: &dyn Fn(Provider) -> Balance,
    ) -> AccountView {
        let (keys, mut notes) = scope.known();
        let me = project_key(&scope.current);
        let mut others = 0.0;
        for key in keys.iter().filter(|k| **k != me) {
            match read_project(key, provider) {
                Ok(b) => others += b.committed,
                Err(why) => notes.push(format!("project {key}: not counted ({why})")),
            }
        }
        AccountView {
            balance: balance(provider),
            others_committed: others,
            notes,
        }
    }

    /// The commitment of `amount` (a `what`, "plan" or "completion") plus what this
    /// project has already committed (`own_committed`, read by the store under its write
    /// lock) plus the other projects' committed money must fit the live balance: the
    /// account holds all of it. An unknown balance never blocks: see
    /// [`AccountView::skipped_note`].
    pub fn check(
        &self,
        provider: Provider,
        amount: f64,
        own_committed: f64,
        what: &str,
    ) -> Result<()> {
        let Some(have) = self.balance.known() else {
            return Ok(());
        };
        if fits(amount + own_committed + self.others_committed, have) {
            return Ok(());
        }
        Err(Error::Budget(format!(
            "the {} account holds ${have:.2}; this project holds ${own_committed:.2} committed; other projects hold ${:.2} committed; this {what} needs ${amount:.2}. Wait for those to finish, or lower the amount",
            provider.label(),
            self.others_committed
        )))
    }

    /// When the balance could not be read the account check was skipped: say so.
    pub fn skipped_note(&self, provider: Provider) -> Option<String> {
        match &self.balance {
            Balance::Known(_) => None,
            Balance::Unknown(why) => Some(format!(
                "account check skipped: the {} balance could not be read ({why}); proceeding under the per-project caps",
                provider.label()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::store::{NewCompletion, NewPlan};

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

    /// A project folder with a store: RunPod cap `cap`, and `commit` committed.
    fn project(root: &Path, name: &str, cap: f64, commit: f64) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.join(".offrig")).expect("dir");
        let s = Store::open(&p.join(".offrig").join("offrig.db")).expect("store");
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
    fn totals_sum_projects_and_note_the_unreadable_ones() {
        let root = tmp("totals");
        let a = project(&root, "a", 30.0, 10.0);
        let b = project(&root, "b", 20.0, 0.0);
        let broken = root.join("broken");
        std::fs::create_dir_all(broken.join(".offrig")).expect("dir");
        std::fs::write(broken.join(".offrig").join("offrig.db"), "not a database").expect("junk");
        let missing = root.join("missing");
        let keys = vec![
            project_key(&a),
            project_key(&b),
            project_key(&broken),
            project_key(&missing),
        ];
        let cur = Store::open(&a.join(".offrig").join("offrig.db")).expect("open");
        let t = account_totals(Provider::RunPod, &keys, Some((&keys[0], &cur)));
        assert_eq!(t.projects, 2);
        assert!((t.caps - 50.0).abs() < 1e-9 && (t.committed - 10.0).abs() < 1e-9);
        assert!((t.unspent - 40.0).abs() < 1e-9, "{t:?}");
        assert_eq!(t.notes.len(), 2, "{:?}", t.notes);
        assert!(t.notes.iter().any(|n| n.contains("no offrig store")));
        let line = t.line(&Balance::Known(100.0));
        assert!(line.starts_with("account runpod"), "{line}");
        assert!(line.contains("across 2 projects"), "{line}");
        assert!(
            line.contains("caps $50.00") && line.contains("unspent $40.00"),
            "{line}"
        );
    }

    #[test]
    fn a_corrupt_cap_excludes_that_project_with_a_note() {
        let root = tmp("corruptcap");
        let a = project(&root, "a", 30.0, 0.0);
        let b = project(&root, "b", 20.0, 0.0);
        let s = Store::open(&b.join(".offrig").join("offrig.db")).expect("store");
        s.set_setting("budget_cap.runpod", "garbage").expect("set");
        drop(s);
        let keys = vec![project_key(&a), project_key(&b)];
        let t = account_totals(Provider::RunPod, &keys, None);
        assert_eq!(t.projects, 1);
        assert_eq!(t.notes.len(), 1);
        assert!(t.notes[0].contains("not a usable cap"), "{:?}", t.notes);
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
            notes: vec![],
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
        let store = Store::open(&me.join(".offrig").join("offrig.db")).expect("store");
        // Balance 40, the other project holds 30: a 20 plan does not fit.
        let view = AccountView::read(Provider::RunPod, &sc, &|_| Balance::Known(40.0));
        assert!((view.others_committed - 30.0).abs() < 1e-9);
        let big = store.create_plan(new_plan(20.0)).expect("plan");
        let msg = store
            .commit_plan_with_account(big.id, &view)
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
        // A 10 plan fits exactly.
        let small = store.create_plan(new_plan(10.0)).expect("plan");
        store
            .commit_plan_with_account(small.id, &view)
            .expect("fits");
    }

    #[test]
    fn the_guard_counts_this_projects_own_committed_money_too() {
        let root = tmp("own");
        let me = project(&root, "me", 100.0, 20.0);
        let other = project(&root, "other", 100.0, 10.0);
        let sc = scope(&root, &me);
        sc.projects.add(&other).expect("reg");
        let store = Store::open(&me.join(".offrig").join("offrig.db")).expect("store");
        let view = AccountView::read(Provider::RunPod, &sc, &|_| Balance::Known(50.0));
        // 20 own + 10 other + 25 new = 55 > 50: refused, though 25 + 10 alone would fit.
        let big = store.create_plan(new_plan(25.0)).expect("plan");
        let msg = store
            .commit_plan_with_account(big.id, &view)
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
        store.commit_plan_with_account(ok.id, &view).expect("fits");
        // Completions count the project's own OpenRouter commitments the same way.
        let or_view = AccountView::read(Provider::OpenRouter, &sc, &|_| Balance::Known(5.0));
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
            .commit_completion_with_account(c(3.0), &or_view)
            .expect("3 fits 5");
        let err = store
            .commit_completion_with_account(c(3.0), &or_view)
            .expect_err("3 + 3 > 5");
        assert!(
            err.to_string()
                .contains("this project holds $3.00 committed"),
            "{err}"
        );
    }

    #[test]
    fn the_guard_proceeds_with_a_note_when_the_balance_is_unknown() {
        let root = tmp("unknown");
        let me = project(&root, "me", 100.0, 0.0);
        let other = project(&root, "other", 100.0, 30.0);
        let sc = scope(&root, &me);
        sc.projects.add(&other).expect("reg");
        let store = Store::open(&me.join(".offrig").join("offrig.db")).expect("store");
        let plan = store.create_plan(new_plan(50.0)).expect("plan");
        let view = AccountView::read(Provider::RunPod, &sc, &|_| {
            Balance::Unknown("no RunPod key".into())
        });
        store
            .commit_plan_with_account(plan.id, &view)
            .expect("not blocked");
        let note = view.skipped_note(Provider::RunPod).expect("note");
        assert!(
            note.contains("skipped") && note.contains("no RunPod key"),
            "{note}"
        );
        assert!(
            AccountView::read(Provider::RunPod, &sc, &|_| Balance::Known(1.0))
                .skipped_note(Provider::RunPod)
                .is_none()
        );
    }

    #[test]
    fn the_guard_covers_openrouter_completions_and_project_caps_still_apply() {
        let root = tmp("or");
        let me = project(&root, "me", 100.0, 0.0);
        let store = Store::open(&me.join(".offrig").join("offrig.db")).expect("store");
        store
            .set_provider_cap(Provider::OpenRouter, 50.0)
            .expect("cap");
        let sc = scope(&root, &me);
        let view = AccountView::read(Provider::OpenRouter, &sc, &|_| Balance::Known(3.0));
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
            .commit_completion_with_account(c(5.0), &view)
            .expect_err("refused");
        assert!(
            err.to_string().contains("OpenRouter account holds $3.00"),
            "{err}"
        );
        store
            .commit_completion_with_account(c(2.5), &view)
            .expect("fits");
        // The per-project cap still refuses first.
        let big = AccountView::read(Provider::OpenRouter, &sc, &|_| Balance::Known(500.0));
        assert!(store.commit_completion_with_account(c(60.0), &big).is_err());
    }
}
