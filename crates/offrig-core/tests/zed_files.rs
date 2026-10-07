//! Zed's settings file: reading, writing with a one-time backup, and how malformed
//! files and odd shapes are handled. Paths are temp files; nothing touches real settings.

mod support;

use std::path::Path;

use offrig_core::error::Error;
use offrig_core::zed::{self, DefaultModel, ZedModel};
use support::temp;

fn model(name: &str) -> ZedModel {
    ZedModel {
        name: name.into(),
        display_name: format!("RunPod · {name}"),
        max_tokens: 65_536,
        tools: true,
        images: false,
    }
}

const P: &str = "settings.json";

#[test]
fn a_missing_settings_file_reads_as_an_empty_object() {
    let dir = temp("zed-missing");
    let text = zed::read_settings(&dir.join(P)).expect("missing is fine");
    assert_eq!(text, "{}\n");
    // And an empty object is a valid base for the first edit.
    let out = zed::apply_provider(
        &text,
        Path::new(P),
        "offrig",
        "http://127.0.0.1:11435/v1",
        &[model("m1")],
    )
    .expect("apply");
    assert!(out.contains("offrig") && out.contains("m1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_settings_path_is_an_io_error() {
    let dir = temp("zed-unreadable");
    let err = zed::read_settings(&dir).expect_err("a directory is not a file");
    assert!(matches!(err, Error::Io { .. }), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn writing_keeps_the_first_version_as_a_backup_and_creates_folders() {
    let dir = temp("zed-write");
    let path = dir.join("deep").join(P);
    zed::write_settings(&path, "{\"a\":1}\n").expect("first write, nothing to back up");
    assert!(!dir.join("deep").join("settings.json.offrig.bak").exists());
    zed::write_settings(&path, "{\"a\":2}\n").expect("second write");
    zed::write_settings(&path, "{\"a\":3}\n").expect("third write");
    assert_eq!(zed::read_settings(&path).expect("read"), "{\"a\":3}\n");
    let bak = dir.join("deep").join("settings.json.offrig.bak");
    assert_eq!(
        std::fs::read_to_string(&bak).expect("backup"),
        "{\"a\":1}\n",
        "the backup is the state before offrig's first change"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_settings_are_an_error_from_every_entry_point() {
    let bad = "{ \"a\": ";
    let path = Path::new("broken.json");
    let jsonc = |e: Error| matches!(e, Error::Jsonc { .. });
    assert!(jsonc(
        zed::apply_provider(bad, path, "offrig", "u", &[]).expect_err("apply")
    ));
    assert!(jsonc(
        zed::remove_provider(bad, path, "offrig").expect_err("remove")
    ));
    assert!(jsonc(
        zed::read_provider(bad, path, "offrig").expect_err("read")
    ));
    assert!(jsonc(zed::default_model(bad, path).expect_err("default")));
    assert!(jsonc(
        zed::set_default_model(
            bad,
            path,
            &DefaultModel {
                provider: "p".into(),
                model: "m".into()
            }
        )
        .expect_err("set default")
    ));
    assert!(jsonc(
        zed::local_ollama_models(bad, path).expect_err("local models")
    ));
}

#[test]
fn the_default_model_needs_both_a_provider_and_a_model_string() {
    let path = Path::new(P);
    let read = |t: &str| zed::default_model(t, path).expect("parses");
    assert_eq!(read("{}"), None);
    assert_eq!(read(r#"{"agent":{}}"#), None);
    assert_eq!(
        read(r#"{"agent":{"default_model":{"provider":"x"}}}"#),
        None
    );
    assert_eq!(
        read(r#"{"agent":{"default_model":{"provider":1,"model":"m"}}}"#),
        None
    );
    assert_eq!(
        read(r#"{"agent":{"default_model":{"provider":"p","model":"m"}}}"#),
        Some(DefaultModel {
            provider: "p".into(),
            model: "m".into()
        })
    );
}

#[test]
fn removing_a_provider_that_is_not_there_changes_nothing() {
    let path = Path::new(P);
    let text = "{\n  // keep me\n  \"theme\": \"One Dark\"\n}\n";
    assert_eq!(
        zed::remove_provider(text, path, "offrig").expect("remove"),
        text
    );
    assert_eq!(
        zed::read_provider(text, path, "offrig").expect("read"),
        None
    );
}

#[test]
fn a_provider_round_trips_and_other_settings_and_comments_survive() {
    let path = Path::new(P);
    let text = "{\n  // my theme\n  \"theme\": \"One Dark\",\n  \"language_models\": {\"ollama\": {\"available_models\": [{\"name\": \"local:8b\"}]}}\n}\n";
    let out = zed::apply_provider(
        text,
        path,
        "offrig",
        "http://127.0.0.1:11435/v1",
        &[model("pod-model")],
    )
    .expect("apply");
    assert!(out.contains("// my theme") && out.contains("One Dark"));
    let v = zed::read_provider(&out, path, "offrig")
        .expect("read")
        .expect("present");
    assert_eq!(v["api_url"], "http://127.0.0.1:11435/v1");
    assert_eq!(v["available_models"][0]["name"], "pod-model");
    assert_eq!(v["available_models"][0]["max_tokens"], 65_536);
    assert_eq!(v["available_models"][0]["capabilities"]["tools"], true);
    assert_eq!(v["available_models"][0]["capabilities"]["images"], false);
    assert_eq!(
        zed::local_ollama_models(&out, path).expect("local"),
        vec!["local:8b".to_string()]
    );
    // Re-applying replaces the provider in place.
    let again = zed::apply_provider(&out, path, "offrig", "http://127.0.0.1:11436/v1", &[])
        .expect("apply again");
    let v = zed::read_provider(&again, path, "offrig")
        .expect("read")
        .expect("present");
    assert_eq!(v["api_url"], "http://127.0.0.1:11436/v1");
    let removed = zed::remove_provider(&again, path, "offrig").expect("remove");
    assert_eq!(
        zed::read_provider(&removed, path, "offrig").expect("read"),
        None
    );
    assert!(removed.contains("One Dark"));
}

#[test]
fn setting_the_default_model_keeps_the_other_keys() {
    let path = Path::new(P);
    let text =
        r#"{"agent":{"default_model":{"provider":"old","model":"o","enable_thinking":true}}}"#;
    let out = zed::set_default_model(
        text,
        path,
        &DefaultModel {
            provider: "offrig".into(),
            model: "pod".into(),
        },
    )
    .expect("set");
    assert!(out.contains("enable_thinking"));
    assert_eq!(
        zed::default_model(&out, path).expect("read"),
        Some(DefaultModel {
            provider: "offrig".into(),
            model: "pod".into()
        })
    );
}

#[test]
fn the_key_variable_is_named_from_the_provider_id() {
    assert_eq!(zed::api_key_env_name("offrig"), "OFFRIG_API_KEY");
    assert_eq!(zed::api_key_env_name("my-pod.1"), "MY_POD_1_API_KEY");
}

#[test]
fn a_key_variable_in_the_environment_counts_as_present() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        assert!(zed::api_key_env_present("offrig-test-zed"));
        return;
    }
    assert!(
        !zed::api_key_env_present("offrig-test-zed-absent"),
        "an unset variable is absent"
    );
    support::reexec(
        "a_key_variable_in_the_environment_counts_as_present",
        &[("OFFRIG_TEST_ZED_API_KEY", "x")],
        &[],
    );
}
