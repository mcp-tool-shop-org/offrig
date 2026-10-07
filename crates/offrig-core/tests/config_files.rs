//! Config files and the validation rules that guard them, plus the environment-driven
//! locations (`OFFRIG_CONFIG_DIR`, `ROLE_OS_DIR`), which run in a child process so the
//! test never changes its own environment.

mod support;

use offrig_core::config::{self, Config, Engine};
use offrig_core::error::Error;
use support::temp;

/// The default config with `f` applied.
fn tweaked(f: impl FnOnce(&mut Config)) -> Config {
    let mut cfg = Config::default();
    f(&mut cfg);
    cfg
}

fn is_config_err(e: &Error, needle: &str) -> bool {
    matches!(e, Error::Config(m) if m.contains(needle))
}

#[test]
fn a_missing_file_is_the_default_and_a_saved_one_round_trips() {
    let dir = temp("cfg-roundtrip");
    let path = dir.join("nested").join("config.toml");
    let loaded = Config::load_from(&path).expect("missing is default");
    assert_eq!(loaded.active_profile, Config::default().active_profile);

    let cfg = tweaked(|c| {
        c.active_profile = "frontier".into();
        c.ssh_alias = "mypod".into();
    });
    cfg.save_to(&path).expect("save creates the folder");
    let back = Config::load_from(&path).expect("load");
    assert_eq!(back.active_profile, "frontier");
    assert_eq!(back.ssh_alias, "mypod");
    assert_eq!(back.profiles, cfg.profiles);
    assert_eq!(back.active().expect("active").name, "frontier");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_malformed_or_invalid_file_is_an_error_never_a_silent_default() {
    let dir = temp("cfg-bad");
    let path = dir.join("config.toml");
    std::fs::write(&path, "this is = not [toml").expect("write");
    let err = Config::load_from(&path).expect_err("malformed");
    assert!(is_config_err(&err, "config.toml"), "names the file: {err}");

    // Well-formed TOML that breaks a rule is refused at load.
    let cfg = tweaked(|c| c.tunnel_port = config::LOCAL_OLLAMA_PORT);
    let text = toml::to_string_pretty(&cfg).expect("toml");
    std::fs::write(&path, text).expect("write");
    let err = Config::load_from(&path).expect_err("invalid");
    assert!(is_config_err(&err, "local Ollama port"), "{err}");

    // A directory where the file should be cannot be read.
    let as_dir = dir.join("is-a-dir");
    std::fs::create_dir_all(&as_dir).expect("dir");
    let err = Config::load_from(&as_dir).expect_err("not a file");
    assert!(matches!(err, Error::Io { .. }), "{err}");

    // An invalid config is not written.
    let target = dir.join("never.toml");
    let err = cfg.save_to(&target).expect_err("refused");
    assert!(is_config_err(&err, "local Ollama port"), "{err}");
    assert!(!target.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_validation_rule_names_its_problem() {
    let mut cfg = tweaked(|c| c.ssh_alias = "two words".into());
    assert!(is_config_err(
        &cfg.validate().expect_err("alias"),
        "one word"
    ));
    cfg.ssh_alias = String::new();
    assert!(is_config_err(
        &cfg.validate().expect_err("alias"),
        "one word"
    ));

    let cfg = tweaked(|c| c.zed_provider = "RunPod".into());
    assert!(is_config_err(
        &cfg.validate().expect_err("provider"),
        "RUNPOD_API_KEY"
    ));

    let edit = |f: &dyn Fn(&mut config::Profile)| {
        let mut cfg = Config::default();
        f(cfg
            .profiles
            .iter_mut()
            .find(|p| p.name == "medium")
            .expect("medium"));
        cfg.validate().expect_err("the edit breaks a rule")
    };
    assert!(is_config_err(
        &edit(&|p| p.gpu_count = 0),
        "needs a gpu type and a gpu count"
    ));
    assert!(is_config_err(
        &edit(&|p| p.gpu_type_ids.clear()),
        "needs a gpu type and a gpu count"
    ));
    assert!(is_config_err(
        &edit(&|p| p.min_cuda = Some("99.9".into())),
        "min_cuda"
    ));
    assert!(is_config_err(
        &edit(&|p| p.min_vram_gb = Some(0)),
        "min_vram_gb must be above 0"
    ));
    assert!(is_config_err(
        &edit(&|p| {
            p.network_volume_id = Some("vol".into());
            p.data_center_id = None;
        }),
        "needs data_center_id"
    ));
    // A recipe is checked through the profile that carries it.
    assert!(is_config_err(
        &edit(&|p| {
            let mut r = Config::default()
                .profile("frontier")
                .expect("frontier")
                .recipe
                .clone()
                .expect("recipe");
            r.image = "lmsysorg/sglang:latest".into();
            p.recipe = Some(r);
        }),
        "pinned tag"
    ));
}

#[test]
fn recipes_need_a_pinned_image_a_repo_id_clean_args_and_a_secret_name() {
    let frontier = Config::default();
    let base = frontier
        .profile("frontier")
        .expect("frontier")
        .recipe
        .clone()
        .expect("recipe");
    assert_eq!(base.engine, Engine::Sglang);
    assert!(base.validate("frontier", 1).is_ok());
    let check = |f: &dyn Fn(&mut config::Recipe), needle: &str| {
        let mut r = base.clone();
        f(&mut r);
        let err = r.validate("frontier", 1).expect_err("rule");
        assert!(is_config_err(&err, needle), "{needle}: {err}");
    };
    check(&|r| r.image = "no-tag".into(), "pinned tag");
    check(&|r| r.image = "x:".into(), "pinned tag");
    check(&|r| r.model = "  ".into(), "Hugging Face repo id");
    check(&|r| r.model = "two words".into(), "Hugging Face repo id");
    check(&|r| r.args.push(String::new()), "one token");
    check(&|r| r.args.push("a b".into()), "one token");
    check(
        &|r| r.hf_token_secret = Some(String::new()),
        "RunPod secret name",
    );
    check(
        &|r| r.hf_token_secret = Some("bad name!".into()),
        "RunPod secret name",
    );
    let mut ok = base.clone();
    ok.hf_token_secret = Some("HF_TOKEN-1".into());
    assert!(ok.validate("frontier", 1).is_ok());
    let p = frontier.profile("frontier").expect("frontier");
    assert_eq!(p.engine(), Some(Engine::Sglang));
    assert_eq!(frontier.profile("medium").expect("medium").engine(), None);
}

#[test]
fn an_unknown_profile_is_a_config_error() {
    let cfg = Config::default();
    let err = cfg.profile("nope").expect_err("unknown");
    assert!(is_config_err(&err, "no profile named nope"), "{err}");
    let mut cfg = cfg;
    cfg.active_profile = "gone".into();
    assert!(cfg.active().is_err());
}

#[test]
fn cuda_versions_compare_numerically_and_unparseable_ones_lose() {
    assert_eq!(config::parse_cuda("12.8"), Some((12, 8)));
    assert_eq!(config::parse_cuda(" 12.10 "), Some((12, 10)));
    assert_eq!(config::parse_cuda("12"), None);
    assert_eq!(config::parse_cuda("12.x"), None);
    assert_eq!(config::parse_cuda("x.8"), None);
    assert_eq!(config::newer_cuda("12.8", "12.10"), "12.10");
    assert_eq!(config::newer_cuda("12.10", "12.8"), "12.10");
    assert_eq!(config::newer_cuda("junk", "12.4"), "12.4");
    assert_eq!(config::newer_cuda("12.4", "junk"), "12.4");
    assert_eq!(config::cuda_meets("12.8", "12.4"), Some(true));
    assert_eq!(config::cuda_meets("12.2", "12.4"), Some(false));
    assert_eq!(config::cuda_meets("12.8", "junk"), None);
    assert_eq!(config::cuda_meets("junk", "12.8"), None);
}

#[test]
fn the_effective_cuda_floor_is_the_newer_of_the_profiles_and_the_jobs() {
    let mut cfg = Config::default();
    let job = cfg
        .profiles
        .iter_mut()
        .find(|p| p.name == "job")
        .expect("job profile");
    let image_floor = job.job.as_ref().and_then(|j| j.min_cuda.clone());
    job.min_cuda = None;
    assert_eq!(
        job.effective_min_cuda(),
        image_floor,
        "the image's floor alone"
    );
    job.min_cuda = Some("13.0".into());
    assert_eq!(
        job.effective_min_cuda().as_deref(),
        Some(config::newer_cuda(
            "13.0",
            image_floor.as_deref().unwrap_or("0.0")
        )),
    );
    job.job = None;
    assert_eq!(
        job.effective_min_cuda().as_deref(),
        Some("13.0"),
        "own floor alone"
    );
    job.min_cuda = None;
    assert_eq!(job.effective_min_cuda(), None);
}

#[test]
fn config_dir_follows_offrig_config_dir_when_set() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        let dir = std::path::PathBuf::from(std::env::var_os("OFFRIG_CONFIG_DIR").expect("set"));
        assert_eq!(config::config_dir().expect("dir"), dir);
        assert_eq!(
            config::config_path().expect("path"),
            dir.join("config.toml")
        );
        // load() / save() use that path.
        let loaded = Config::load().expect("missing is default");
        assert_eq!(loaded.ssh_alias, "offrig");
        let cfg = tweaked(|c| c.ssh_alias = "viaenv".into());
        cfg.save().expect("save");
        assert!(dir.join("config.toml").is_file());
        assert_eq!(Config::load().expect("load").ssh_alias, "viaenv");
        return;
    }
    let dir = temp("cfg-env");
    support::reexec(
        "config_dir_follows_offrig_config_dir_when_set",
        &[("OFFRIG_CONFIG_DIR", &dir.display().to_string())],
        &[],
    );
    assert!(dir.join("config.toml").is_file(), "the child wrote there");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_offrig_config_dir_falls_back_to_the_user_config_dir() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        let dir = config::config_dir().expect("dir");
        assert!(dir.ends_with("offrig"), "{}", dir.display());
        return;
    }
    support::reexec(
        "an_empty_offrig_config_dir_falls_back_to_the_user_config_dir",
        &[("OFFRIG_CONFIG_DIR", "")],
        &[],
    );
}

#[test]
fn role_os_dir_comes_from_the_environment_when_set() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        assert_eq!(
            config::default_role_os_dir().as_deref(),
            std::env::var("ROLE_OS_DIR").ok().as_deref()
        );
        assert_eq!(
            Config::default().role_os_dir.as_deref(),
            std::env::var("ROLE_OS_DIR").ok().as_deref()
        );
        return;
    }
    support::reexec(
        "role_os_dir_comes_from_the_environment_when_set",
        &[("ROLE_OS_DIR", "some/role-os")],
        &[],
    );
}

#[test]
fn a_blank_role_os_dir_is_ignored() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        let found = config::default_role_os_dir();
        assert_ne!(found.as_deref(), Some("   "));
        return;
    }
    support::reexec(
        "a_blank_role_os_dir_is_ignored",
        &[("ROLE_OS_DIR", "   ")],
        &[],
    );
}
