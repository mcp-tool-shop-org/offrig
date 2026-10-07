//! Finding or creating a profile's network volume, and the staging pod's secrets,
//! against a mock RunPod.

mod support;

use offrig_core::config::Config;
use offrig_core::runpod::{NetworkVolume, RunPod};
use offrig_core::stage;
use support::serve;

fn frontier() -> offrig_core::config::Profile {
    Config::default()
        .profile("frontier")
        .expect("frontier")
        .clone()
}

fn rp(url: &str) -> RunPod {
    RunPod::new("k", url, &format!("{url}/graphql"))
}

#[test]
fn an_existing_volume_in_the_data_center_is_reused_not_recreated() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /networkvolumes" => (
            200,
            r#"[{"id":"other-dc","name":"offrig-frontier","size":300,"dataCenterId":"US-KS-2"},
                {"id":"other-name","name":"something","size":300,"dataCenterId":"EUR-IS-1"},
                {"id":"mine","name":"offrig-frontier","size":300,"dataCenterId":"EUR-IS-1"}]"#
                .to_string(),
        ),
        _ => (404, "{}".to_string()),
    });
    let (vol, created) = stage::ensure_volume(&rp(&m.url), &frontier(), "EUR-IS-1").expect("found");
    assert_eq!(vol.id, "mine");
    assert!(!created);
    assert_eq!(m.count("POST /networkvolumes"), 0, "nothing was created");
}

#[test]
fn a_missing_volume_is_created_sized_for_the_weights() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /networkvolumes" => (
            200,
            r#"[{"id":"x","name":"offrig-frontier","size":300,"dataCenterId":"US-KS-2"}]"#
                .to_string(),
        ),
        "POST /networkvolumes" => (
            200,
            r#"{"id":"new","name":"offrig-frontier","size":300,"dataCenterId":"EUR-IS-1"}"#
                .to_string(),
        ),
        _ => (404, "{}".to_string()),
    });
    let (vol, created) = stage::ensure_volume(&rp(&m.url), &frontier(), "EUR-IS-1").expect("made");
    assert_eq!(vol.id, "new");
    assert!(created);
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json");
    assert_eq!(sent["name"], "offrig-frontier");
    assert_eq!(sent["size"], 300, "252 GB x 1.15 + 10");
    assert_eq!(sent["dataCenterId"], "EUR-IS-1");
}

#[test]
fn a_failing_volume_api_stops_the_staging() {
    let m = serve(|_, _| (500, "down".to_string()));
    let err = stage::ensure_volume(&rp(&m.url), &frontier(), "EUR-IS-1").expect_err("api down");
    assert!(err.retryable(), "{err}");
}

#[test]
fn a_hugging_face_secret_reaches_the_staging_pod_as_a_runpod_secret_reference() {
    let mut p = frontier();
    if let Some(r) = p.recipe.as_mut() {
        r.hf_token_secret = Some("hf_main".into());
    }
    let vol = NetworkVolume {
        id: "vol1".into(),
        name: stage::volume_name(&p),
        size: 300,
        data_center_id: "EUR-IS-1".into(),
    };
    let body = stage::stage_pod(&p, &vol).expect("pod");
    assert_eq!(body.env["HF_TOKEN"], "{{ RUNPOD_SECRET_hf_main }}");
    assert_eq!(body.name, "offrig-stage-frontier");
    assert_eq!(body.gpu_count, 1);
    assert!(body.docker_start_cmd[0].contains("STAGED_OK"));
}
