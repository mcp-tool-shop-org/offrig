//! Ollama over the tunnel: what is installed, what is loaded, what a model can do,
//! and a chat check shaped like Zed's requests (OpenAI-compatible, streamed, tools).

use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};

pub struct Ollama {
    agent: ureq::Agent,
    base: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub details: TagDetails,
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
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::Ollama(format!("{what}: the reply carried no message content")))
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
}
