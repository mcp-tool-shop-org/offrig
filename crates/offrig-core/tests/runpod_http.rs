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

/// A v2 pod. Direct SSH is `ssh.direct`; the proxy is present and ignored.
const V2_POD: &str = r#"{"id":"p1","name":"offrig-frontier","status":"RUNNING","cost":8.36,
    "image":"ollama/ollama:0.35.0","cudaVersion":"12.8","dataCenterId":"EU-RO-1",
    "gpu":{"id":"NVIDIA H200","count":1},
    "ssh":{"proxy":{"host":"ssh.runpod.io","port":22,"username":"tok","command":"ssh tok@ssh.runpod.io"},
           "direct":{"host":"203.0.113.10","port":2222,"username":"root","command":"ssh root@203.0.113.10 -p 2222"}}}"#;

fn wrapped_pod(pod: &str) -> String {
    let pod: serde_json::Value = serde_json::from_str(pod).expect("pod json");
    serde_json::json!({
        "pods": [pod],
        "pagination": {"nextCursor": serde_json::Value::Null, "hasNextPage": false}
    })
    .to_string()
}

/// A small-profile create whose GPU list and price cap the test sets.
fn listed(ids: &[&str], cap: Option<f64>) -> offrig_core::runpod::PodCreate {
    let cfg = Config::default();
    let mut body = spec::pod_create(&cfg, cfg.profile("small").expect("small"));
    body.gpu_type_ids = ids.iter().map(|s| (*s).to_string()).collect();
    body.max_price_hr = cap;
    body
}

fn posts(m: &support::Mock) -> Vec<support::Req> {
    m.requests()
        .into_iter()
        .filter(|r| r.route() == "POST /pods")
        .collect()
}

fn gpu_id(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).expect("create body");
    v["gpu"]["id"].as_str().expect("gpu.id").to_string()
}

#[test]
fn pod_calls_use_the_right_routes_and_the_bearer_key() {
    let m = serve(|req, _| match req.route().as_str() {
        "GET /pods" => (200, wrapped_pod(V2_POD)),
        "GET /pods/p1" => (200, V2_POD.to_string()),
        "POST /pods" => (201, V2_POD.to_string()),
        "DELETE /pods/p1" | "POST /pods/p1/action" => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    });
    let rp = client(&m.url);

    let pods = rp.list_pods().expect("list");
    assert_eq!(pods.len(), 1);
    assert_eq!(m.last().target, "/pods");
    assert!(
        !m.last().target.contains("includeMachine"),
        "v2 has no includeMachine: {}",
        m.last().target
    );
    assert_eq!(m.last().header("authorization"), Some("Bearer sekret-key"));
    assert_eq!(pods[0].gpu_type(), Some("NVIDIA H200"));
    assert_eq!(pods[0].host_cuda(), Some("12.8"));

    let pod = rp.get_pod("p1").expect("get");
    assert_eq!(pod.ssh_endpoint(), Some(("203.0.113.10".to_string(), 2222)));
    assert_eq!(m.last().target, "/pods/p1");

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
    assert_eq!(sent["image"], body.image_name);
    assert_eq!(sent["gpu"]["id"], body.gpu_type_ids[0]);
    assert_eq!(sent["gpu"]["count"], body.gpu_count);
    assert_eq!(sent["disk"], body.container_disk_in_gb);
    assert_eq!(sent["startSsh"], true);
    assert!(sent.get("gpuTypeIds").is_none(), "{sent}");
    assert!(sent.get("allowedCudaVersions").is_none(), "{sent}");
    assert!(sent.get("interruptible").is_none(), "{sent}");
    match &body.min_cuda_version {
        Some(v) => assert_eq!(sent["gpu"]["minCudaVersion"], v.as_str()),
        None => assert!(sent["gpu"].get("minCudaVersion").is_none(), "{sent}"),
    }
    assert!(
        m.last()
            .header("content-type")
            .is_some_and(|t| t.contains("json"))
    );

    rp.stop_pod("p1").expect("stop");
    assert_eq!(m.last().route(), "POST /pods/p1/action");
    let stop: serde_json::Value = serde_json::from_str(&m.last().body).expect("action body");
    assert_eq!(stop["action"], "stop");
    rp.delete_pod("p1").expect("delete");
    assert_eq!(m.last().route(), "DELETE /pods/p1");
}

#[test]
fn volume_calls_use_the_right_routes() {
    let m = serve(|req, _| {
        match req.route().as_str() {
        "GET /network-volumes" => (
            200,
            r#"{"networkVolumes":[{"id":"v1","name":"weights","size":500,"dataCenter":"EU-RO-1"}]}"#
                .to_string(),
        ),
        "POST /network-volumes" => (
            201,
            r#"{"id":"v2","name":"new","size":100,"dataCenter":"US-KS-2"}"#.to_string(),
        ),
        "DELETE /network-volumes/v2" => (200, String::new()),
        _ => (404, "{}".to_string()),
    }
    });
    let rp = client(&m.url);
    let vols = rp.list_volumes().expect("list");
    assert_eq!((vols[0].id.as_str(), vols[0].size), ("v1", 500));
    assert_eq!(vols[0].data_center_id, "EU-RO-1");
    let made = rp.create_volume("new", 100, "US-KS-2").expect("create");
    assert_eq!(made.data_center_id, "US-KS-2");
    let sent: serde_json::Value = serde_json::from_str(&m.last().body).expect("json body");
    assert_eq!(sent["name"], "new");
    assert_eq!(sent["size"], 100);
    assert_eq!(sent["dataCenter"], "US-KS-2");
    assert!(sent.get("dataCenterId").is_none(), "{sent}");
    rp.delete_volume("v2").expect("delete");
    assert_eq!(m.last().route(), "DELETE /network-volumes/v2");
}

#[test]
fn a_400_tries_the_next_gpu_and_stops_at_the_list() {
    let m = serve(|req, nth| match req.route().as_str() {
        "POST /pods" if nth == 0 => (
            400,
            r#"{"title":"Bad Request","status":400,"detail":"could not place this GPU type"}"#
                .into(),
        ),
        "POST /pods" => (201, V2_POD.into()),
        _ => (404, "{}".into()),
    });
    let rp = client(&m.url);
    let pod = rp
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], None))
        .expect("second type is placed");
    assert_eq!(pod.id, "p1");
    let sent = posts(&m);
    assert_eq!(sent.len(), 2);
    assert_eq!(gpu_id(&sent[0].body), "NVIDIA H200");
    assert_eq!(gpu_id(&sent[1].body), "NVIDIA L4");

    // The list is the whole search. A 400 on the last id rents nothing.
    let m = serve(|_, _| {
        (
            400,
            r#"{"title":"Bad Request","status":400,"detail":"could not place this GPU type"}"#
                .into(),
        )
    });
    let err = client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], None))
        .expect_err("both refused");
    assert!(matches!(err, Error::NoCapacity(_)), "{err}");
    assert!(err.to_string().contains("NVIDIA H200"), "{err}");
    assert!(err.to_string().contains("NVIDIA L4"), "{err}");
    assert_eq!(posts(&m).len(), 2);
}

#[test]
fn a_422_stops_the_loop_and_keeps_the_problem_detail() {
    let m = serve(|_, _| {
        (
            422,
            r#"{"title":"Unprocessable Entity","status":422,"detail":"minCudaVersion must be major.minor","errors":["gpu.minCudaVersion"]}"#
                .into(),
        )
    });
    let err = client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], None))
        .expect_err("bad body");
    match err {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 422);
            assert!(
                body.contains("minCudaVersion must be major.minor"),
                "{body}"
            );
            assert!(body.contains("gpu.minCudaVersion"), "{body}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        posts(&m).len(),
        1,
        "a contract error is not a capacity miss"
    );
}

#[test]
fn a_402_stops_the_loop() {
    let m = serve(|_, _| {
        (
            402,
            r#"{"title":"Payment Required","status":402,"detail":"insufficient balance"}"#.into(),
        )
    });
    let err = client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], None))
        .expect_err("no balance");
    match err {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 402);
            assert!(body.contains("insufficient balance"), "{body}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(posts(&m).len(), 1);
}

#[test]
fn a_500_that_says_no_instances_does_not_try_the_next_gpu() {
    let m = serve(|_, _| {
        (
            500,
            r#"{"error":"create pod: There are no instances currently available"}"#.into(),
        )
    });
    let err = client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], None))
        .expect_err("not a placement 400");
    match err {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 500);
            assert!(body.contains("no instances currently available"), "{body}");
        }
        other => panic!("{other}"),
    }
    assert_eq!(
        posts(&m).len(),
        1,
        "the wait layer retries this same first GPU"
    );
}

const PRICED: &str = r#"{"data":{"gpuTypes":[
  {"id":"NVIDIA H200","displayName":"H200","memoryInGb":141,"secureCloud":true,
   "lowestPrice":{"uninterruptablePrice":1.0,"stockStatus":"High"}},
  {"id":"NVIDIA L4","displayName":"L4","memoryInGb":24,"secureCloud":true,
   "lowestPrice":{"uninterruptablePrice":0.4,"stockStatus":"Low"}}
]}}"#;

#[test]
fn the_plan_price_skips_a_dearer_gpu_and_posts_the_next() {
    let m = serve(|req, _| match req.route().as_str() {
        "POST /graphql" => (200, PRICED.to_string()),
        "POST /pods" => (201, V2_POD.to_string()),
        _ => (404, "{}".to_string()),
    });
    let rp = client(&m.url);
    // Equal to the cap is allowed.
    rp.create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], Some(1.0)))
        .expect("at the cap");
    assert_eq!(
        posts(&m)
            .iter()
            .map(|r| gpu_id(&r.body))
            .collect::<Vec<_>>(),
        ["NVIDIA H200"]
    );
    // Above the cap is skipped. The cheaper id on the list is posted.
    rp.create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], Some(0.5)))
        .expect("under the cap");
    let ids: Vec<_> = posts(&m).iter().map(|r| gpu_id(&r.body)).collect();
    assert_eq!(ids, ["NVIDIA H200", "NVIDIA L4"]);
    // Nothing on the list is under this cap, so nothing is posted.
    let before = posts(&m).len();
    let err = rp
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], Some(0.1)))
        .expect_err("all above the cap");
    assert!(matches!(err, Error::NoCapacity(_)), "{err}");
    assert_eq!(posts(&m).len(), before);
}

#[test]
fn a_gpu_with_no_listed_price_is_still_tried() {
    let offers = r#"{"data":{"gpuTypes":[
      {"id":"NVIDIA L4","displayName":"L4","memoryInGb":24,"secureCloud":true,
       "lowestPrice":{"uninterruptablePrice":0.4,"stockStatus":"Low"}}
    ]}}"#;
    let m = serve(move |req, nth| match req.route().as_str() {
        "POST /graphql" => (200, offers.to_string()),
        "POST /pods" if nth == 0 => (
            400,
            r#"{"title":"Bad Request","status":400,"detail":"could not place NVIDIA H200"}"#.into(),
        ),
        "POST /pods" => (201, V2_POD.into()),
        _ => (404, "{}".into()),
    });
    client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], Some(1.0)))
        .expect("unknown price is attempted");
    let ids: Vec<_> = posts(&m).iter().map(|r| gpu_id(&r.body)).collect();
    assert_eq!(ids, ["NVIDIA H200", "NVIDIA L4"]);
}

#[test]
fn a_failed_price_read_still_tries_every_listed_gpu() {
    let m = serve(|req, nth| match req.route().as_str() {
        "POST /graphql" => (500, "down".into()),
        "POST /pods" if nth == 0 => (
            400,
            r#"{"title":"Bad Request","status":400,"detail":"could not place this GPU type"}"#
                .into(),
        ),
        "POST /pods" => (201, V2_POD.into()),
        _ => (404, "{}".into()),
    });
    client(&m.url)
        .create_pod(&listed(&["NVIDIA H200", "NVIDIA L4"], Some(1.0)))
        .expect("create anyway");
    let ids: Vec<_> = posts(&m).iter().map(|r| gpu_id(&r.body)).collect();
    assert_eq!(ids, ["NVIDIA H200", "NVIDIA L4"]);
}

#[test]
fn list_pods_reads_the_wrapper_and_the_next_page() {
    let m = serve(|req, nth| {
        match (req.route().as_str(), nth) {
        ("GET /pods", 0) => (
            200,
            r#"{"pods":[{"id":"p1","status":"RUNNING","cost":1.0,"gpu":{"id":"NVIDIA L4","count":1}}],"pagination":{"nextCursor":"a/b c","hasNextPage":true}}"#
                .into(),
        ),
        ("GET /pods", _) => (
            200,
            r#"{"pods":[{"id":"p2","status":"RUNNING","cost":2.0,"gpu":{"id":"NVIDIA H200","count":1}}],"pagination":{"nextCursor":null,"hasNextPage":false}}"#
                .into(),
        ),
        _ => (404, "{}".into()),
    }
    });
    let pods = client(&m.url).list_pods().expect("list");
    assert_eq!(pods.len(), 2);
    assert_eq!(pods[0].gpu_type(), Some("NVIDIA L4"));
    assert_eq!(pods[1].id, "p2");
    assert_eq!(m.requests()[1].target, "/pods?cursor=a%2Fb%20c");
    assert_eq!(m.count("GET /pods"), 2);
}

#[test]
fn list_pods_stops_after_fifty_pages() {
    let m = serve(|req, nth| {
        assert_eq!(req.route(), "GET /pods");
        let body = serde_json::json!({
            "pods": [{"id": format!("p{nth}"), "desiredStatus": "RUNNING"}],
            "pagination": {"nextCursor": format!("c{nth}"), "hasNextPage": true}
        });
        (200, body.to_string())
    });
    let pods = client(&m.url).list_pods().expect("capped");
    assert_eq!(pods.len(), 50);
    assert_eq!(pods[0].id, "p0");
    assert_eq!(pods[49].id, "p49");
    assert_eq!(m.count("GET /pods"), 50);
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
