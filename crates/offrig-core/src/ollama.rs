//! Ollama over the tunnel: what is installed, what is loaded, what a model can do,
//! and a chat check shaped like Zed's requests (OpenAI-compatible, streamed, tools).

use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};

pub struct Ollama {
    agent: ureq::Agent,
    base: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub details: TagDetails,
    /// The model's content digest, as `/api/tags` reports it.
    #[serde(default)]
    pub digest: String,
    /// Set when the entry is a stub that Ollama forwards to a hosted service.
    #[serde(default)]
    pub remote_host: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct TagDetails {
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Loaded {
    pub name: String,
    #[serde(default)]
    pub size_vram: u64,
    /// Total bytes the loaded model occupies (VRAM plus any CPU share).
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub context_length: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelInfo {
    pub tools: bool,
    pub images: bool,
    pub thinking: bool,
    /// The model's trained context window, when Ollama reports it.
    pub context_length: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatCheck {
    pub model: String,
    pub reply: String,
    pub streamed_chunks: usize,
    pub tool_call: Option<String>,
    pub seconds: f64,
}

#[derive(Deserialize)]
struct TagList {
    #[serde(default)]
    models: Vec<Tag>,
}

#[derive(Deserialize)]
struct LoadedList {
    #[serde(default)]
    models: Vec<Loaded>,
}

impl Ollama {
    pub fn new(base_url: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .new_agent();
        Self {
            agent,
            base: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// The base URL this client talks to.
    pub fn base(&self) -> &str {
        &self.base
    }

    fn get(&self, path: &str, what: &str) -> Result<Value> {
        let mut resp = self
            .agent
            .get(format!("{}{}", self.base, path))
            .call()
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| Error::http(what, e))?;
        if status != 200 {
            return Err(Error::Ollama(format!("{what}: http {status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| Error::decode(what, e))
    }

    fn post(&self, path: &str, body: &Value, what: &str) -> Result<Value> {
        let mut resp = self
            .agent
            .post(format!("{}{}", self.base, path))
            .send_json(body)
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| Error::http(what, e))?;
        if status != 200 {
            return Err(Error::Ollama(format!("{what}: http {status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| Error::decode(what, e))
    }
}

/// Model ids from an OpenAI-compatible `GET /v1/models`, which Ollama and SGLang both
/// serve.
pub fn model_ids(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl Ollama {
    /// True when `path` answers 200. SGLang's `/health` does so only once the model is
    /// loaded and warm; its `/v1/models` answers earlier.
    pub fn answers(&self, path: &str) -> bool {
        self.agent
            .get(format!("{}{}", self.base, path))
            .call()
            .is_ok_and(|r| r.status().as_u16() == 200)
    }

    pub fn openai_models(&self) -> Result<Vec<String>> {
        Ok(model_ids(&self.get("/v1/models", "model list")?))
    }
}

impl Ollama {
    pub fn version(&self) -> Result<String> {
        let v = self.get("/api/version", "ollama version")?;
        Ok(v["version"].as_str().unwrap_or_default().to_string())
    }

    pub fn tags(&self) -> Result<Vec<Tag>> {
        let v = self.get("/api/tags", "ollama tags")?;
        let list: TagList =
            serde_json::from_value(v).map_err(|e| Error::decode("ollama tags", e))?;
        Ok(list.models)
    }

    pub fn loaded(&self) -> Result<Vec<Loaded>> {
        let v = self.get("/api/ps", "ollama ps")?;
        let list: LoadedList =
            serde_json::from_value(v).map_err(|e| Error::decode("ollama ps", e))?;
        Ok(list.models)
    }

    pub fn info(&self, model: &str) -> Result<ModelInfo> {
        let v = self.post("/api/show", &json!({ "model": model }), "ollama show")?;
        Ok(parse_show(&v))
    }

    pub fn delete(&self, model: &str) -> Result<()> {
        let what = "ollama delete";
        let mut resp = self
            .agent
            .delete(format!("{}/api/delete", self.base))
            .force_send_body()
            .send_json(json!({ "model": model }))
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        if status == 200 {
            Ok(())
        } else {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            Err(Error::Ollama(format!("{what}: http {status}: {text}")))
        }
    }

    /// Load the model into VRAM now, so the first Zed request does not wait for it.
    pub fn warm(&self, model: &str) -> Result<()> {
        self.post(
            "/api/generate",
            &json!({ "model": model, "prompt": "", "keep_alive": -1, "stream": false }),
            "ollama warm",
        )
        .map(|_| ())
    }

    /// One non-streamed chat turn through the OpenAI-compatible endpoint. Returns the
    /// reply text. Reasoning models may put their thinking in a separate field; only
    /// the answer is returned.
    pub fn chat(&self, model: &str, prompt: &str, max_tokens: Option<u32>) -> Result<String> {
        self.chat_reply(model, prompt, max_tokens).map(|r| r.text)
    }

    /// A non-streamed chat turn with its token count.
    pub fn chat_reply(&self, model: &str, prompt: &str, max_tokens: Option<u32>) -> Result<Reply> {
        let what = "chat";
        let mut body = json!({
            "model": model,
            "stream": false,
            "messages": [{ "role": "user", "content": prompt }],
        });
        if let Some(n) = max_tokens {
            body["max_tokens"] = json!(n);
        }
        let v = self.post("/v1/chat/completions", &body, what)?;
        if let Some(err) = v.get("error") {
            return Err(Error::Ollama(format!("{what}: {err}")));
        }
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                Error::Ollama(format!("{what}: the reply carried no message content"))
            })?;
        let answer = strip_thinking(content);
        if answer.is_empty() && v["choices"][0]["finish_reason"] == "length" {
            return Err(Error::Ollama(format!(
                "{what}: the model spent its whole token limit thinking; raise max_tokens"
            )));
        }
        Ok(Reply {
            text: answer.to_string(),
            tokens: v["usage"]["completion_tokens"].as_i64(),
        })
    }

    /// A streamed chat with one tool offered, through the OpenAI-compatible endpoint
    /// Zed uses. Proves the model answers, streams, and can call a tool.
    pub fn chat_check(&self, model: &str) -> Result<ChatCheck> {
        let what = "chat check";
        let started = std::time::Instant::now();
        let body = json!({
            "model": model,
            "stream": true,
            "temperature": 0,
            "messages": [
                { "role": "system", "content": "You are a coding assistant. Use tools when they fit." },
                { "role": "user", "content": "Read the file src/main.rs using the read_file tool." }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read a file from the project",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }
            }]
        });
        let mut resp = self
            .agent
            .post(format!("{}/v1/chat/completions", self.base))
            .send_json(&body)
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(Error::Ollama(format!("{what}: http {status}: {text}")));
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut stream = SseChat::default();
        for line in reader.lines() {
            let line = line.map_err(|e| Error::io("reading the chat stream", e))?;
            if stream.feed(&line)? {
                break;
            }
        }
        if stream.chunks == 0 {
            return Err(Error::Ollama(format!(
                "{what}: the stream carried no chunks"
            )));
        }
        Ok(ChatCheck {
            model: model.to_string(),
            reply: stream.text,
            streamed_chunks: stream.chunks,
            tool_call: stream.tool_call,
            seconds: started.elapsed().as_secs_f64(),
        })
    }
}

/// One message of a multi-message chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Msg {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
        }
    }
}

/// How much a thinking model reasons before it answers. Ollama takes a boolean for
/// most models and a level for gpt-oss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkLevel {
    Off,
    On,
    Low,
    Medium,
    High,
}

impl ThinkLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkLevel::Off => "off",
            ThinkLevel::On => "on",
            ThinkLevel::Low => "low",
            ThinkLevel::Medium => "medium",
            ThinkLevel::High => "high",
        }
    }

    fn to_json(self) -> Value {
        match self {
            ThinkLevel::Off => json!(false),
            ThinkLevel::On => json!(true),
            other => json!(other.as_str()),
        }
    }
}

/// A chat sent to Ollama's native `/api/chat`, where `format` (a JSON schema) and
/// `think` live. Every option left `None` is left to the server's default.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Msg>,
    /// A JSON schema the reply must follow (structured output).
    pub format: Option<Value>,
    pub think: Option<ThinkLevel>,
    pub temperature: Option<f64>,
    pub seed: Option<i64>,
    pub num_ctx: Option<u32>,
    pub num_predict: Option<i32>,
}

impl ChatRequest {
    /// The `/api/chat` body: never streamed, sampling settings under `options`.
    pub fn body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "stream": false,
            "messages": self.messages,
        });
        if let Some(f) = &self.format {
            body["format"] = f.clone();
        }
        if let Some(t) = self.think {
            body["think"] = t.to_json();
        }
        let mut options = serde_json::Map::new();
        if let Some(t) = self.temperature {
            options.insert("temperature".into(), json!(t));
        }
        if let Some(n) = self.seed {
            options.insert("seed".into(), json!(n));
        }
        if let Some(n) = self.num_ctx {
            options.insert("num_ctx".into(), json!(n));
        }
        if let Some(n) = self.num_predict {
            options.insert("num_predict".into(), json!(n));
        }
        if !options.is_empty() {
            body["options"] = Value::Object(options);
        }
        body
    }
}

/// What `/api/chat` answered.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatResponse {
    pub content: String,
    /// The model's reasoning, when it keeps it apart from the answer.
    pub thinking: String,
    pub done_reason: String,
    pub eval_count: Option<u64>,
    pub prompt_eval_count: Option<u64>,
    /// Nanoseconds, as Ollama reports it.
    pub total_duration: Option<u64>,
}

/// Headroom below `num_ctx` that counts as full: prompt tokens plus reply tokens at or
/// above `num_ctx - CTX_FULL_SLACK` mean the window was used up.
pub const CTX_FULL_SLACK: u64 = 16;

/// `Some(ContextOverflow)` when the prompt and the reply together filled `num_ctx`.
///
/// Ollama silently shifts or truncates context when that happens, so the reply may have
/// been written without the start of the prompt. A prompt-only heuristic (a prompt count
/// far below the estimate) is deliberately not used: `prompt_eval_count` also drops when
/// Ollama reuses a cached prefix, so a low count is not evidence of truncation.
pub fn context_overflow(num_ctx: u32, resp: &ChatResponse) -> Option<Error> {
    let (p, e) = (resp.prompt_eval_count?, resp.eval_count.unwrap_or(0));
    (p + e >= u64::from(num_ctx).saturating_sub(CTX_FULL_SLACK)).then(|| {
        Error::ContextOverflow(format!(
            "prompt_eval_count {p} + eval_count {e} = {} reached num_ctx {num_ctx}; the server may have dropped the start of the prompt",
            p + e
        ))
    })
}

impl Ollama {
    /// One non-streamed chat through the native API, returned as the server answered it,
    /// including a reply cut at the token limit. Used to measure a prompt with
    /// `num_predict: 1`, where the cut is expected.
    pub fn chat_raw(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let what = "chat";
        let v = self.post("/api/chat", &req.body(), what)?;
        if let Some(err) = v.get("error") {
            return Err(Error::Ollama(format!("{what}: {err}")));
        }
        let message = &v["message"];
        let content = message["content"]
            .as_str()
            .ok_or_else(|| Error::Ollama(format!("{what}: the reply carried no message")))?;
        Ok(ChatResponse {
            content: content.to_string(),
            thinking: message["thinking"].as_str().unwrap_or_default().to_string(),
            done_reason: v["done_reason"].as_str().unwrap_or_default().to_string(),
            eval_count: v["eval_count"].as_u64(),
            prompt_eval_count: v["prompt_eval_count"].as_u64(),
            total_duration: v["total_duration"].as_u64(),
        })
    }

    /// A non-streamed multi-message chat through the native API. A reply that stopped
    /// at the token limit (`done_reason: length`) is a `Truncated` error, never a
    /// short answer, unless the window was full: then it is a `ContextOverflow`.
    pub fn chat_messages(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let resp = self.chat_raw(req)?;
        if resp.done_reason == "length" {
            if let Some(e) = req.num_ctx.and_then(|n| context_overflow(n, &resp)) {
                return Err(e);
            }
            return Err(Error::Truncated {
                message: format!(
                    "{} stopped at its token limit after {} tokens; raise num_predict",
                    req.model,
                    resp.eval_count.unwrap_or(0)
                ),
                thinking: resp.thinking,
            });
        }
        Ok(resp)
    }
}

impl Ollama {
    /// Embed `inputs` in one `/api/embed` call, one vector per input in order. With
    /// `cpu_only` the request carries `num_gpu: 0` so the model never loads on a GPU.
    /// A model the server does not have is reported with the pull line.
    pub fn embed(&self, model: &str, inputs: &[String], cpu_only: bool) -> Result<Vec<Vec<f32>>> {
        let what = "embed";
        let mut body = json!({ "model": model, "input": inputs });
        if cpu_only {
            body["options"] = json!({ "num_gpu": 0 });
        }
        let v = self.post("/api/embed", &body, what).map_err(|e| match &e {
            Error::Ollama(m) if m.contains("http 404") => Error::Refused(format!(
                "the embedding model {model} is not installed on {}; run `ollama pull {model}` against that host",
                self.base
            )),
            _ => e,
        })?;
        let rows = v["embeddings"]
            .as_array()
            .ok_or_else(|| Error::Ollama(format!("{what}: the reply carried no embeddings")))?;
        let out: Vec<Vec<f32>> = rows
            .iter()
            .map(|r| {
                r.as_array()
                    .map(|a| a.iter().map(|n| n.as_f64().unwrap_or(0.0) as f32).collect())
                    .unwrap_or_default()
            })
            .collect();
        let dim = out.first().map_or(0, Vec::len);
        if out.len() != inputs.len() || dim == 0 || out.iter().any(|r| r.len() != dim) {
            return Err(Error::Ollama(format!(
                "{what}: expected {} vectors of one size, got {}",
                inputs.len(),
                out.len()
            )));
        }
        Ok(out)
    }
}

pub fn parse_show(v: &Value) -> ModelInfo {
    let caps: Vec<&str> = v["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let context_length = v["model_info"].as_object().and_then(|m| {
        m.iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, n)| n.as_u64())
            .and_then(|n| u32::try_from(n).ok())
    });
    ModelInfo {
        tools: caps.contains(&"tools"),
        images: caps.contains(&"vision"),
        thinking: caps.contains(&"thinking"),
        context_length,
    }
}

/// Accumulates an OpenAI-style server-sent-event chat stream.
#[derive(Default)]
pub struct SseChat {
    pub text: String,
    pub chunks: usize,
    pub tool_call: Option<String>,
}

impl SseChat {
    /// Feed one line. Returns true at `data: [DONE]`.
    pub fn feed(&mut self, line: &str) -> Result<bool> {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(false);
        };
        if data == "[DONE]" {
            return Ok(true);
        }
        let v: Value =
            serde_json::from_str(data).map_err(|e| Error::decode("a chat stream chunk", e))?;
        if let Some(err) = v.get("error") {
            return Err(Error::Ollama(format!("chat stream error: {err}")));
        }
        self.chunks += 1;
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str() {
            self.text.push_str(t);
        }
        if let Some(calls) = delta["tool_calls"].as_array()
            && let Some(name) = calls.first().and_then(|c| c["function"]["name"].as_str())
            && self.tool_call.is_none()
        {
            self.tool_call = Some(name.to_string());
        }
        Ok(false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    /// Tokens generated, thinking included, when the server reports it.
    pub tokens: Option<i64>,
}

/// The answer without a thinking model's reasoning. Ollama normally moves it to a
/// separate field, but qwen3 on Ollama 0.35 was seen leaking it into the content,
/// closing tag included, even with thinking switched off (rehearsal, 2026-10-03).
pub fn strip_thinking(content: &str) -> &str {
    match content.rfind("</think>") {
        Some(i) => content[i + "</think>".len()..].trim(),
        None => content.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_reports_capabilities_and_context() {
        let v = json!({
            "capabilities": ["completion", "tools", "thinking"],
            "model_info": { "general.architecture": "qwen3moe", "qwen3moe.context_length": 262144 }
        });
        let i = parse_show(&v);
        assert!(i.tools && i.thinking && !i.images);
        assert_eq!(i.context_length, Some(262_144));
        assert_eq!(parse_show(&json!({})), ModelInfo::default());
    }

    #[test]
    fn sse_collects_text_tool_calls_and_stops_at_done() {
        let mut s = SseChat::default();
        let lines = [
            r#"data: {"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"ok"}}]}"#,
            "data: [DONE]",
        ];
        let mut done = false;
        for l in lines {
            done = s.feed(l).expect("valid chunk");
        }
        assert!(done);
        assert_eq!(s.chunks, 3);
        assert_eq!(s.tool_call.as_deref(), Some("read_file"));
        assert_eq!(s.text, "ok");
    }

    #[test]
    fn sse_surfaces_stream_errors() {
        let mut s = SseChat::default();
        assert!(
            s.feed(r#"data: {"error":{"message":"model not found"}}"#)
                .is_err()
        );
    }

    #[test]
    fn chat_bodies_carry_only_what_was_set() {
        let bare = ChatRequest {
            model: "m".into(),
            messages: vec![Msg::new("user", "hi")],
            ..Default::default()
        };
        let b = bare.body();
        assert_eq!(b["stream"], false);
        assert_eq!(b["messages"][0]["role"], "user");
        assert!(b.get("format").is_none() && b.get("think").is_none());
        assert!(b.get("options").is_none());
        let full = ChatRequest {
            format: Some(json!({"type": "object"})),
            think: Some(ThinkLevel::High),
            temperature: Some(0.0),
            seed: Some(7),
            num_ctx: Some(8192),
            num_predict: Some(512),
            ..bare
        };
        let b = full.body();
        assert_eq!(b["think"], "high");
        assert_eq!(b["format"]["type"], "object");
        assert_eq!(b["options"]["seed"], 7);
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(b["options"]["num_predict"], 512);
        assert_eq!(b["options"]["temperature"], 0.0);
    }

    #[test]
    fn think_levels_serialise_as_ollama_expects() {
        assert_eq!(ThinkLevel::Off.to_json(), json!(false));
        assert_eq!(ThinkLevel::On.to_json(), json!(true));
        assert_eq!(ThinkLevel::Low.to_json(), json!("low"));
        for l in [
            ThinkLevel::Off,
            ThinkLevel::On,
            ThinkLevel::Low,
            ThinkLevel::Medium,
            ThinkLevel::High,
        ] {
            assert!(!l.as_str().is_empty());
        }
    }

    #[test]
    fn thinking_is_stripped_from_answers() {
        assert_eq!(
            strip_thinking(
                "  plain answer 
"
            ),
            "plain answer"
        );
        assert_eq!(
            strip_thinking(
                "We are given the data...
Line 1
</think>

2026-10-03
v0.35.1"
            ),
            "2026-10-03
v0.35.1"
        );
        assert_eq!(strip_thinking("<think>hmm</think>"), "");
    }

    #[test]
    fn a_full_window_is_an_overflow_and_a_loaded_model_reports_its_split() {
        let r = |p, e| ChatResponse {
            prompt_eval_count: Some(p),
            eval_count: Some(e),
            ..Default::default()
        };
        assert!(context_overflow(1000, &r(900, 84)).is_some());
        assert!(context_overflow(1000, &r(900, 83)).is_none());
        assert!(context_overflow(1000, &ChatResponse::default()).is_none());
        let l: Loaded =
            serde_json::from_value(json!({"name": "m:1", "size": 100, "size_vram": 60}))
                .expect("loaded");
        assert_eq!((l.size, l.size_vram), (100, 60));
    }
}
