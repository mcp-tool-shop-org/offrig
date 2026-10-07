//! More window tests: every panel and dialog, rendered by egui's test harness, with
//! the worker replaced by a channel the test reads.

use super::ui_tests::{harness, sent};
use super::*;
use crate::worker::tests::Rig;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use offrig_core::ollama::TagDetails;
use offrig_core::zed::DefaultModel;
use std::sync::mpsc::Receiver;

fn show(app: App) -> Harness<'static, App> {
    Harness::builder()
        .with_size(egui::vec2(1180.0, 900.0))
        .build_ui_state(|ui, app: &mut App| app.draw(ui), app)
}

/// An app with no updates applied at all.
fn bare() -> (Harness<'static, App>, Receiver<Cmd>) {
    let (app, cmds, _tx) = test_app();
    (show(app), cmds)
}

fn pod_with(extra: serde_json::Value) -> Pod {
    let mut v = serde_json::json!({
        "id": "abc", "name": "offrig-medium", "desiredStatus": "RUNNING", "costPerHr": 1.59,
        "publicIp": "1.2.3.4", "portMappings": {"22": 2222}, "ports": ["22/tcp"], "gpuCount": 1,
        "machine": {"gpuTypeId": "NVIDIA A100-SXM4-80GB"}
    });
    v.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    serde_json::from_value(v).expect("pod json")
}

fn offer(id: &str, mem: u32, price: Option<f64>) -> GpuOffer {
    GpuOffer {
        id: id.into(),
        display_name: id.into(),
        memory_gb: mem,
        gpu_count: 1,
        price_per_hr: price,
        stock: None,
    }
}

fn with_profile(edit: impl FnOnce(&mut Config)) -> Update {
    let mut cfg = Config::default();
    edit(&mut cfg);
    Update::Config(Box::new(cfg))
}

// --- State ----------------------------------------------------------------------------

#[test]
fn every_update_kind_lands_in_its_field() {
    let mut s = State::default();
    s.apply(Update::PodModels(vec![Tag {
        name: "m:1b".into(),
        size: 1_000_000_000,
        details: TagDetails::default(),
    }]));
    s.apply(Update::Check(ChatCheck {
        model: "m:1b".into(),
        reply: "hi".into(),
        streamed_chunks: 3,
        tool_call: None,
        seconds: 1.5,
    }));
    s.apply(Update::Gpu(vec![GpuStat {
        index: 0,
        name: "A".into(),
        util_pct: 7,
        mem_used_mb: 1,
        mem_total_mb: 2,
    }]));
    s.apply(Update::Idle(Idle::Unknown));
    s.apply(Update::Zed(ZedStatus {
        provider_models: vec!["m:1b".into()],
        default: None,
        key_env: true,
    }));
    s.apply(Update::Busy(Some("working".into())));
    s.apply(Update::Tunnel(true));
    assert_eq!(s.pod_models[0].name, "m:1b");
    assert_eq!(s.check.as_ref().map(|c| c.streamed_chunks), Some(3));
    assert_eq!(s.gpus[0].util_pct, 7);
    assert_eq!(s.idle, Some(Idle::Unknown));
    assert_eq!(s.zed.provider_models, ["m:1b"]);
    assert_eq!(s.busy.as_deref(), Some("working"));
    assert!(s.tunnel);
}

#[test]
fn money_questions_have_no_answer_until_the_data_arrives() {
    let mut s = State::default();
    assert!(s.active_profile().is_none() && s.current_pod().is_none());
    assert!(s.runway_with(1.0).is_none(), "no account yet");
    assert!(s.pod_session_cost(0).is_none(), "no pod yet");

    s.apply(Update::Config(Box::default()));
    s.apply(Update::Account(Account {
        client_balance: 15.9,
        current_spend_per_hr: 0.0,
        spend_limit: None,
    }));
    let runway = s.runway_with(1.59).expect("a runway");
    assert!((runway - 10.0).abs() < 1e-9, "{runway}");

    let p = s.active_profile().expect("medium").clone();
    assert!(s.best_offer(&p).is_none(), "no market read yet");
    s.apply(Update::Offers(
        1,
        vec![offer("NVIDIA A100-SXM4-80GB", 80, None)],
    ));
    assert!(
        s.best_offer(&p).is_none(),
        "the only listed GPU has none free"
    );

    s.apply(Update::Pods(vec![pod_with(serde_json::json!({}))]));
    assert!(
        s.pod_session_cost(0).is_none(),
        "the pod has not reported a start time"
    );
    s.apply(Update::Pods(vec![pod_with(serde_json::json!({
        "lastStartedAt": "2026-10-02 16:00:00.000 +0000 UTC"
    }))]));
    let started = cost::parse_timestamp("2026-10-02 16:00:00.000 +0000 UTC").expect("timestamp");
    let two_hours_in = s
        .pod_session_cost(started + 7200)
        .expect("a cost once started");
    assert!((two_hours_in - 3.18).abs() < 1e-9, "{two_hours_in}");
}

#[test]
fn queued_updates_are_applied_when_the_window_draws() {
    let (mut app, _cmds, tx) = test_app();
    tx.send(Update::Log("from the worker".into()))
        .expect("send");
    tx.send(Update::Tunnel(true)).expect("send");
    app.drain();
    assert_eq!(
        app.st.log.back().map(String::as_str),
        Some("from the worker")
    );
    assert!(app.st.tunnel);
}

// --- top bar, activity log, profiles ----------------------------------------------------------

#[test]
fn before_anything_arrives_the_window_says_it_is_loading() {
    let (mut h, cmds) = bare();
    h.run();
    h.get_by_label("Reading the account…");
    h.get_by_label("Loading…");
    h.get_by_label("No pod running");
    h.get_by_label("Refresh").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::Refresh]);
}

#[test]
fn errors_show_in_a_banner_and_in_the_log_until_dismissed() {
    let (mut h, _cmds) = harness(vec![
        Update::Log("a plain line".into()),
        Update::Error("the pod fell over".into()),
    ]);
    h.run();
    h.get_by_label("Error: the pod fell over");
    h.get_by_label("a plain line");
    h.get_by_label("error: the pod fell over");
    h.get_by_label("Dismiss").click();
    h.run();
    assert!(h.query_by_label("Error: the pod fell over").is_none());
    assert!(h.state().st.error.is_none());
    h.get_by_label("error: the pod fell over");
}

#[test]
fn runway_is_shown_for_every_balance() {
    for (balance, shown) in [
        (1.2, "Runway 1.5 h"),
        (3.2, "Runway 4.0 h"),
        (80.0, "Runway 4.2 days"),
    ] {
        let (mut h, _cmds) = harness(vec![Update::Account(Account {
            client_balance: balance,
            current_spend_per_hr: 0.8,
            spend_limit: None,
        })]);
        h.run();
        h.get_by_label(shown);
    }
    let (mut h, _cmds) = harness(vec![Update::Account(Account {
        client_balance: 5.0,
        current_spend_per_hr: 0.0,
        spend_limit: None,
    })]);
    h.run();
    h.get_by_label("Runway unlimited");
}

#[test]
fn picking_another_profile_asks_the_worker_to_switch() {
    let (mut h, cmds) = harness(vec![]);
    h.run();
    h.get_by_label("small (Small)").click();
    h.run();
    assert!(sent(&cmds).contains(&Cmd::SetProfile("small".into())));
}

#[test]
fn the_profile_card_describes_waiting_and_storage() {
    let (mut h, _cmds) = harness(vec![with_profile(|c| {
        c.active_profile = "frontier".into();
    })]);
    h.run();
    h.get_by_label("wait up to 120 min (nothing rented while waiting)");
    h.get_by_label("400 GB pod disk (deleted with the pod)");
    h.get_by_label_contains("No listed GPU has 4 free right now");

    let (mut h, _cmds) = harness(vec![with_profile(|c| {
        for p in &mut c.profiles {
            if p.name == "medium" {
                p.network_volume_id = Some("vol123".into());
                p.data_center_id = Some("EU-RO-1".into());
            }
        }
    })]);
    h.run();
    h.get_by_label("network volume vol123 (kept between pods)");
}

#[test]
fn a_gpu_too_small_for_the_largest_model_is_flagged() {
    let (mut h, _cmds) = harness(vec![Update::Offers(
        1,
        vec![offer("NVIDIA A100-SXM4-80GB", 40, Some(1.0))],
    )]);
    h.run();
    h.get_by_label_contains("40 GB VRAM: the largest model may not fit");
    h.get_by_label_contains("Runway with this pod:");
}

#[test]
fn the_gpu_market_lists_priced_offers_and_marks_the_profiles_cards() {
    let (mut h, _cmds) = harness(vec![Update::Offers(
        1,
        vec![
            offer("NVIDIA A100-SXM4-80GB", 80, Some(1.59)),
            offer("NVIDIA A40", 48, Some(0.49)),
            offer("NVIDIA RTX A2000", 6, None),
        ],
    )]);
    h.run();
    assert!(h.query_by_label("A40").is_none(), "collapsed until opened");
    h.get_by_label("GPU market").click();
    h.run();
    h.get_by_label("A40");
    h.get_by_label("48 GB");
    h.get_by_label("$0.49");
    assert!(
        h.query_by_label("RTX A2000").is_none(),
        "an offer with no free GPU is not listed"
    );
}

#[test]
fn auto_stop_accepts_five_minutes_or_more_and_can_be_switched_off() {
    let (mut h, cmds) = harness(vec![]);
    h.run();
    h.get_by_label("Auto-stop").click();
    h.run();
    h.get_by_label("Terminate the pod after 30 minutes with every GPU idle.");

    h.state_mut().ui.auto_stop_input = "3".into();
    h.run();
    h.get_by_label("Set").click();
    h.run();
    assert!(sent(&cmds).is_empty(), "under five minutes is refused");

    h.state_mut().ui.auto_stop_input = "45".into();
    h.run();
    h.get_by_label("Set").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::SetAutoStop(Some(45))]);

    h.get_by_label("Off").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::SetAutoStop(None)]);

    h.state_mut().st.apply(with_profile(|c| {
        c.auto_stop_idle_minutes = None;
    }));
    h.run();
    h.get_by_label("Off: the pod runs until you shut it down.");
}

#[test]
fn a_config_naming_no_known_profile_shows_the_radio_list_only() {
    let (mut h, _cmds) = harness(vec![with_profile(|c| {
        c.active_profile = "gone".into();
    })]);
    h.run();
    h.get_by_label("small (Small)");
    assert!(h.query_by_label("Context").is_none(), "no profile card");
    assert!(h.query_by_label("Launch pod").is_none());
}

#[test]
fn the_market_section_is_empty_until_offers_for_this_gpu_count_arrive() {
    let (mut h, _cmds) = harness(vec![with_profile(|c| {
        c.active_profile = "frontier".into();
    })]);
    h.run();
    h.get_by_label("GPU market").click();
    h.run();
    assert!(
        h.query_by_label("VRAM").is_none(),
        "no table without offers"
    );
    h.state_mut().st.apply(Update::Offers(
        4,
        vec![GpuOffer {
            gpu_count: 4,
            ..offer(
                "NVIDIA RTX PRO 6000 Blackwell Server Edition",
                96,
                Some(8.36),
            )
        }],
    ));
    h.run();
    h.get_by_label("GPU ×4");
    h.get_by_label("384 GB");
}

// --- the pod panel ------------------------------------------------------------------------------------

#[test]
fn gpu_load_and_idle_state_are_shown_for_a_running_pod() {
    let (mut h, _cmds) = harness(vec![
        Update::Pods(vec![pod_with(serde_json::json!({}))]),
        Update::Gpu(vec![GpuStat {
            index: 0,
            name: "A100".into(),
            util_pct: 90,
            mem_used_mb: 2048,
            mem_total_mb: 81920,
        }]),
    ]);
    h.run();
    h.get_by_label("#0 90% · 2/80 GB");
    h.get_by_label("1 × NVIDIA A100-SXM4-80GB");
    for (idle, shown) in [
        (Idle::Busy, "busy"),
        (Idle::Idle(Duration::from_secs(300)), "idle 5 min"),
        (Idle::Stop, "idle limit reached"),
        (Idle::Unknown, "no GPU reading"),
    ] {
        h.state_mut().st.apply(Update::Idle(idle));
        h.run();
        h.get_by_label(shown);
    }
}

#[test]
fn the_pod_panel_sends_its_buttons_as_commands() {
    let (mut h, cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("Open /workspace in Zed").click();
    h.run();
    h.get_by_label("Write Zed provider").click();
    h.run();
    h.get_by_label("Remove from Zed").click();
    h.run();
    h.get_by_label("Run checks").click();
    h.run();
    assert_eq!(
        sent(&cmds),
        [
            Cmd::OpenZedRemote,
            Cmd::ConfigureZed {
                default_model: None
            },
            Cmd::RemoveZed,
            Cmd::Guard
        ]
    );
}

#[test]
fn a_busy_worker_disables_the_buttons_that_would_queue_behind_it() {
    let (mut h, cmds) = harness(vec![
        Update::Pods(vec![pod_with(serde_json::json!({}))]),
        Update::Busy(Some("Pulling x".into())),
    ]);
    h.run_steps(3);
    h.get_by_label("Open tunnel").click();
    h.get_by_label("Write Zed provider").click();
    h.get_by_label("Run checks").click();
    h.run_steps(3);
    assert!(sent(&cmds).is_empty());
}

#[test]
fn the_pods_models_can_be_tested_once_the_tunnel_is_open() {
    let tag = Tag {
        name: "podonly:1b".into(),
        size: 1_500_000_000,
        details: TagDetails {
            parameter_size: "1B".into(),
            quantization_level: "Q4_0".into(),
        },
    };
    let (mut h, cmds) = harness(vec![
        Update::Pods(vec![pod_with(serde_json::json!({}))]),
        Update::PodModels(vec![tag]),
    ]);
    h.run();
    h.get_by_label("podonly:1b  (1.5 GB, 1B Q4_0)");
    h.get_by_label("Test").click();
    h.run();
    assert!(sent(&cmds).is_empty(), "the tunnel is closed");

    h.state_mut().st.apply(Update::Tunnel(true));
    h.run();
    h.get_by_label("Test").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::Check("podonly:1b".into())]);
}

#[test]
fn without_models_the_panel_says_to_open_the_tunnel() {
    let (mut h, _cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("Open the tunnel to list them.");
}

#[test]
fn pulls_show_progress_or_their_failure() {
    let (mut h, _cmds) = harness(vec![
        Update::Pods(vec![pod_with(serde_json::json!({}))]),
        Update::Pull {
            model: "big:70b".into(),
            state: PullState::Running {
                status: "pulling".into(),
                completed: 500_000_000,
                total: 1_000_000_000,
            },
        },
        Update::Pull {
            model: "new:1b".into(),
            state: PullState::Running {
                status: "starting".into(),
                completed: 0,
                total: 0,
            },
        },
        Update::Pull {
            model: "bad:1b".into(),
            state: PullState::Failed("disk full".into()),
        },
        Update::Pull {
            model: "gone:1b".into(),
            state: PullState::NotStarted,
        },
    ]);
    h.run();
    h.get_by_label_contains("big:70b: 0.5 / 1.0 GB (pulling)");
    h.get_by_label("new:1b: starting");
    h.get_by_label("bad:1b: disk full");
    assert!(h.query_by_label_contains("gone:1b").is_none());
}

#[test]
fn a_chat_check_result_is_summarised_with_its_tool_call() {
    let check = |tool: Option<&str>| {
        Update::Check(ChatCheck {
            model: "podonly:1b".into(),
            reply: "hi".into(),
            streamed_chunks: 3,
            tool_call: tool.map(str::to_string),
            seconds: 1.234,
        })
    };
    let (mut h, _cmds) = harness(vec![
        Update::Pods(vec![pod_with(serde_json::json!({}))]),
        check(Some("read_file")),
    ]);
    h.run();
    h.get_by_label("podonly:1b: answered in 1.2s over 3 streamed chunks; tool call: read_file");
    h.state_mut().st.apply(check(None));
    h.run();
    h.get_by_label("podonly:1b: answered in 1.2s over 3 streamed chunks; tool call: none");
}

#[test]
fn the_zed_section_reports_what_zed_has_and_picks_a_default() {
    let (mut h, cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("offrig's provider is not in Zed's settings yet.");
    h.get_by_label("Zed has no default agent model set.");
    h.get_by_label_contains("OFFRIG_API_KEY is not set yet");

    h.state_mut().st.apply(Update::Zed(ZedStatus {
        provider_models: vec!["a:1b".into(), "b:2b".into()],
        default: Some(DefaultModel {
            provider: "ollama".into(),
            model: "qwen3:14b".into(),
        }),
        key_env: true,
    }));
    h.run();
    h.get_by_label("Provider \"offrig\" offers: a:1b, b:2b");
    h.get_by_label("Zed's default agent model: ollama / qwen3:14b");
    assert!(h.query_by_label_contains("OFFRIG_API_KEY").is_none());

    h.get_by_value("keep Zed's default").click();
    h.run();
    h.get_by_label("gpt-oss:120b").click();
    h.run();
    h.get_by_label("Write Zed provider").click();
    h.run();
    assert_eq!(
        sent(&cmds),
        [Cmd::ConfigureZed {
            default_model: Some("gpt-oss:120b".into())
        }]
    );
}

#[test]
fn the_guard_section_starts_empty() {
    let (mut h, _cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("Not run yet.");
}

// --- dialogs --------------------------------------------------------------------------------------------

#[test]
fn cancelling_the_launch_dialog_sends_nothing() {
    let (mut h, cmds) = harness(vec![]);
    h.run();
    h.get_by_label("Launch pod").click();
    h.run();
    h.get_by_label("Launch pod?");
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label("Launch pod?").is_none());
    assert!(sent(&cmds).is_empty());
}

#[test]
fn escape_closes_the_dialogs() {
    let (mut h, cmds) = harness(vec![]);
    h.run();
    h.get_by_label("Launch pod").click();
    h.run();
    h.get_by_label("Launch pod?");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label("Launch pod?").is_none());

    let (mut h, _cmds2) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("Shut down").click();
    h.run();
    h.get_by_label("Shut down the pod?");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label("Shut down the pod?").is_none());
    assert!(!h.state().ui.confirm_shutdown);
    assert!(sent(&cmds).is_empty());
}

#[test]
fn the_launch_dialog_without_a_config_has_only_a_heading() {
    let (mut app, cmds, _tx) = test_app();
    app.ui.confirm_launch = true;
    let mut h = show(app);
    h.run();
    h.get_by_label("Launch pod?");
    assert!(h.query_by_label("Launch").is_none());
    assert!(sent(&cmds).is_empty());
}

#[test]
fn the_shutdown_dialog_for_a_pod_on_a_network_volume_says_the_models_stay() {
    let (mut h, _cmds) = harness(vec![Update::Pods(vec![pod_with(
        serde_json::json!({"networkVolumeId": "vol123"}),
    )])]);
    h.run();
    h.get_by_label("Shut down").click();
    h.run();
    h.get_by_label_contains("keeps the models on the network volume");
    h.get_by_label_contains("costs $1.59/hr while it runs");
}

#[test]
fn a_dialog_for_a_pod_that_is_already_gone_just_closes() {
    let (mut h, _cmds) = harness(vec![]);
    h.state_mut().ui.confirm_shutdown = true;
    h.run();
    h.get_by_label("The pod is already gone.");
    h.get_by_label("OK").click();
    h.run();
    assert!(h.query_by_label("The pod is already gone.").is_none());
    assert!(!h.state().ui.allow_close, "this was not a close request");

    // Closing the window while the dialog says the pod is gone lets the window close.
    h.state_mut().ui.confirm_close = true;
    h.run();
    h.get_by_label("The pod is already gone.");
    h.get_by_label("OK").click();
    h.run();
    assert!(h.state().ui.allow_close);
}

fn request_close(h: &mut Harness<'static, App>) {
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    h.step();
}

#[test]
fn closing_the_window_with_a_pod_up_asks_first() {
    let (mut h, cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    request_close(&mut h);
    h.run();
    h.get_by_label("The pod is still running");
    assert!(h.state().ui.confirm_close);

    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label("The pod is still running").is_none());
    assert!(!h.state().ui.allow_close);

    request_close(&mut h);
    h.run();
    h.get_by_label("Keep it running and quit").click();
    h.run();
    assert!(h.state().ui.allow_close && !h.state().ui.confirm_close);
    assert!(sent(&cmds).is_empty(), "the pod is left alone");

    // Once the user has chosen to quit, a second close request goes through.
    request_close(&mut h);
    h.run();
    assert!(h.query_by_label("The pod is still running").is_none());
}

#[test]
fn closing_the_window_with_no_pod_just_closes() {
    let (mut h, _cmds) = harness(vec![]);
    h.run();
    request_close(&mut h);
    h.run();
    assert!(!h.state().ui.confirm_close);
}

#[test]
fn terminating_closes_the_tunnel_and_ends_the_pod_through_runpod() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "terminating_closes_the_tunnel_and_ends_the_pod_through_runpod",
    ) else {
        return;
    };
    // The window's Terminate runs the real RunPod client on a thread; here that
    // client talks to this mock (the child's environment points it there).
    let rp = rig.runpod(|route, _, _| {
        if route == "DELETE /pods/abc" {
            (200, "{}".into())
        } else {
            (404, "{}".into())
        }
    });
    let (mut h, cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    request_close(&mut h);
    h.run();
    h.get_by_label("The pod is still running");
    h.get_by_label("Terminate").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::CloseTunnel]);
    assert!(
        h.state().ui.allow_close && !h.state().ui.confirm_close && !h.state().ui.confirm_shutdown
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while rp.count("DELETE /pods/abc") == 0 || h.state().st.log.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the pod was never terminated"
        );
        std::thread::sleep(Duration::from_millis(50));
        h.state_mut().drain();
    }
    assert_eq!(rp.count("DELETE /pods/abc"), 1);
    assert_eq!(
        h.state().st.log.back().map(String::as_str),
        Some("terminated offrig-medium (abc)")
    );
}

#[test]
fn the_shutdown_button_terminates_without_closing_the_window() {
    let Some(rig) = Rig::enter(
        module_path!(),
        "the_shutdown_button_terminates_without_closing_the_window",
    ) else {
        return;
    };
    let rp = rig.runpod(|route, _, _| {
        if route == "DELETE /pods/abc" {
            (500, r#"{"error":"nope"}"#.into())
        } else {
            (404, "{}".into())
        }
    });
    let (mut h, cmds) = harness(vec![Update::Pods(vec![pod_with(serde_json::json!({}))])]);
    h.run();
    h.get_by_label("Shut down").click();
    h.run();
    h.get_by_label("Terminate").click();
    h.run();
    assert_eq!(sent(&cmds), [Cmd::CloseTunnel]);
    assert!(!h.state().ui.allow_close, "the window stays open");

    // RunPod refuses: the failure reaches the window as an error banner.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while h.state().st.error.is_none() {
        assert!(std::time::Instant::now() < deadline, "no error arrived");
        std::thread::sleep(Duration::from_millis(50));
        h.state_mut().drain();
    }
    assert!(
        h.state()
            .st
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("shutting down offrig-medium: "))
    );
    assert_eq!(rp.count("DELETE /pods/abc"), 1);
}

// --- the eframe entry point ---------------------------------------------------------------------------

#[test]
fn the_eframe_app_draws_the_same_window() {
    let (app, _cmds, _tx) = test_app();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1180.0, 900.0))
        .build_ui_state(
            |ui, app: &mut App| {
                let mut frame = eframe::Frame::_new_kittest();
                eframe::App::ui(app, ui, &mut frame);
            },
            app,
        );
    h.run();
    h.get_by_label("Profiles");
}
