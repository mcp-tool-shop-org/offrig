//! The project database's refusals and edge cases: bad inputs, the transition law, the
//! budget, jobs, schema migrations and files from a newer offrig.

mod support;

use offrig_core::error::Error;
use offrig_core::store::{Kind, NewHandoff, NewPlan, NewRecord, State, Store};
use support::temp;

fn store() -> Store {
    Store::open_in_memory().expect("in-memory store")
}

fn refused(e: &Error, needle: &str) -> bool {
    matches!(e, Error::Refused(m) if m.contains(needle))
}

fn plan(hours: f64, price: f64) -> NewPlan {
    NewPlan {
        profile: "frontier".into(),
        gpu_count: 1,
        gpu_types: vec!["NVIDIA H200".into()],
        max_hours: hours,
        max_price_hr: price,
        note: None,
    }
}

fn handoff(s: &Store) -> i64 {
    s.add_handoff(NewHandoff {
        role_id: "game-designer".into(),
        mission: "design verbs".into(),
        acceptance: "has a Verbs heading".into(),
        ..Default::default()
    })
    .expect("handoff")
}

#[test]
fn kinds_and_states_parse_both_ways_and_unknown_names_are_refused() {
    for k in ["brief", "constraint", "decision", "fact", "checkpoint"] {
        assert_eq!(Kind::parse(k).expect("known").as_str(), k);
    }
    let err = Kind::parse("rumour").expect_err("unknown kind");
    assert!(refused(&err, "unknown record kind"), "{err}");
    for st in [
        "pending",
        "dispatched",
        "running",
        "complete",
        "failed",
        "timed_out",
        "invalid_output",
        "ownership_violation",
        "review",
    ] {
        assert_eq!(State::parse(st).expect("known").as_str(), st);
    }
    let err = State::parse("lost").expect_err("unknown state");
    assert!(refused(&err, "unknown handoff state"), "{err}");
}

#[test]
fn the_transition_law_lets_failed_and_timed_out_handoffs_be_retried() {
    assert_eq!(State::Failed.allowed(), [State::Dispatched]);
    assert_eq!(State::TimedOut.allowed(), [State::Dispatched]);
    assert!(State::Complete.allowed().is_empty());
    assert!(State::InvalidOutput.blocked() && State::OwnershipViolation.blocked());
    assert!(!State::Failed.blocked());
    assert!(State::Complete.terminal() && !State::Review.terminal());
    assert!(State::Running.in_flight() && !State::Review.in_flight());
}

#[test]
fn a_transition_names_its_reason_and_explains_a_refusal() {
    let s = store();
    let id = handoff(&s);
    // Pending may only be dispatched; the refusal lists what is allowed.
    let err = s
        .transition(id, State::Complete, "skip ahead", None)
        .expect_err("not lawful");
    assert!(err.to_string().contains("allowed: dispatched"), "{err}");
    // An empty reason is refused even for a lawful move.
    let err = s
        .transition(id, State::Dispatched, "  ", None)
        .expect_err("needs a reason");
    assert!(refused(&err, "needs a reason"), "{err}");
    assert_eq!(
        s.handoff(id).expect("read").expect("exists").state,
        State::Pending,
        "a refused move changes nothing"
    );
    s.transition(id, State::Dispatched, "go", None)
        .expect("dispatch");
    // Failed handoffs can be sent out again; attempts count the dispatches.
    s.transition(id, State::Failed, "boom", None).expect("fail");
    let again = s
        .transition(id, State::Dispatched, "retry", None)
        .expect("retry");
    assert_eq!(again.attempts, 2);
    // An unknown handoff is refused, not a panic.
    assert!(refused(
        &s.transition(999, State::Dispatched, "x", None)
            .expect_err("unknown"),
        "no handoff 999"
    ));
    // Complete is terminal, and the refusal says so.
    s.transition(id, State::Complete, "done", None)
        .expect("complete");
    let err = s
        .transition(id, State::Dispatched, "again", None)
        .expect_err("terminal");
    assert!(err.to_string().contains("complete is terminal"), "{err}");
}

#[test]
fn blocked_states_move_only_with_an_override_that_is_logged() {
    let s = store();
    let id = handoff(&s);
    s.transition(id, State::Dispatched, "go", None)
        .expect("dispatch");
    s.transition(
        id,
        State::OwnershipViolation,
        "touched a file outside scope",
        None,
    )
    .expect("flag");
    let err = s
        .transition(id, State::Dispatched, "retry", None)
        .expect_err("blocked");
    assert!(err.to_string().contains("override reason"), "{err}");
    let err = s
        .transition(id, State::Dispatched, "retry", Some("   "))
        .expect_err("a blank override is no override");
    assert!(matches!(err, Error::Transition { .. }), "{err}");
    s.transition(
        id,
        State::Dispatched,
        "retry",
        Some("owner approved the extra file"),
    )
    .expect("override");
    let events = s.events(id).expect("events");
    let last = events.last().expect("an event");
    assert_eq!(last.1, "dispatched");
    assert!(last.2.starts_with("OVERRIDE: owner approved"), "{last:?}");
    assert!(last.3, "the override flag is stored");
}

#[test]
fn heartbeats_keep_in_flight_handoffs_alive_and_run_details_are_recorded() {
    let s = store();
    let id = handoff(&s);
    s.set_run_details(id, "qwen3-coder", "rolehash", "prompthash")
        .expect("details");
    let h = s.handoff(id).expect("read").expect("exists");
    assert_eq!(h.model.as_deref(), Some("qwen3-coder"));
    assert_eq!(h.role_hash.as_deref(), Some("rolehash"));
    assert_eq!(h.prompt_hash.as_deref(), Some("prompthash"));
    s.transition(id, State::Dispatched, "go", None)
        .expect("dispatch");
    let before = s.handoff(id).expect("read").expect("exists").updated_at;
    s.heartbeat(id).expect("beat");
    let after = s.handoff(id).expect("read").expect("exists").updated_at;
    assert!(after >= before);
    // Stale ones are timed out by the reaper, using the same predicate as status.
    let now = after + 1_000;
    assert_eq!(s.reap_stale(now, 60).expect("reap"), vec![id]);
    assert_eq!(
        s.handoff(id).expect("read").expect("exists").state,
        State::TimedOut
    );
    // A heartbeat for a handoff that is no longer in flight is a harmless no-op.
    s.heartbeat(id).expect("no-op");
    assert!(s.reap_stale(now, 60).expect("reap").is_empty());
}

#[test]
fn handoffs_are_validated_when_added() {
    let s = store();
    let new = |role: &str, mission: &str| NewHandoff {
        role_id: role.into(),
        mission: mission.into(),
        acceptance: "x".into(),
        ..Default::default()
    };
    assert!(refused(
        &s.add_handoff(new(" ", "m")).expect_err("no role"),
        "needs a role and a mission"
    ));
    assert!(refused(
        &s.add_handoff(new("game-designer", " "))
            .expect_err("no mission"),
        "needs a role and a mission"
    ));
    assert!(refused(
        &s.add_handoff(NewHandoff {
            acceptance: " ".into(),
            ..new("game-designer", "m")
        })
        .expect_err("no acceptance"),
        "acceptance check"
    ));
    assert!(refused(
        &s.add_handoff(NewHandoff {
            accept_on_checks: true,
            ..new("game-designer", "m")
        })
        .expect_err("accept without checks"),
        "accept_on_checks needs checks"
    ));
    assert!(refused(
        &s.add_handoff(NewHandoff {
            depends_on: vec![42],
            ..new("game-designer", "m")
        })
        .expect_err("unknown dependency"),
        "handoff 42"
    ));
}

#[test]
fn ready_handoffs_wait_for_their_dependencies() {
    let s = store();
    let a = handoff(&s);
    let b = s
        .add_handoff(NewHandoff {
            role_id: "game-designer".into(),
            mission: "uses a".into(),
            acceptance: "x".into(),
            depends_on: vec![a],
            ..Default::default()
        })
        .expect("b");
    let ids = |s: &Store| {
        s.ready()
            .expect("ready")
            .iter()
            .map(|h| h.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&s), vec![a]);
    s.transition(a, State::Dispatched, "go", None)
        .expect("dispatch");
    s.transition(a, State::Complete, "done", None)
        .expect("complete");
    assert_eq!(ids(&s), vec![b]);
}

#[test]
fn records_refuse_the_unknown_and_the_inactive() {
    let s = store();
    let rec = |body: &str| NewRecord {
        kind: Some(Kind::Decision),
        body: body.into(),
        author: "t".into(),
        ..Default::default()
    };
    let err = s
        .record(NewRecord {
            supersedes: Some(77),
            reason: Some("because".into()),
            ..rec("new")
        })
        .expect_err("nothing to supersede");
    assert!(refused(&err, "no record 77 to supersede"), "{err}");
    let id = s.record(rec("one")).expect("record");
    s.withdraw(id, "wrong").expect("withdraw");
    let err = s.withdraw(id, "again").expect_err("already withdrawn");
    assert!(refused(&err, "is not active"), "{err}");
    assert!(s.get(12345).expect("read").is_none());
}

#[test]
fn the_budget_cap_and_plan_prices_must_be_sane() {
    let s = store();
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(refused(
            &s.set_budget_cap(bad).expect_err("bad cap"),
            "non-negative"
        ));
    }
    s.set_budget_cap(10.0).expect("cap");
    assert!(refused(
        &s.create_plan(plan(0.0, 3.0)).expect_err("no hours"),
        "max_hours must be a positive number"
    ));
    assert!(refused(
        &s.create_plan(plan(f64::INFINITY, 3.0))
            .expect_err("endless"),
        "max_hours"
    ));
    assert!(refused(
        &s.create_plan(plan(1.0, 0.0)).expect_err("free"),
        "max_price_hr must be positive"
    ));
    assert!(matches!(
        s.create_plan(plan(10.0, 3.0)).expect_err("over the cap"),
        Error::Budget(_)
    ));
}

#[test]
fn a_plan_commits_once_and_only_while_the_budget_has_room() {
    let s = store();
    s.set_budget_cap(10.0).expect("cap");
    let a = s.create_plan(plan(2.0, 3.0)).expect("a: $6");
    let b = s
        .create_plan(plan(1.0, 3.5))
        .expect("b: $3.50, fits while a is only planned");
    let committed = s.commit_plan(a.id).expect("commit a");
    assert_eq!(committed.state, "committed");
    assert_eq!(
        s.commit_plan(a.id).expect("idempotent").committed_at,
        committed.committed_at
    );
    // $10 - $6 committed leaves $4; b needs $3.50 and fits, a second 3.5 would not.
    s.commit_plan(b.id).expect("commit b");
    let c = s
        .create_plan(plan(0.2, 3.0))
        .expect_err("only $0.50 is left for planning");
    assert!(matches!(c, Error::Budget(_)));
    assert!(refused(
        &s.commit_plan(404).expect_err("unknown"),
        "no plan 404"
    ));
    // A closed plan cannot be committed again.
    s.close_plan(a.id, 1.0).expect("close");
    assert!(refused(&s.commit_plan(a.id).expect_err("closed"), "plan"));
}

#[test]
fn a_commit_that_no_longer_fits_is_refused_at_commit_time() {
    let s = store();
    s.set_budget_cap(10.0).expect("cap");
    let a = s.create_plan(plan(2.0, 3.0)).expect("a: $6");
    let b = s
        .create_plan(plan(2.0, 3.0))
        .expect("b: $6, both only planned");
    s.commit_plan(a.id).expect("commit a");
    let err = s.commit_plan(b.id).expect_err("$4 left, b needs $6");
    assert!(refused(&err, "only $4.00 of the budget is left"), "{err}");
}

#[test]
fn closing_releases_the_commit_and_cancelling_a_planned_plan_spends_nothing() {
    let s = store();
    s.set_budget_cap(20.0).expect("cap");
    let planned = s.create_plan(plan(1.0, 4.0)).expect("planned");
    let b = s.close_plan(planned.id, 0.0).expect("cancel");
    assert!((b.remaining - 20.0).abs() < 1e-9);
    assert_eq!(
        s.plan(planned.id).expect("read").expect("exists").state,
        "cancelled"
    );
    // Closing again is a no-op that just reports the budget.
    let again = s.close_plan(planned.id, 5.0).expect("idempotent");
    assert!((again.spent - b.spent).abs() < 1e-9);
    assert!(refused(
        &s.close_plan(999, 0.0).expect_err("unknown"),
        "no plan 999"
    ));

    let live = s.create_plan(plan(1.0, 4.0)).expect("live");
    s.commit_plan(live.id).expect("commit");
    let after = s.close_plan(live.id, 1.5).expect("close");
    assert!((after.spent - 1.5).abs() < 1e-9 && after.committed.abs() < 1e-9);
    assert!((after.remaining - 18.5).abs() < 1e-9);
    assert!(s.open_plans().expect("open").is_empty());
}

#[test]
fn jobs_and_per_plan_settings_round_trip() {
    let s = store();
    s.set_budget_cap(50.0).expect("cap");
    let p = s.create_plan(plan(1.0, 3.0)).expect("plan");
    assert!(s.job(1).expect("read").is_none());
    let id = s.create_job(p.id, "launch").expect("job");
    let j = s.job(id).expect("read").expect("exists");
    assert_eq!((j.kind.as_str(), j.state.as_str()), ("launch", "running"));
    s.update_job(
        id,
        "failed",
        &serde_json::json!({"step": 3}),
        Some("no capacity"),
    )
    .expect("update");
    let j = s.job(id).expect("read").expect("exists");
    assert_eq!(j.state, "failed");
    assert_eq!(j.progress["step"], 3);
    assert_eq!(j.error.as_deref(), Some("no capacity"));
    assert_eq!(
        s.job_for_plan(p.id, "launch")
            .expect("read")
            .expect("job")
            .id,
        id
    );
    assert!(s.job_for_plan(p.id, "other").expect("read").is_none());

    assert_eq!(s.plan_lane(p.id).expect("lane"), None);
    s.set_plan_lane(p.id, "proj").expect("set");
    assert_eq!(s.plan_lane(p.id).expect("lane").as_deref(), Some("proj"));
    s.set_plan_min_cuda(p.id, "12.8").expect("set");
    assert_eq!(
        s.plan_min_cuda(p.id).expect("cuda").as_deref(),
        Some("12.8")
    );
    assert_eq!(s.plan_wait_minutes(p.id).expect("wait"), None);
    s.set_plan_wait_minutes(p.id, 45).expect("set");
    assert_eq!(s.plan_wait_minutes(p.id).expect("wait"), Some(45));
    assert_eq!(s.plan_container_disk_gb(p.id).expect("disk"), None);
    s.set_plan_container_disk_gb(p.id, 80).expect("set");
    assert_eq!(s.plan_container_disk_gb(p.id).expect("disk"), Some(80));
    s.set_setting("plan_wait_minutes:1", "soon").expect("set");
    assert_eq!(s.plan_wait_minutes(1).expect("unparseable"), None);
}

#[test]
fn the_journal_lists_side_effects_with_no_outcome() {
    let s = store();
    let a = s
        .journal(
            "launch",
            Some(1),
            &serde_json::json!({"profile": "frontier"}),
        )
        .expect("journal");
    let b = s
        .journal("delete", None, &serde_json::json!({}))
        .expect("journal");
    s.journal_outcome(a, "ok").expect("outcome");
    let open = s.unfinished_journal().expect("open");
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].id, open[0].action.as_str()), (b, "delete"));
    assert_eq!(open[0].plan_id, None);
}

#[test]
fn a_file_database_persists_and_creates_its_folder() {
    let dir = temp("store-file");
    let path = dir.join("nested").join("project.db");
    {
        let s = Store::open(&path).expect("open creates the folder");
        s.record(NewRecord {
            kind: Some(Kind::Fact),
            body: "persisted across opens".into(),
            author: "t".into(),
            ..Default::default()
        })
        .expect("record");
    }
    let s = Store::open(&path).expect("reopen");
    assert_eq!(s.active(Kind::Fact).expect("read").len(), 1);
    drop(s);
    // A file where the folder should be cannot be opened.
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").expect("write");
    assert!(Store::open(&blocker.join("p.db")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_database_from_a_newer_offrig_is_refused_untouched() {
    let dir = temp("store-newer");
    let path = dir.join("project.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn.execute_batch("PRAGMA user_version = 99;")
            .expect("version");
    }
    let err = Store::open(&path).err().expect("a newer schema is refused");
    assert!(refused(&err, "schema v99"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn older_databases_are_migrated_in_place_and_keep_their_rows() {
    let dir = temp("store-migrate");
    let path = dir.join("project.db");
    {
        // A v1 database: plans without the clock columns, handoffs without checks.
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn.execute_batch(
            "CREATE TABLE plans (
               id INTEGER PRIMARY KEY, profile TEXT NOT NULL, gpu_count INTEGER NOT NULL,
               gpu_types TEXT NOT NULL, max_hours REAL NOT NULL, max_price_hr REAL NOT NULL,
               worst_case REAL NOT NULL,
               state TEXT NOT NULL DEFAULT 'planned' CHECK (state IN ('planned','committed','closed','cancelled')),
               pod_id TEXT, created_at INTEGER NOT NULL, note TEXT);
             INSERT INTO plans(profile, gpu_count, gpu_types, max_hours, max_price_hr, worst_case, created_at)
               VALUES ('frontier', 1, '[]', 1.0, 3.0, 3.0, 100);
             CREATE TABLE handoffs (
               id INTEGER PRIMARY KEY, role_id TEXT NOT NULL, mission TEXT NOT NULL,
               acceptance TEXT NOT NULL, scope TEXT NOT NULL DEFAULT '[]',
               depends_on TEXT NOT NULL DEFAULT '[]', state TEXT NOT NULL DEFAULT 'pending',
               attempts INTEGER NOT NULL DEFAULT 0, branch TEXT, model TEXT, role_hash TEXT,
               prompt_hash TEXT, result_record INTEGER, created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL);
             INSERT INTO handoffs(role_id, mission, acceptance, created_at, updated_at)
               VALUES ('game-designer', 'old mission', 'old check', 1, 1);
             PRAGMA user_version = 1;",
        )
        .expect("v1 schema");
    }
    let s = Store::open(&path).expect("migrates");
    let p = s.plan(1).expect("read").expect("old plan kept");
    assert_eq!(p.profile, "frontier");
    assert_eq!((p.committed_at, p.started_at), (None, None));
    let h = s.handoff(1).expect("read").expect("old handoff kept");
    assert_eq!(h.mission, "old mission");
    assert!(h.checks.is_empty() && !h.accept_on_checks);
    // The new columns work, and reopening the migrated file is a no-op.
    s.set_budget_cap(10.0).expect("cap");
    s.commit_plan(1).expect("commit uses committed_at");
    drop(s);
    let again = Store::open(&path).expect("reopen");
    assert!(
        again
            .plan(1)
            .expect("read")
            .expect("exists")
            .committed_at
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
