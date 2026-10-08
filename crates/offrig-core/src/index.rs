//! The project index: documents, code and logs cut into chunks, plus the active
//! records, searched by keywords (FTS5 BM25) and by meaning (embeddings), fused by
//! reciprocal rank fusion. Design: docs/verifier-design.md, section 1.
//!
//! Evidence behind the choices (docs/verifier-design.md, "Retrieval"): BM25 stays as
//! the floor because dense retrievers often lose to it out of domain (BEIR, Thakur et
//! al. 2021); RRF with k = 60 beats either ranker with no tuning (Cormack et al.
//! 2009); a header on each chunk gives the retriever context (Anthropic 2024); int8
//! vectors keep about 99% of retrieval quality at a quarter of the size.
//!
//! Embeddings are computed by a dedicated CPU-only Ollama (`embed_url`), never the
//! shared one on 11434 and never a GPU. Search by meaning never silently degrades: a
//! missing model or a different model is an error that says what to run.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::{Config, LOCAL_OLLAMA_PORT};
use crate::error::{Error, Result};
use crate::lanes::{LANE_COUNT, LANE_PORT_BASE, LANE_PORT_STEP, SIDECAR_PORT_BASE};
use crate::ollama::Ollama;
use crate::store::{NewChunk, Query, Record, Store};

/// The dedicated CPU-only Ollama the embeddings come from. The port sits clear of the
/// local Ollama (11434), the plain lane (11435, 11436), the lane range (11500..11628)
/// and the side-car range (11700..11764).
pub const DEFAULT_EMBED_URL: &str = "http://127.0.0.1:11490";
pub const EMBED_URL_ENV: &str = "OFFRIG_EMBED_URL";
pub const DEFAULT_EMBED_MODEL: &str = "nomic-embed-text";

/// Chunk size, in characters (about 200 to 500 tokens).
pub const MIN_CHARS: usize = 800;
pub const MAX_CHARS: usize = 2000;
/// Files larger than this are not indexed.
pub const MAX_FILE_BYTES: u64 = 1_000_000;
/// Chunks embedded per request.
pub const BATCH: usize = 32;
/// How many candidates each ranker contributes, and how many fused hits are kept.
pub const RANKER_DEPTH: usize = 50;
pub const FUSED_KEEP: usize = 20;
/// The RRF constant from Cormack et al.
pub const RRF_K: f64 = 60.0;

// ---------------------------------------------------------------- the embed endpoint

/// Why `port` may not be the embed port, if it may not.
pub fn embed_port_conflict(port: u16, tunnel_port: u16) -> Option<&'static str> {
    let lanes_top = LANE_PORT_BASE + LANE_COUNT * LANE_PORT_STEP;
    let sidecar_top = SIDECAR_PORT_BASE + LANE_COUNT;
    match port {
        0..=1023 => Some("a privileged port"),
        LOCAL_OLLAMA_PORT => Some("the shared local Ollama's port"),
        p if p == tunnel_port || p == tunnel_port.saturating_add(1) => {
            Some("the configured tunnel's port or its runner port")
        }
        11435 | 11436 => Some("the plain lane's tunnel or runner port"),
        p if (LANE_PORT_BASE..lanes_top).contains(&p) => Some("a project lane's port range"),
        p if (SIDECAR_PORT_BASE..sidecar_top).contains(&p) => Some("the side-car port range"),
        _ => None,
    }
}

fn url_port(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host_port = rest.split('/').next().unwrap_or("");
    host_port.rsplit_once(':')?.1.parse().ok()
}

/// The embed URL to use: `env` (the value of `OFFRIG_EMBED_URL`) when set, else the
/// config's. A URL whose port belongs to something else offrig runs is refused.
pub fn resolve_embed_url(cfg_url: &str, env: Option<&str>, tunnel_port: u16) -> Result<String> {
    let (url, from) = match env.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => (v, EMBED_URL_ENV),
        None => (cfg_url.trim(), "embed_url"),
    };
    let Some(port) = url_port(url) else {
        return Err(Error::Config(format!(
            "{from} = {url:?} has no port; use http://127.0.0.1:<port>"
        )));
    };
    if let Some(why) = embed_port_conflict(port, tunnel_port) {
        return Err(Error::Config(format!(
            "{from} = {url} uses port {port}, which is {why}; embeddings need their own CPU-only Ollama, e.g. {DEFAULT_EMBED_URL}"
        )));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// The embed URL for this machine: the env var, else the config file, else the default.
pub fn embed_url() -> Result<String> {
    let (url, tunnel) = Config::load()
        .map(|c| (c.embed_url, c.tunnel_port))
        .unwrap_or_else(|_| (DEFAULT_EMBED_URL.to_string(), 11435));
    resolve_embed_url(&url, std::env::var(EMBED_URL_ENV).ok().as_deref(), tunnel)
}

/// A client for the embed endpoint.
pub fn embed_client() -> Result<Ollama> {
    Ok(Ollama::new(&embed_url()?))
}

fn start_line(base: &str) -> String {
    let hostport = base.split("://").nth(1).unwrap_or(base);
    format!(
        "start a CPU-only Ollama for embeddings with `OLLAMA_HOST={hostport} CUDA_VISIBLE_DEVICES=-1 ollama serve`, then `OLLAMA_HOST={hostport} ollama pull {DEFAULT_EMBED_MODEL}`"
    )
}

/// Embed through the CPU-only endpoint. A server that is not there is reported with
/// the line that starts it; offrig never falls back to the shared Ollama.
/// What a text is embedded as. Some models were trained with a task prefix and lose
/// retrieval quality without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Document,
    Query,
}

/// The task prefix `model` expects: nomic-embed-text was trained with
/// `search_document: ` and `search_query: ` and its model card says to always use them.
/// Other models get none.
pub fn task_prefix(model: &str, task: Task) -> &'static str {
    if !model.starts_with("nomic-embed-text") {
        return "";
    }
    match task {
        Task::Document => "search_document: ",
        Task::Query => "search_query: ",
    }
}

pub fn embed_call(
    ollama: &Ollama,
    model: &str,
    task: Task,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>> {
    let prefix = task_prefix(model, task);
    let inputs: Vec<String> = inputs.iter().map(|t| format!("{prefix}{t}")).collect();
    ollama.embed(model, &inputs, true).map_err(|e| match e {
        Error::Http { .. } => Error::Refused(format!(
            "no embedding server answers at {}; {}",
            ollama.base(),
            start_line(ollama.base())
        )),
        other => other,
    })
}

/// Refuse when the index was built with another model than `model`.
pub fn check_model(store: &Store, model: &str) -> Result<()> {
    match store.setting("embed_model")? {
        Some(stored) if stored != model => Err(Error::Refused(format!(
            "the project index was built with {stored}, not {model}; run `offrig index --rebuild` to rebuild it with the new model"
        ))),
        _ => Ok(()),
    }
}

/// Refuse when vectors of size `dim` do not match the index.
pub fn check_dim(store: &Store, dim: usize) -> Result<()> {
    match store
        .setting("embed_dim")?
        .and_then(|d| d.parse::<usize>().ok())
    {
        Some(stored) if stored != dim => Err(Error::Refused(format!(
            "the project index holds {stored}-dimension vectors but the embedding model returned {dim}; run `offrig index --rebuild`"
        ))),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- pure functions

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The line that opens every chunk and is indexed and embedded with it.
pub fn header(source: &str, kind: &str, title: &str) -> String {
    format!("[{source} \u{b7} {kind} \u{b7} {title}]")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Doc,
    Code,
    Log,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Doc => "doc",
            SourceKind::Code => "code",
            SourceKind::Log => "log",
        }
    }
}

/// What kind of source a file is, from its extension. Unknown text counts as a doc.
pub fn kind_for(path: &Path) -> SourceKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "log" => SourceKind::Log,
        "rs" | "py" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "c"
        | "h" | "cc" | "cpp" | "hpp" | "cs" | "rb" | "php" | "swift" | "lua" | "sh" | "ps1"
        | "sql" | "toml" | "yaml" | "yml" | "json" | "html" | "css" | "gd" => SourceKind::Code,
        _ => SourceKind::Doc,
    }
}

fn is_decl(line: &str) -> bool {
    if line.starts_with(char::is_whitespace) || line.is_empty() {
        return false;
    }
    const STARTS: [&str; 18] = [
        "fn ",
        "pub ",
        "impl",
        "struct ",
        "enum ",
        "trait ",
        "mod ",
        "def ",
        "class ",
        "function ",
        "export ",
        "async ",
        "func ",
        "type ",
        "interface ",
        "const ",
        "static ",
        "#[",
    ];
    STARTS.iter().any(|s| line.starts_with(s))
}

fn is_heading(line: &str) -> bool {
    line.starts_with('#') && line.trim_start_matches('#').starts_with(' ')
}

struct Block {
    title: String,
    text: String,
    structural: bool,
}

/// Split `s` into pieces of at most `max` characters, on line breaks where it can.
fn split_long(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.split('\n') {
        let mut line = line;
        while line.chars().count() > max {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let cut = line.char_indices().nth(max).map_or(line.len(), |(i, _)| i);
            out.push(line[..cut].to_string());
            line = &line[cut..];
        }
        let extra = usize::from(!cur.is_empty());
        if !cur.is_empty() && cur.chars().count() + extra + line.chars().count() > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Cut `text` into chunks of about 800 to 2000 characters on paragraph boundaries
/// (and, for code, at top-level declarations), each opening with its header line.
pub fn chunk(source: &str, kind: SourceKind, text: &str) -> Vec<NewChunk> {
    let file_title = source.rsplit('/').next().unwrap_or(source).to_string();
    let mut title = file_title.clone();
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut cur_title = title.clone();
    let mut cur_structural = false;
    let flush = |cur: &mut Vec<&str>, title: &str, structural: bool, blocks: &mut Vec<Block>| {
        if !cur.is_empty() {
            blocks.push(Block {
                title: title.to_string(),
                text: cur.join("\n"),
                structural,
            });
            cur.clear();
        }
    };
    for line in text.lines() {
        let line = line.trim_end();
        let structure = match kind {
            SourceKind::Doc => is_heading(line),
            SourceKind::Code => is_decl(line),
            SourceKind::Log => false,
        };
        if line.is_empty() {
            flush(&mut cur, &cur_title, cur_structural, &mut blocks);
            continue;
        }
        if structure {
            flush(&mut cur, &cur_title, cur_structural, &mut blocks);
            title = line
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(80)
                .collect();
        }
        if cur.is_empty() {
            cur_title = title.clone();
            cur_structural = structure;
        }
        cur.push(line);
    }
    flush(&mut cur, &cur_title, cur_structural, &mut blocks);

    // Pack blocks into chunks.
    let mut packed: Vec<(String, String)> = Vec::new();
    let mut open: Option<(String, String)> = None;
    for b in blocks {
        for (i, piece) in split_long(&b.text, MAX_CHARS).into_iter().enumerate() {
            let structural = b.structural && i == 0;
            if let Some((t, body)) = open.take() {
                let len = body.chars().count();
                let fits = len + 2 + piece.chars().count() <= MAX_CHARS;
                if fits && !(structural && len >= MIN_CHARS) {
                    open = Some((t, format!("{body}\n\n{piece}")));
                    continue;
                }
                packed.push((t, body));
            }
            open = Some((b.title.clone(), piece));
        }
    }
    packed.extend(open);
    // A scrap at the end joins the chunk before it when that still fits.
    if packed.len() > 1 {
        let tail_len = packed[packed.len() - 1].1.chars().count();
        let prev_len = packed[packed.len() - 2].1.chars().count();
        if tail_len < MIN_CHARS / 4 && prev_len + 2 + tail_len <= MAX_CHARS {
            let (_, tail) = packed.pop().unwrap_or_default();
            if let Some(prev) = packed.last_mut() {
                prev.1.push_str("\n\n");
                prev.1.push_str(&tail);
            }
        }
    }
    packed
        .into_iter()
        .enumerate()
        .map(|(i, (title, body))| NewChunk {
            ordinal: i as i64,
            body: format!("{}\n{body}", header(source, kind.as_str(), &title)),
            title,
        })
        .collect()
}

/// int8 quantisation with one scale per vector: `x ~ q * scale`.
pub fn quantize(v: &[f32]) -> (Vec<i8>, f32) {
    let max = v.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        return (vec![0; v.len()], 0.0);
    }
    let scale = max / 127.0;
    let q = v
        .iter()
        .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8)
        .collect();
    (q, scale)
}

pub fn dequantize(q: &[i8], scale: f32) -> Vec<f32> {
    q.iter().map(|b| f32::from(*b) * scale).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Cosine of a float query against a stored int8 vector, dequantised.
pub fn cosine_q(query: &[f32], q: &[i8], scale: f32) -> f32 {
    cosine(query, &dequantize(q, scale))
}

/// Reciprocal rank fusion: each list adds `1 / (k + rank)` (rank from 1) to an id's
/// score. Highest score first; ties keep the order ids were first seen.
pub fn rrf(lists: &[Vec<i64>], k: f64, keep: usize) -> Vec<(i64, f64)> {
    let mut scores: Vec<(i64, f64)> = Vec::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            let add = 1.0 / (k + (rank + 1) as f64);
            match scores.iter_mut().find(|(i, _)| i == id) {
                Some((_, s)) => *s += add,
                None => scores.push((*id, add)),
            }
        }
    }
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    scores.truncate(keep);
    scores
}

// ---------------------------------------------------------------- embedding the chunks

/// Embed every chunk that has no vector yet, in batches of [`BATCH`]. Pins the index's
/// model and dimension on the first vectors, and refuses a model or size that differs
/// from what is pinned. Returns how many chunks were embedded.
pub fn embed_pending(store: &Store, ollama: &Ollama, model: &str) -> Result<usize> {
    check_model(store, model)?;
    let mut done = 0;
    loop {
        let todo = store.unembedded(BATCH)?;
        if todo.is_empty() {
            return Ok(done);
        }
        let texts: Vec<String> = todo.iter().map(|(_, t)| t.clone()).collect();
        let vecs = embed_call(ollama, model, Task::Document, &texts)?;
        let dim = vecs.first().map_or(0, Vec::len);
        check_dim(store, dim)?;
        let rows: Vec<(i64, Vec<i8>, f32)> = todo
            .iter()
            .zip(&vecs)
            .map(|((id, _), v)| {
                let (q, scale) = quantize(v);
                (*id, q, scale)
            })
            .collect();
        store.put_embeddings(model, dim, &rows)?;
        done += rows.len();
    }
}

// ---------------------------------------------------------------- search

/// The model the index was built with, or the default when nothing is embedded yet.
pub fn index_model(store: &Store) -> Result<String> {
    Ok(store
        .setting("embed_model")?
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string()))
}

/// Embed one query. Needs no store, so a caller can do it outside any lock.
pub fn embed_query(ollama: &Ollama, model: &str, text: &str) -> Result<Vec<f32>> {
    Ok(embed_call(ollama, model, Task::Query, &[text.to_string()])?
        .into_iter()
        .next()
        .unwrap_or_default())
}

/// The query vector for hybrid search, from the index's own model. Refuses when the
/// server returns a different size than the index holds.
pub fn query_vector(store: &Store, ollama: &Ollama, text: &str) -> Result<Vec<f32>> {
    let v = embed_query(ollama, &index_model(store)?, text)?;
    check_dim(store, v.len())?;
    Ok(v)
}

/// Chunk ids by hybrid ranking: BM25 top 50 and cosine top 50 fused by RRF, top 20 kept.
pub fn fuse(store: &Store, text: &str, qvec: &[f32], kind: Option<&str>) -> Result<Vec<i64>> {
    let keyword = store.fts_chunks(text, kind, RANKER_DEPTH)?;
    let mut scored: Vec<(i64, f32)> = store
        .embedding_rows(kind)?
        .into_iter()
        .filter(|(_, _, bytes)| bytes.len() == qvec.len())
        .map(|(id, scale, bytes)| {
            let q: Vec<i8> = bytes.iter().map(|b| *b as i8).collect();
            (id, cosine_q(qvec, &q, scale))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let dense: Vec<i64> = scored
        .into_iter()
        .take(RANKER_DEPTH)
        .map(|(i, _)| i)
        .collect();
    Ok(rrf(&[keyword, dense], RRF_K, FUSED_KEEP)
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// Active records ranked by hybrid search. `qvec = None` is keyword search over the
/// records. Kind and task filters apply to either.
pub fn search_records(store: &Store, q: &Query, qvec: Option<&[f32]>) -> Result<Vec<Record>> {
    let Some(qvec) = qvec else {
        return store.search(q);
    };
    let limit = if q.limit == 0 { 8 } else { q.limit.min(50) };
    let ids = fuse(store, &q.text, qvec, Some("record"))?;
    let mut out = Vec::new();
    for c in store.chunks_by_ids(&ids)? {
        let Some(id) = c
            .source
            .strip_prefix("record:")
            .and_then(|i| i.parse().ok())
        else {
            continue;
        };
        let Some(r) = store.get(id)? else { continue };
        if r.status == "active"
            && q.kind.is_none_or(|k| k == r.kind)
            && q.task_id.is_none_or(|t| Some(t) == r.task_id)
        {
            out.push(r);
        }
        if out.len() == limit {
            break;
        }
    }
    Ok(out)
}

/// Which search ran, for the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Keyword,
    Hybrid,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Keyword => "keyword",
            Mode::Hybrid => "hybrid",
        }
    }
}

/// Which search a request gets. `requested = None` picks hybrid when the index has
/// embeddings and keyword otherwise. An explicit `Hybrid` without an index is an
/// error, never a quiet keyword search.
pub fn plan_mode(store: &Store, requested: Option<Mode>) -> Result<Mode> {
    let have = store.has_embeddings()?;
    match requested {
        Some(Mode::Hybrid) if !have => Err(Error::Refused(
            "hybrid search needs the project index; run `offrig index <paths>` (records are embedded too), or ask for mode=keyword".into(),
        )),
        Some(m) => Ok(m),
        None if have => Ok(Mode::Hybrid),
        None => Ok(Mode::Keyword),
    }
}

/// Finish a hybrid search with a query vector already in hand (see [`embed_query`]).
pub fn search_with_vector(store: &Store, q: &Query, qvec: &[f32]) -> Result<Vec<Record>> {
    check_dim(store, qvec.len())?;
    search_records(store, q, Some(qvec))
}

/// Memory search in the requested mode, embedding the query itself when hybrid.
pub fn memory_search(
    store: &Store,
    ollama: &Ollama,
    q: &Query,
    requested: Option<Mode>,
) -> Result<(Vec<Record>, Mode)> {
    let mode = plan_mode(store, requested)?;
    match mode {
        Mode::Keyword => Ok((search_records(store, q, None)?, mode)),
        Mode::Hybrid => {
            let v = query_vector(store, ollama, &q.text)?;
            Ok((search_records(store, q, Some(&v))?, mode))
        }
    }
}

/// Records for a handoff prompt: hybrid when the index has embeddings and the embed
/// server answers, keyword otherwise. A prompt is built mid-run, so an unreachable
/// embed server costs the ranking, not the turn.
pub fn records_for_prompt(store: &Store, q: &Query) -> Result<Vec<Record>> {
    if store.has_embeddings()?
        && let Ok(ollama) = embed_client()
        && let Ok(v) = query_vector(store, &ollama, &q.text)
    {
        return search_records(store, q, Some(&v));
    }
    store.search(q)
}

// ---------------------------------------------------------------- walking and indexing

/// A reason a file is not indexed, if it is not: secrets-like names.
pub fn secret_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with(".env")
        || n.starts_with("id_")
        || n.contains("credentials")
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|e| n.ends_with(e))
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IndexReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub skipped: Vec<(String, &'static str)>,
    pub chunks: usize,
    pub embedded: usize,
}

fn rel_source(path: &Path, project: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let base = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    abs.strip_prefix(&base)
        .unwrap_or(&abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The files under `roots` that could be indexed, honouring `.gitignore`. `.git` and
/// `.offrig` are never walked.
pub fn walk(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                n != ".git" && n != ".offrig"
            })
            .build();
        out.extend(
            walker
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .map(ignore::DirEntry::into_path),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// Chunk and store every changed file under `roots` (unchanged sources are skipped by
/// hash), then embed whatever has no vector. `force` re-chunks unchanged sources too.
pub fn index_paths(
    store: &Store,
    ollama: &Ollama,
    model: &str,
    project: &Path,
    roots: &[PathBuf],
    force: bool,
) -> Result<IndexReport> {
    check_model(store, model)?;
    let mut rep = IndexReport::default();
    for path in walk(roots) {
        let source = rel_source(&path, project);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if name.as_deref().is_some_and(secret_like) {
            rep.skipped.push((source, "secrets-like file"));
            continue;
        }
        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
        if size > MAX_FILE_BYTES {
            rep.skipped.push((source, "over 1 MB"));
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            rep.skipped.push((source, "unreadable"));
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            rep.skipped.push((source, "binary"));
            continue;
        }
        let Ok(text) = String::from_utf8(bytes.clone()) else {
            rep.skipped.push((source, "not UTF-8 text"));
            continue;
        };
        if text.trim().is_empty() {
            rep.skipped.push((source, "empty"));
            continue;
        }
        let sha = sha256_hex(&bytes);
        if !force && store.source_sha(&source)?.as_deref() == Some(sha.as_str()) {
            rep.unchanged += 1;
            continue;
        }
        let kind = kind_for(&path);
        let chunks = chunk(&source, kind, &text);
        store.replace_source(&source, kind.as_str(), &sha, &chunks)?;
        rep.indexed += 1;
        rep.chunks += chunks.len();
    }
    rep.embedded = embed_pending(store, ollama, model)?;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(n: usize, ch: char) -> String {
        std::iter::repeat_n(ch, n).collect()
    }

    fn body_of(c: &NewChunk) -> &str {
        c.body.split_once('\n').map_or("", |x| x.1)
    }

    #[test]
    fn documents_split_on_paragraphs_within_the_size_band() {
        let text: Vec<String> = (0..12).map(|i| para(500, (b'a' + i) as char)).collect();
        let chunks = chunk("docs/a.md", SourceKind::Doc, &text.join("\n\n"));
        assert!(chunks.len() > 1);
        for (i, c) in chunks.iter().enumerate() {
            let n = body_of(c).chars().count();
            assert!(n <= MAX_CHARS, "chunk {i} has {n}");
            if i + 1 < chunks.len() {
                assert!(n >= MIN_CHARS, "chunk {i} has {n}");
            }
            assert_eq!(c.ordinal, i as i64);
            assert!(c.body.starts_with("[docs/a.md \u{b7} doc \u{b7} a.md]\n"));
        }
        // Nothing is lost and no paragraph is cut.
        let all: String = chunks.iter().map(body_of).collect::<Vec<_>>().join("\n\n");
        assert_eq!(all, text.join("\n\n"));
    }

    #[test]
    fn a_heading_starts_a_chunk_once_the_last_one_is_big_enough() {
        let big = para(900, 'x');
        let text = format!("# One\n\n{big}\n\n## Two\n\nshort tail paragraph\n\n{big}");
        let chunks = chunk("a.md", SourceKind::Doc, &text);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0].title, "One");
        assert_eq!(chunks[1].title, "Two");
        assert!(chunks[1].body.contains("## Two"));
        // A small first section does not split off.
        let small = chunk("a.md", SourceKind::Doc, "# One\n\nhi\n\n## Two\n\nthere");
        assert_eq!(small.len(), 1);
    }

    #[test]
    fn code_breaks_at_declarations_and_titles_them() {
        let f1 = format!("fn first() {{\n    {}\n}}", para(900, 'a'));
        let f2 = format!("fn second() {{\n    {}\n}}", para(900, 'b'));
        let chunks = chunk("src/lib.rs", SourceKind::Code, &format!("{f1}\n{f2}"));
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].title, "fn first() {");
        assert_eq!(chunks[1].title, "fn second() {");
        assert!(chunks[1].body.contains("code \u{b7} fn second() {"));
    }

    #[test]
    fn oversized_blocks_and_lines_are_cut_to_fit() {
        let long_line = para(5000, 'z');
        let chunks = chunk("big.log", SourceKind::Log, &long_line);
        assert_eq!(chunks.len(), 3);
        assert!(
            chunks
                .iter()
                .all(|c| body_of(c).chars().count() <= MAX_CHARS)
        );
        assert!(chunk("e.md", SourceKind::Doc, "  \n\n").is_empty());
    }

    #[test]
    fn quantising_round_trips_with_a_small_error() {
        let v: Vec<f32> = (0..64).map(|i| ((i as f32) * 0.37).sin() * 3.0).collect();
        let (q, scale) = quantize(&v);
        let back = dequantize(&q, scale);
        let worst = v
            .iter()
            .zip(&back)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst <= scale, "error {worst} over one step {scale}");
        assert!(cosine(&v, &back) > 0.9999);
        assert!((cosine_q(&v, &q, scale) - cosine(&v, &back)).abs() < 1e-6);
        assert_eq!(quantize(&[0.0, 0.0]), (vec![0, 0], 0.0));
    }

    #[test]
    fn nomic_gets_its_task_prefixes_and_other_models_none() {
        assert_eq!(
            task_prefix("nomic-embed-text", Task::Document),
            "search_document: "
        );
        assert_eq!(
            task_prefix("nomic-embed-text:v1.5", Task::Query),
            "search_query: "
        );
        assert_eq!(task_prefix("bge-m3", Task::Query), "");
    }

    #[test]
    fn cosine_orders_vectors_and_survives_zeros() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn fusion_rewards_agreement_and_keeps_rank_order() {
        // 3 is second in both lists; 1 and 2 each top one list.
        let fused = rrf(&[vec![1, 3, 4], vec![2, 3, 5]], RRF_K, 10);
        let ids: Vec<i64> = fused.iter().map(|(i, _)| *i).collect();
        assert_eq!(ids[0], 3);
        assert_eq!(&ids[1..3], &[1, 2], "tie keeps first-seen order");
        assert!((fused[0].1 - 2.0 / 62.0).abs() < 1e-12);
        assert_eq!(rrf(&[vec![1, 2, 3]], RRF_K, 2).len(), 2);
        assert!(rrf(&[], RRF_K, 5).is_empty());
    }

    #[test]
    fn secrets_like_names_are_recognised() {
        for n in [
            ".env",
            ".env.local",
            "server.pem",
            "a.KEY",
            "id_rsa",
            "id_ed25519.pub",
            "aws_credentials.json",
        ] {
            assert!(secret_like(n), "{n}");
        }
        for n in ["main.rs", "README.md", "keyboard.rs", "idle.rs"] {
            assert!(!secret_like(n), "{n}");
        }
    }

    #[test]
    fn the_embed_port_collides_with_nothing_offrig_runs() {
        let default = url_port(DEFAULT_EMBED_URL).expect("port");
        let tunnel = Config::default().tunnel_port;
        assert_eq!(embed_port_conflict(default, tunnel), None);
        // Nothing the lanes or side-cars can take is the embed port.
        let mut taken = vec![LOCAL_OLLAMA_PORT, tunnel, tunnel + 1, 11435, 11436];
        for i in 0..LANE_COUNT {
            let p = crate::lanes::lane_port(i);
            taken.extend([p, p + 1, SIDECAR_PORT_BASE + i]);
        }
        assert!(!taken.contains(&default));
        for p in taken {
            assert!(embed_port_conflict(p, tunnel).is_some(), "{p}");
        }
        assert!(embed_port_conflict(80, tunnel).is_some());
        // A configured tunnel elsewhere is protected too.
        assert!(embed_port_conflict(12000, 12000).is_some());
    }

    #[test]
    fn a_colliding_embed_url_is_refused_and_the_env_wins() {
        let tunnel = 11435;
        let e = resolve_embed_url("http://127.0.0.1:11434", None, tunnel)
            .expect_err("shared ollama")
            .to_string();
        assert!(
            e.contains("11434") && e.contains("shared local Ollama"),
            "{e}"
        );
        assert!(resolve_embed_url("http://127.0.0.1:11435", None, tunnel).is_err());
        assert!(resolve_embed_url("http://127.0.0.1:11520/", None, tunnel).is_err());
        assert!(resolve_embed_url("http://127.0.0.1:11705", None, tunnel).is_err());
        assert!(resolve_embed_url("http://localhost", None, tunnel).is_err());
        // The env var overrides the config, and is checked like it.
        assert_eq!(
            resolve_embed_url(
                "http://127.0.0.1:11434",
                Some("http://127.0.0.1:11491/"),
                tunnel
            )
            .expect("env"),
            "http://127.0.0.1:11491"
        );
        assert!(
            resolve_embed_url(DEFAULT_EMBED_URL, Some("http://127.0.0.1:11434"), tunnel).is_err()
        );
        assert_eq!(
            resolve_embed_url(DEFAULT_EMBED_URL, Some("  "), tunnel).expect("blank env"),
            DEFAULT_EMBED_URL
        );
    }

    #[test]
    fn a_different_model_or_size_names_the_rebuild() {
        let s = Store::open_in_memory().expect("store");
        assert!(check_model(&s, "anything").is_ok() && check_dim(&s, 9).is_ok());
        s.set_setting("embed_model", "nomic-embed-text")
            .expect("set");
        s.set_setting("embed_dim", "768").expect("set");
        assert!(check_model(&s, "nomic-embed-text").is_ok());
        let e = check_model(&s, "other").expect_err("model").to_string();
        assert!(e.contains("offrig index --rebuild"), "{e}");
        let e = check_dim(&s, 384).expect_err("dim").to_string();
        assert!(
            e.contains("768") && e.contains("offrig index --rebuild"),
            "{e}"
        );
    }
}
