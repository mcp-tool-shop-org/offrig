//! The project index against a mock embedding server: walking and incremental
//! indexing, hybrid memory search, and every way the embedding endpoint can be wrong.

mod support;

use offrig_core::index::{self, Mode};
use offrig_core::ollama::Ollama;
use offrig_core::store::{Kind, NewRecord, Query, Store};
use serde_json::{Value, json};
use support::{dead_url, serve, temp};

/// A vector per input: axis 0 for "alpha" or "first letter", axis 1 for "beta".
fn vector(text: &str) -> Vec<f64> {
    let t = text.to_lowercase();
    vec![
        f64::from(u8::from(t.contains("alpha") || t.contains("first letter"))),
        f64::from(u8::from(t.contains("beta"))),
        0.1,
    ]
}

fn embed_reply(body: &str) -> (u16, String) {
    let v: Value = serde_json::from_str(body).expect("json");
    let rows: Vec<Vec<f64>> = v["input"]
        .as_array()
        .expect("input")
        .iter()
        .map(|t| vector(t.as_str().unwrap_or("")))
        .collect();
    (200, json!({ "embeddings": rows }).to_string())
}

fn record(s: &Store, kind: Kind, body: &str) -> i64 {
    s.record(NewRecord {
        kind: Some(kind),
        body: body.into(),
        author: "test".into(),
        ..Default::default()
    })
    .expect("record")
}

fn q(text: &str) -> Query {
    Query {
        text: text.into(),
        limit: 8,
        ..Default::default()
    }
}

#[test]
fn indexing_walks_skips_and_is_incremental() {
    let m = serve(|r, _| match r.route().as_str() {
        "POST /api/embed" => embed_reply(&r.body),
        _ => (404, "{}".into()),
    });
    let o = Ollama::new(&m.url);
    let dir = temp("index-walk");
    std::fs::create_dir_all(dir.join("docs")).expect("dir");
    std::fs::create_dir_all(dir.join("target")).expect("dir");
    std::fs::write(dir.join(".gitignore"), "target/\n").expect("ignore");
    std::fs::write(dir.join("docs/a.md"), "# Alpha\n\nalpha notes").expect("a");
    std::fs::write(dir.join("src.rs"), "fn beta() {}\n").expect("src");
    std::fs::write(dir.join("target/out.txt"), "alpha build output").expect("ignored");
    std::fs::write(dir.join(".env"), "KEY=alpha").expect("env");
    std::fs::write(dir.join("id_rsa"), "alpha").expect("key");
    std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).expect("bin");
    std::fs::write(dir.join("huge.txt"), vec![b'a'; 1_100_000]).expect("big");
    std::fs::write(dir.join("empty.md"), "  \n").expect("empty");
    std::fs::create_dir_all(dir.join(".offrig")).expect("dir");
    std::fs::write(dir.join(".offrig/note.md"), "alpha").expect("offrig");

    let s = Store::open_in_memory().expect("store");
    let roots = [dir.clone()];
    let run = |model: &str| index::index_paths(&s, &o, model, &dir, &roots, false);
    let rep = run("nomic-embed-text").expect("index");
    assert_eq!(
        (rep.indexed, rep.unchanged, rep.chunks, rep.embedded),
        (3, 0, 3, 3)
    );
    let why = |name: &str| rep.skipped.iter().find(|(s, _)| s == name).map(|(_, w)| *w);
    assert_eq!(why(".env"), Some("secrets-like file"));
    assert_eq!(why("id_rsa"), Some("secrets-like file"));
    assert_eq!(why("blob.bin"), Some("binary"));
    assert_eq!(why("huge.txt"), Some("over 1 MB"));
    assert_eq!(why("empty.md"), Some("empty"));
    assert!(
        why("target/out.txt").is_none(),
        "gitignored files are never seen"
    );
    assert!(
        rep.skipped.iter().all(|(s, _)| !s.starts_with(".offrig")),
        "the project database directory is never walked"
    );
    let st = s.index_stats().expect("stats");
    assert_eq!(
        (st.model.as_deref(), st.dim),
        (Some("nomic-embed-text"), Some(3))
    );
    let kinds: Vec<_> = st
        .chunks_by_kind
        .iter()
        .map(|(k, n)| (k.as_str(), *n))
        .collect();
    assert_eq!(kinds, [("code", 1), ("doc", 2)]);
    assert_eq!(st.sources, 3, "a.md, src.rs and .gitignore");

    // Nothing changed: nothing is re-chunked or re-embedded.
    let calls = m.count("POST /api/embed");
    let again = run("nomic-embed-text").expect("again");
    assert_eq!((again.indexed, again.unchanged, again.embedded), (0, 3, 0));
    assert_eq!(m.count("POST /api/embed"), calls);

    // A changed file has its chunks replaced and only those embedded.
    std::fs::write(dir.join("src.rs"), "fn beta() { let x = 1; }\n").expect("edit");
    let third = run("nomic-embed-text").expect("third");
    assert_eq!((third.indexed, third.unchanged, third.embedded), (1, 2, 1));
    assert_eq!(s.index_stats().expect("stats").embedded, 3);

    // A different model is refused until the index is rebuilt.
    let e = run("other-model").expect_err("mismatch").to_string();
    assert!(e.contains("offrig index --rebuild"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn embedding_batches_of_32_on_the_cpu() {
    let m = serve(|r, _| match r.route().as_str() {
        "POST /api/embed" => embed_reply(&r.body),
        _ => (404, "{}".into()),
    });
    let s = Store::open_in_memory().expect("store");
    for i in 0..70 {
        record(&s, Kind::Fact, &format!("fact number {i}"));
    }
    let n = index::embed_pending(&s, &Ollama::new(&m.url), "nomic-embed-text").expect("embed");
    assert_eq!(n, 70);
    assert_eq!(m.count("POST /api/embed"), 3, "32 + 32 + 6");
    for r in m.requests() {
        let v: Value = serde_json::from_str(&r.body).expect("json");
        assert_eq!(v["options"]["num_gpu"], 0);
    }
}

#[test]
fn memory_search_is_hybrid_once_embedded_and_keyword_before() {
    let m = serve(|r, _| match r.route().as_str() {
        "POST /api/embed" => embed_reply(&r.body),
        _ => (404, "{}".into()),
    });
    let o = Ollama::new(&m.url);
    let s = Store::open_in_memory().expect("store");
    let a = record(&s, Kind::Decision, "alpha is the plan");
    let b = record(&s, Kind::Decision, "beta is the fallback");
    let c = record(&s, Kind::Fact, "the first letter of the Greek alphabet");
    let old = record(&s, Kind::Fact, "alpha was measured slow");
    let fresh = s
        .record(NewRecord {
            kind: Some(Kind::Fact),
            body: "alpha measured fast now".into(),
            author: "test".into(),
            supersedes: Some(old),
            reason: Some("remeasured".into()),
            ..Default::default()
        })
        .expect("supersede");

    // No embeddings yet: default is keyword, and asking for hybrid is an error.
    let (rs, mode) = index::memory_search(&s, &o, &q("alpha"), None).expect("keyword");
    assert_eq!(mode, Mode::Keyword);
    assert!(
        !rs.iter().any(|r| r.id == c),
        "keywords alone miss the paraphrase"
    );
    let e = index::memory_search(&s, &o, &q("alpha"), Some(Mode::Hybrid))
        .expect_err("no index")
        .to_string();
    assert!(e.contains("offrig index"), "{e}");
    assert_eq!(m.count("POST /api/embed"), 0);

    index::embed_pending(&s, &o, "nomic-embed-text").expect("embed");
    let (rs, mode) = index::memory_search(&s, &o, &q("alpha"), None).expect("hybrid");
    assert_eq!(mode, Mode::Hybrid);
    let ids: Vec<i64> = rs.iter().map(|r| r.id).collect();
    assert!(ids.contains(&a) && ids.contains(&c), "{ids:?}");
    assert!(
        ids.contains(&fresh) && !ids.contains(&old),
        "superseded stays out: {ids:?}"
    );
    assert!(
        !ids.contains(&b) || ids.iter().position(|i| *i == b) > ids.iter().position(|i| *i == a)
    );
    // Keyword mode still works on request, and filters apply to hybrid.
    let (kw, mode) = index::memory_search(&s, &o, &q("alpha"), Some(Mode::Keyword)).expect("kw");
    assert_eq!(mode, Mode::Keyword);
    assert!(!kw.iter().any(|r| r.id == c));
    let only_facts = Query {
        kind: Some(Kind::Fact),
        ..q("alpha")
    };
    let (rs, _) = index::memory_search(&s, &o, &only_facts, None).expect("filtered");
    assert!(rs.iter().all(|r| r.kind == Kind::Fact) && rs.iter().any(|r| r.id == c));
    // A withdrawn record is gone from hybrid results too.
    s.withdraw(c, "wrong").expect("withdraw");
    let (rs, _) = index::memory_search(&s, &o, &q("alpha"), None).expect("after");
    assert!(!rs.iter().any(|r| r.id == c));
}

#[test]
fn a_missing_model_a_dead_server_and_a_resized_model_are_all_errors() {
    let s = Store::open_in_memory().expect("store");
    record(&s, Kind::Fact, "alpha");
    // The model is not installed.
    let m = serve(|_, _| (404, r#"{"error":"model not found"}"#.into()));
    let e = index::embed_pending(&s, &Ollama::new(&m.url), "nomic-embed-text")
        .expect_err("404")
        .to_string();
    assert!(e.contains("ollama pull nomic-embed-text"), "{e}");
    // Nothing answers: the error names the CPU-only start line, not the shared Ollama.
    let e = index::embed_pending(&s, &Ollama::new(&dead_url()), "nomic-embed-text")
        .expect_err("dead")
        .to_string();
    assert!(
        e.contains("CUDA_VISIBLE_DEVICES=-1 ollama serve") && e.contains("OLLAMA_HOST="),
        "{e}"
    );
    assert!(!e.contains("11434"), "{e}");
    assert!(!s.has_embeddings().expect("has"), "nothing half-written");
    // Embed once, then the server starts returning another size.
    let ok = serve(|r, _| embed_reply(&r.body));
    index::embed_pending(&s, &Ollama::new(&ok.url), "nomic-embed-text").expect("embed");
    let wide = serve(|r, _| {
        let n = serde_json::from_str::<Value>(&r.body).expect("json")["input"]
            .as_array()
            .map_or(0, Vec::len);
        (
            200,
            json!({ "embeddings": vec![vec![1.0, 0.0, 0.0, 0.0]; n] }).to_string(),
        )
    });
    let e = index::memory_search(&s, &Ollama::new(&wide.url), &q("alpha"), None)
        .expect_err("resized")
        .to_string();
    assert!(e.contains("offrig index --rebuild"), "{e}");
    record(&s, Kind::Fact, "new alpha");
    assert!(index::embed_pending(&s, &Ollama::new(&wide.url), "nomic-embed-text").is_err());
}

#[test]
fn rebuilding_re_embeds_every_chunk_with_the_new_model() {
    let m = serve(|r, _| embed_reply(&r.body));
    let o = Ollama::new(&m.url);
    let s = Store::open_in_memory().expect("store");
    record(&s, Kind::Fact, "alpha");
    index::embed_pending(&s, &o, "model-a").expect("first");
    assert!(index::embed_pending(&s, &o, "model-b").is_err());
    s.clear_embeddings().expect("clear");
    assert_eq!(index::embed_pending(&s, &o, "model-b").expect("rebuilt"), 1);
    assert_eq!(
        s.setting("embed_model").expect("setting").as_deref(),
        Some("model-b")
    );
}
