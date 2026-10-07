//! The lane registry's refusals and lookups: hand-edited files, a full registry, lock
//! failures, and the side-car's lazily allocated view of its lane.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use offrig_core::config::Config;
use offrig_core::error::Error;
use offrig_core::lanes::{LANE_COUNT, Lane, LaneCtx, Registry};
use offrig_core::store::{NewPlan, Store};
use support::temp;

fn project(root: &Path, name: &str) -> PathBuf {
    let p = root.join("projects").join(name);
    std::fs::create_dir_all(&p).expect("project dir");
    p
}

fn lane_file(tag: &str, alias: &str, port: u16, project: &str) -> String {
    format!(
        "[[lane]]\nproject = \"{project}\"\ntag = \"{tag}\"\nssh_alias = \"{alias}\"\ntunnel_port = {port}\n"
    )
}

fn refuses(reg: &Registry, root: &Path, needle: &str) {
    let err = reg
        .lane_for_project(&project(root, "other"), &Config::default())
        .expect_err("a bad registry is refused");
    assert!(
        matches!(&err, Error::Config(m) if m.contains(needle)),
        "{needle}: {err}"
    );
}

#[test]
fn hand_edited_registries_that_break_a_rule_are_refused() {
    let d = temp("lane-validate");
    let reg = Registry::at(&d);
    for (tag, alias, why) in [
        ("Bad_Tag", "offrig-Bad_Tag", "must be 1-24"),
        ("-edge", "offrig--edge", "must be 1-24"),
        ("", "offrig-", "must be 1-24"),
        ("a", "something-else", "must use the alias offrig-a"),
    ] {
        std::fs::write(reg.path(), lane_file(tag, alias, 11500, "/p")).expect("write");
        refuses(&reg, &d, why);
    }
    // Two lanes that share a tag, an alias, a port or a project.
    let one = lane_file("a", "offrig-a", 11500, "/p1");
    for second in [
        lane_file("a", "offrig-a", 11502, "/p2"),
        lane_file("b", "offrig-b", 11500, "/p2"),
        lane_file("b", "offrig-b", 11502, "/p1"),
    ] {
        std::fs::write(reg.path(), format!("{one}{second}")).expect("write");
        refuses(&reg, &d, "share a project, tag, alias or port");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_registry_holds_a_fixed_number_of_lanes_and_says_when_it_is_full() {
    let d = temp("lane-full");
    let reg = Registry::at(&d);
    let base = Config::default();
    let mut ports = std::collections::HashSet::new();
    for i in 0..LANE_COUNT {
        let lane = reg
            .lane_for_project(&project(&d, &format!("proj{i}")), &base)
            .unwrap_or_else(|e| panic!("lane {i}: {e}"));
        assert!(ports.insert(lane.tunnel_port), "ports are never shared");
    }
    let err = reg
        .lane_for_project(&project(&d, "one-too-many"), &base)
        .expect_err("no lane left");
    assert!(
        matches!(&err, Error::Config(m) if m.contains("lanes are in use")),
        "{err}"
    );
    assert_eq!(reg.all().expect("all").len(), usize::from(LANE_COUNT));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn lookups_find_what_was_allocated_and_allocate_nothing() {
    let d = temp("lane-find");
    let reg = Registry::at(&d);
    let base = Config::default();
    let p = project(&d, "alpha");
    assert!(
        reg.find_project(&p).expect("no file yet").is_none(),
        "no registry file, no lane"
    );
    assert!(!reg.path().exists(), "looking allocates nothing");
    assert!(reg.find_tag("alpha").expect("empty").is_none());
    assert!(reg.all().expect("empty").is_empty());

    let lane = reg.lane_for_project(&p, &base).expect("allocate");
    assert_eq!(lane.tag.as_deref(), Some("alpha"));
    assert_eq!(lane.label(), "alpha");
    assert_eq!(reg.find_project(&p).expect("find"), Some(lane.clone()));
    assert!(
        reg.find_project(&project(&d, "beta"))
            .expect("other")
            .is_none()
    );
    assert_eq!(reg.find_tag("alpha").expect("tag"), Some(lane.clone()));
    assert!(reg.find_tag("beta").expect("tag").is_none());
    let all = reg.all().expect("all");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].1, lane);
    assert_eq!(Lane::plain(&base).label(), "plain");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn folder_names_without_letters_still_get_a_tag() {
    let d = temp("lane-names");
    let reg = Registry::at(&d);
    let lane = reg
        .lane_for_project(&project(&d, "!!!"), &Config::default())
        .expect("allocate");
    assert_eq!(lane.tag.as_deref(), Some("project"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_unreadable_registry_or_lock_is_an_error_not_a_hang() {
    let d = temp("lane-io");
    let reg = Registry::at(&d);
    // A directory where the registry file should be.
    std::fs::create_dir_all(reg.path()).expect("dir");
    let err = reg.all().expect_err("cannot read a directory");
    assert!(matches!(err, Error::Io { .. }), "{err}");
    std::fs::remove_dir_all(reg.path()).expect("cleanup");

    // A live lock held by someone else is waited for, then reported.
    let quick =
        Registry::at(&d).with_lock_timing(Duration::from_millis(100), Duration::from_secs(600));
    std::fs::write(d.join("lanes.lock"), b"").expect("lock");
    let err = quick.all().expect_err("lock never freed");
    assert!(matches!(err, Error::Timeout(_)), "{err}");
    std::fs::remove_file(d.join("lanes.lock")).expect("unlock");

    // The registry folder cannot be made under a file.
    let blocker = d.join("blocker");
    std::fs::write(&blocker, b"x").expect("write");
    let err = Registry::at(blocker.join("sub"))
        .all()
        .expect_err("no folder");
    assert!(matches!(err, Error::Io { .. }), "{err}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_registry_in_the_default_config_directory_follows_the_environment() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        let dir = PathBuf::from(std::env::var_os("OFFRIG_CONFIG_DIR").expect("set"));
        let reg = Registry::open_default().expect("default registry");
        assert_eq!(reg.path(), dir.join("lanes.toml"));
        return;
    }
    let d = temp("lane-default");
    support::reexec(
        "a_registry_in_the_default_config_directory_follows_the_environment",
        &[("OFFRIG_CONFIG_DIR", &d.display().to_string())],
        &[],
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_sidecars_lane_is_allocated_on_first_need_and_found_without_allocating() {
    let d = temp("lane-ctx");
    let reg = Registry::at(&d);
    let p = project(&d, "gamma");
    let ctx = LaneCtx::new(Config::default(), &p, reg.clone());
    assert_eq!(
        ctx.own_if_allocated().expect("peek"),
        None,
        "merely asking claims no lane"
    );
    assert!(!reg.path().exists());
    let lane = ctx.own().expect("allocate");
    assert_eq!(ctx.own().expect("cached"), lane);
    assert_eq!(ctx.own_if_allocated().expect("peek"), Some(lane.clone()));
    let cfg = ctx.own_cfg().expect("lane config");
    assert_eq!(cfg.ssh_alias, lane.ssh_alias);
    assert_eq!(cfg.tunnel_port, lane.tunnel_port);

    // A second side-car for the same project finds the lane without allocating one.
    let other = LaneCtx::new(Config::default(), &p, reg.clone());
    assert_eq!(other.own_if_allocated().expect("peek"), Some(lane.clone()));

    // Configs by tag: none is the plain lane, a known tag its lane, an unknown one is refused.
    assert_eq!(ctx.cfg_for_tag(None).expect("plain").ssh_alias, "offrig");
    assert_eq!(
        ctx.cfg_for_tag(lane.tag.as_deref())
            .expect("lane")
            .ssh_alias,
        lane.ssh_alias
    );
    let err = ctx
        .cfg_for_tag(Some("ghost"))
        .expect_err("not in the registry");
    assert!(
        matches!(&err, Error::Refused(m) if m.contains("\"ghost\" is not in")),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_planned_plan_adopts_the_projects_lane_and_a_recorded_lane_is_followed() {
    let d = temp("lane-adopt");
    let reg = Registry::at(&d);
    let ctx = LaneCtx::new(Config::default(), &project(&d, "delta"), reg);
    let store = Store::open_in_memory().expect("store");
    store.set_budget_cap(100.0).expect("cap");
    let planned = store
        .create_plan(NewPlan {
            profile: "frontier".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 1.0,
            max_price_hr: 3.0,
            note: None,
        })
        .expect("plan");
    assert_eq!(store.plan_lane(planned.id).expect("lane"), None);
    ctx.adopt(&store, &planned).expect("adopt");
    let lane = ctx.own().expect("lane");
    assert_eq!(store.plan_lane(planned.id).expect("lane"), lane.tag);
    // A plan that already recorded a lane is left as is; its config is that lane's.
    ctx.adopt(&store, &planned).expect("adopt again");
    let cfg = ctx.cfg_for_plan(&store, &planned).expect("config");
    assert_eq!(cfg.ssh_alias, lane.ssh_alias);

    // A committed plan with no lane predates lanes: the plain lane's.
    let old = store
        .create_plan(NewPlan {
            profile: "frontier".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 1.0,
            max_price_hr: 3.0,
            note: None,
        })
        .expect("plan");
    store.commit_plan(old.id).expect("commit");
    let old = store.plan(old.id).expect("read").expect("exists");
    ctx.adopt(&store, &old)
        .expect("committed plans are not adopted");
    assert_eq!(store.plan_lane(old.id).expect("lane"), None);
    assert_eq!(
        ctx.cfg_for_plan(&store, &old).expect("plain").ssh_alias,
        "offrig"
    );
    // A fresh planned plan takes this project's lane without recording it.
    let fresh = store
        .create_plan(NewPlan {
            profile: "frontier".into(),
            gpu_count: 1,
            gpu_types: vec![],
            max_hours: 0.5,
            max_price_hr: 3.0,
            note: None,
        })
        .expect("plan");
    assert_eq!(
        ctx.cfg_for_plan(&store, &fresh).expect("own").ssh_alias,
        lane.ssh_alias
    );
    let _ = std::fs::remove_dir_all(&d);
}
