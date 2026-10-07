//! The RunPod client against a mock server on 127.0.0.1: every call's method, path,
//! auth header and body, and how failures surface.

mod support;

use offrig_core::config::Config;
use offrig_core::error::Error;
use offrig_core::runpod::RunPod;
use offrig_core::spec;
use support::{dead_url, serve};

fn client(base: &str) -> RunPod {
    RunPod::new(
        "sekret-key",
        &format!("{base}/"),
        &format!("{base}/graphql"),
    )
}

const POD: &str = r#"{"id":"p1","name":"offrig-frontier","desiredStatus":"RUNNING","costPerHr":8.36,
    "publicIp":"1.2.3.4","portMappings":{"22":2222},
    "machine":{"gpuTypeId":"NVIDIA H200","dataCenterId":"EU-RO-1","cudaVersion":12.8}}"#;

#[test]
fn pod_calls_use_the_right_routes_and_the_bearer_key() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, format!("[{POD}]")),
        "GET /pods/p1" => (200, POD.to_string()),
        "POST /pods" => (200, POD.to_string()),
        "DELETE /pods/p1" | "POST /pods/p1/stop" => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    });
    let rp = client(&m.url);

    let pods = rp.list_pods().expect("list");
    assert_eq!(pods.len(), 1);
    assert_eq!(m.last().target, "/pods?includeMachine=true");
    assert_eq!(m.last().header("authorization"), Some("Bearer sekret-key"));
    assert_eq!(pods[0].gpu_type(), Some("NVIDIA H200"));
    assert_eq!(pods[0].host_cuda(), Some("12.8"));

    let pod = rp.get_pod("p1").expect("get");
    assert_eq!(pod.ssh_endpoint(), Some(("1.2.3.4".to_string(), 2222)));
    assert_eq!(m.last().target, "/pods/p1?includeMachine=true");

    assert_eq!(rp.find_pod("p1").expect("by id").id, "p1");
    assert_eq!(rp.find_pod("offrig-frontier").expect("by name").id, "p1");
    let missing = rp.find_pod("nope").expect_err("unknown pod");
    assert!(
        matches!(missing, Error::PodNotFound(ref n) if n == "nope"),
        "{missing}"
    );

    let cfg = Config::default();
    let body = spec::pod_create(&cfg, cfg.profile("frontier").expect("frontier"));
    let created = rp.create_pod(&body).expect("create");
    assert_eq!(created.id, "p1");
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json body");
    assert_eq!(sent["name"], body.name);
    assert_eq!(sent["gpuCount"], body.gpu_count);
    assert!(
        m.last()
            .header("content-type")
            .is_some_and(|t| t.contains("json"))
    );

    rp.stop_pod("p1").expect("stop");
    assert_eq!(m.last().route(), "POST /pods/p1/stop");
    rp.delete_pod("p1").expect("delete");
    assert_eq!(m.last().route(), "DELETE /pods/p1");
}

#[test]
fn volume_calls_use_the_right_routes() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /networkvolumes" => (
            200,
            r#"[{"id":"v1","name":"weights","size":500,"dataCenterId":"EU-RO-1"}]"#.to_string(),
        ),
        "POST /networkvolumes" => (
            200,
            r#"{"id":"v2","name":"new","size":100,"dataCenterId":"US-KS-2"}"#.to_string(),
        ),
        "DELETE /networkvolumes/v2" => (200, String::new()),
        _ => (404, "{}".to_string()),
    });
    let rp = client(&m.url);
    let vols = rp.list_volumes().expect("list");
    assert_eq!((vols[0].id.as_str(), vols[0].size), ("v1", 500));
    let made = rp.create_volume("new", 100, "US-KS-2").expect("create");
    assert_eq!(made.data_center_id, "US-KS-2");
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json body");
    assert_eq!(sent["name"], "new");
    assert_eq!(sent["size"], 100);
    assert_eq!(sent["dataCenterId"], "US-KS-2");
    rp.delete_volume("v2").expect("delete");
    assert_eq!(m.last().route(), "DELETE /networkvolumes/v2");
}

#[test]
fn a_failing_status_becomes_an_api_error_with_a_clipped_body() {
    let long = "x".repeat(2000);
    let m = serve(move |req, _| match req.path() {
        "/pods" => (500, long.clone()),
        _ => (429, "slow down".to_string()),
    });
    let rp = client(&m.url);
    match rp.list_pods().expect_err("500") {
        Error::Api { status, body, what } => {
            assert_eq!((status, what.as_str()), (500, "list pods"));
            assert_eq!(body.len(), 600, "body is cut to 600 bytes");
        }
        other => panic!("{other}"),
    }
    let e = rp.stop_pod("p1").expect_err("429");
    assert!(matches!(e, Error::Api { status: 429, .. }), "{e}");
    assert!(e.retryable());
}

#[test]
fn an_unreadable_body_is_a_decode_error_and_a_dead_server_a_network_error() {
    let m = serve(|_, _| (200, "not json".to_string()));
    let rp = client(&m.url);
    let e = rp.get_pod("p1").expect_err("bad json");
    assert!(matches!(e, Error::Decode { .. }), "{e}");
    assert_eq!(e.code(), "internal");

    let gone = client(&dead_url());
    let e = gone.list_pods().expect_err("connection refused");
    assert!(matches!(e, Error::Http { .. }), "{e}");
    assert_eq!(e.code(), "network");
}

#[test]
fn account_reads_the_graphql_balance() {
    let m = serve(|req, _| {
        assert_eq!(req.route(), "POST /graphql");
        (
            200,
            r#"{"data":{"myself":{"clientBalance":42.5,"currentSpendPerHr":1.25,"spendLimit":80}}}"#
                .to_string(),
        )
    });
    let a = client(&m.url).account().expect("account");
    assert!((a.client_balance - 42.5).abs() < 1e-9);
    assert_eq!(a.spend_limit, Some(80.0));
    assert!(m.last().body.contains("clientBalance"));
}

#[test]
fn graphql_errors_and_empty_answers_are_reported() {
    let m = serve(|_, nth| match nth {
        0 => (
            200,
            r#"{"errors":[{"message":"bad key"},{"message":"also bad"}]}"#.to_string(),
        ),
        _ => (200, r#"{"data":null}"#.to_string()),
    });
    let rp = client(&m.url);
    match rp.account().expect_err("graphql errors") {
        Error::GraphQl { what, message } => {
            assert_eq!(what, "account balance");
            assert_eq!(message, "bad key; also bad");
        }
        other => panic!("{other}"),
    }
    match rp.data_centers().expect_err("no data") {
        Error::GraphQl { message, .. } => assert_eq!(message, "response carried no data"),
        other => panic!("{other}"),
    }
}

const GPU_TYPES: &str = r#"{"data":{"gpuTypes":[
  {"id":"NVIDIA H200","displayName":"H200","memoryInGb":141,"secureCloud":true,
   "lowestPrice":{"uninterruptablePrice":3.5,"stockStatus":"High"}},
  {"id":"NVIDIA L4","displayName":"L4","memoryInGb":24,"secureCloud":true,
   "lowestPrice":{"uninterruptablePrice":0.4,"stockStatus":"Low"}},
  {"id":"NVIDIA A100","displayName":"A100","memoryInGb":80,"secureCloud":true,"lowestPrice":null},
  {"id":"NVIDIA B200 MIG 1g","displayName":"MIG","memoryInGb":10,"secureCloud":true,"lowestPrice":null},
  {"id":"AMD MI300X","displayName":"AMD","memoryInGb":192,"secureCloud":true,"lowestPrice":null},
  {"id":"NVIDIA T4","displayName":"T4","memoryInGb":16,"secureCloud":false,"lowestPrice":null},
  {"id":"NVIDIA Ghost","displayName":"Ghost","memoryInGb":0,"secureCloud":true,"lowestPrice":null}
]}}"#;

#[test]
fn gpu_offers_are_filtered_and_sorted_cheapest_first() {
    let m = serve(|_, _| (200, GPU_TYPES.to_string()));
    let rp = client(&m.url);
    let offers = rp.gpu_offers(2).expect("offers");
    let ids: Vec<&str> = offers.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["NVIDIA L4", "NVIDIA H200", "NVIDIA A100"]);
    assert_eq!(offers[0].price_per_hr, Some(0.4));
    assert_eq!(offers[0].stock.as_deref(), Some("Low"));
    assert_eq!(offers[0].gpu_count, 2);
    assert_eq!(offers[1].total_vram_gb(), 282);
    assert_eq!(offers[2].price_per_hr, None);
    assert!(m.last().body.contains("gpuCount: 2"));
    assert!(!m.last().body.contains("dataCenterId"));
}

#[test]
fn a_data_center_filter_is_sent_only_when_it_is_a_plain_id() {
    let m = serve(|_, _| (200, GPU_TYPES.to_string()));
    let rp = client(&m.url);
    rp.gpu_offers_in(1, Some("EU-RO-1")).expect("offers");
    assert!(
        m.last().body.contains(r#"dataCenterId: \"EU-RO-1\""#),
        "{}",
        m.last().body
    );
    // Anything that could break out of the query is dropped, not sent.
    rp.gpu_offers_in(1, Some("EU\" } evil")).expect("offers");
    assert!(!m.last().body.contains("evil"));
    assert!(!m.last().body.contains("dataCenterId"));
}

#[test]
fn data_centers_are_listed() {
    let m = serve(|_, _| {
        (
            200,
            r#"{"data":{"dataCenters":[{"id":"EU-RO-1","name":"Romania","storageSupport":true},{"id":"X"}]}}"#
                .to_string(),
        )
    });
    let dcs = client(&m.url).data_centers().expect("data centers");
    assert_eq!(dcs.len(), 2);
    assert!(dcs[0].storage_support);
    assert!(!dcs[1].storage_support);
}

/// `from_env` reads process variables, which a test cannot change in-process, so each
/// case re-runs this test as a child with its own environment.
fn child(name: &str, vars: &[(&str, &str)], remove: &[&str]) {
    support::reexec(name, vars, remove);
}

#[test]
fn from_env_without_a_key_is_missing_api_key() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        assert!(matches!(RunPod::from_env(), Err(Error::MissingApiKey)));
        return;
    }
    child(
        "from_env_without_a_key_is_missing_api_key",
        &[],
        &["RUNPOD_API_KEY"],
    );
    child(
        "from_env_without_a_key_is_missing_api_key",
        &[("RUNPOD_API_KEY", "   ")],
        &[],
    );
}

#[test]
fn from_env_honours_the_test_base_in_debug_builds() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        let rp = RunPod::from_env().expect("client from env");
        assert!(rp.list_pods().expect("list").is_empty());
        assert!(rp.account().is_ok(), "graphql goes to <base>/graphql");
        return;
    }
    let m = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, "[]".to_string()),
        "POST /graphql" => (
            200,
            r#"{"data":{"myself":{"clientBalance":1.0,"currentSpendPerHr":0.0}}}"#.to_string(),
        ),
        _ => (404, "{}".to_string()),
    });
    child(
        "from_env_honours_the_test_base_in_debug_builds",
        &[
            ("RUNPOD_API_KEY", "env-key"),
            ("OFFRIG_TEST_RUNPOD_BASE", &m.url),
        ],
        &[],
    );
    // The key went to the mock, not to the real endpoint.
    assert_eq!(m.last().header("authorization"), Some("Bearer env-key"));
    assert_eq!(m.count("GET /pods"), 1);
}

#[test]
fn an_empty_test_base_is_ignored() {
    if std::env::var_os("OFFRIG_CHILD").is_some() {
        assert!(RunPod::from_env().is_ok());
        return;
    }
    child(
        "an_empty_test_base_is_ignored",
        &[
            ("RUNPOD_API_KEY", "env-key"),
            ("OFFRIG_TEST_RUNPOD_BASE", ""),
        ],
        &[],
    );
}
