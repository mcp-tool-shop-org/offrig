//! OpenRouter, for one approved job: Kimi-K3 piano arrangements for ai-jam-sessions
//! (the Director, 2026-10-08). OpenRouter stays off for everything else, so both the
//! models and the projects allowed are constants here, widened only by a pull request.
//!
//! What this module owns is how OpenRouter prices and bills: the priced worst case of a
//! call, the streamed chat completion with its real charge (`usage.cost`), and the
//! charge of a generation looked up by id after a failed stream. The books themselves
//! are kept by `store`. Design and evidence: docs/sidecar-design.md, "The OpenRouter lane".
//!
//! The key is `OPENROUTER_API_KEY`. It is sent only on the chat and generation calls
//! (the price list is public and gets no key), and never written anywhere.

use std::io::{BufRead, BufReader};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::trace;

/// Models offrig may call on OpenRouter.
pub const ALLOWED_MODELS: &[&str] = &["moonshotai/kimi-k3"];

/// Project lanes allowed to call OpenRouter: the approval covers ai-jam-sessions only.
pub const ALLOWED_LANES: &[&str] = &["ai-jam-sessions"];

pub const KEY_ENV: &str = "OPENROUTER_API_KEY";

const BASE: &str = "https://openrouter.ai/api/v1";

/// Tokens allowed on top of the byte bound for the chat template and role markers.
pub const TEMPLATE_ALLOWANCE_TOKENS: u64 = 512;

/// The reasoning efforts OpenRouter accepts.
pub const EFFORTS: &[&str] = &["minimal", "low", "medium", "high"];

/// A model's price, the dearest across every provider that serves it, in $ per token.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Price {
    pub prompt: f64,
    pub completion: f64,
    /// A flat per-request price, when a provider has one.
    pub request: f64,
    /// How many providers were priced.
    pub endpoints: usize,
}

impl Price {
    pub fn prompt_per_m(&self) -> f64 {
        per_m(self.prompt)
    }

    pub fn completion_per_m(&self) -> f64 {
        per_m(self.completion)
    }

    /// The most a call can be charged: every input token, plus `max_tokens` of output
    /// (reasoning and answer together: OpenRouter counts reasoning toward `max_tokens`
    /// and bills it at the output rate), plus any per-request price. Rounded up to the cent.
    pub fn worst_case(&self, input_tokens: u64, max_tokens: u64) -> f64 {
        let raw =
            input_tokens as f64 * self.prompt + max_tokens as f64 * self.completion + self.request;
        (raw * 100.0 - 1e-9).ceil().max(0.0) / 100.0
    }
}

fn per_m(per_token: f64) -> f64 {
    (per_token * 1e6 * 1e6).round() / 1e6
}

/// An upper bound on the input tokens of a call: a byte-level BPE token is at least one
/// byte of UTF-8, so the byte length over-counts, and the template allowance covers the
/// tokens the chat template adds.
pub fn input_bound(system: &str, user: &str) -> u64 {
    (system.len() + user.len()) as u64 + TEMPLATE_ALLOWANCE_TOKENS
}

pub fn model_allowed(model: &str) -> bool {
    ALLOWED_MODELS.contains(&model)
}

pub fn lane_allowed(tag: &str) -> bool {
    ALLOWED_LANES.contains(&tag)
}

/// One completion request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub user: String,
    pub max_tokens: u64,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    /// The price the worst case was computed from: no provider dearer than this may serve.
    pub price: Price,
}

impl Request {
    pub fn body(&self) -> Value {
        let mut messages = Vec::new();
        if !self.system.is_empty() {
            messages.push(json!({"role": "system", "content": self.system}));
        }
        messages.push(json!({"role": "user", "content": self.user}));
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "max_tokens": self.max_tokens,
            "usage": {"include": true},
            "provider": {"max_price": {
                "prompt": self.price.prompt_per_m(),
                "completion": self.price.completion_per_m(),
            }},
        });
        if let Some(e) = &self.reasoning_effort {
            body["reasoning"] = json!({"effort": e});
        }
        if let Some(t) = self.temperature {
            body["temperature"] = json!(t);
        }
        body
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// OpenRouter's real charge for the call, in dollars.
    pub cost: Option<f64>,
}

/// A finished completion.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    pub generation_id: Option<String>,
    pub provider: Option<String>,
    pub content: String,
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub usage: Usage,
}

/// A completion that did not finish. `generation_id` is set when a generation started,
/// which means OpenRouter may have charged for it.
#[derive(Debug)]
pub struct Failed {
    pub generation_id: Option<String>,
    pub provider: Option<String>,
    pub partial: String,
    pub error: Error,
}

/// The charge of one generation, looked up by id.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationCost {
    pub cost: f64,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub provider: Option<String>,
}

pub struct OpenRouter {
    agent: ureq::Agent,
    base: String,
    key: String,
}

impl OpenRouter {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var(KEY_ENV)
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or(Error::MissingOpenRouterKey)?;
        // Tests point the side-car at a mock. Debug builds only: a release binary can
        // never be redirected to send the key somewhere else.
        #[cfg(debug_assertions)]
        if let Ok(base) = std::env::var("OFFRIG_TEST_OPENROUTER_BASE")
            && !base.is_empty()
        {
            return Ok(Self::new(key, &base));
        }
        Ok(Self::new(key, BASE))
    }

    pub fn new(key: impl Into<String>, base: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A high-effort arrangement streams for minutes; this bounds a hung stream.
            .timeout_global(Some(Duration::from_secs(1800)))
            .build()
            .new_agent();
        Self {
            agent,
            base: base.trim_end_matches('/').to_string(),
            key: key.into(),
        }
    }

    fn redact(&self, text: &str) -> String {
        trace::redact_with(text, &[&self.key])
    }

    fn api_error(&self, what: &str, status: u16, body: &str) -> Error {
        let mut body = self.redact(body);
        body.truncate(600);
        Error::OpenRouter {
            what: what.into(),
            status,
            body,
        }
    }

    /// The dearest price across every provider of `model`, from the public endpoint list.
    /// No key is sent with this read.
    pub fn price(&self, model: &str) -> Result<Price> {
        let what = "the model's provider prices";
        let mut resp = self
            .agent
            .get(format!("{}/models/{model}/endpoints", self.base))
            .call()
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| Error::http(what, e))?;
        if status != 200 {
            return Err(self.api_error(what, status, &text));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::decode(what, e))?;
        parse_price(&v).ok_or_else(|| Error::OpenRouter {
            what: what.into(),
            status,
            body: format!("no priced provider listed for {model}"),
        })
    }

    /// Stream one completion. `on_generation` is called once, as soon as the stream names
    /// the generation id, so the caller can store it before anything else can go wrong.
    pub fn complete(
        &self,
        req: &Request,
        mut on_generation: impl FnMut(&str),
    ) -> std::result::Result<Completion, Box<Failed>> {
        let what = "the chat completion";
        let fail = |error: Error, done: &Completion| {
            Box::new(Failed {
                generation_id: done.generation_id.clone(),
                provider: done.provider.clone(),
                partial: done.content.clone(),
                error,
            })
        };
        let mut out = Completion::default();
        let resp = self
            .agent
            .post(format!("{}/chat/completions", self.base))
            .header("Authorization", &format!("Bearer {}", self.key))
            .send_json(req.body())
            .map_err(|e| fail(Error::http(what, e), &out))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let mut resp = resp;
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(fail(self.api_error(what, status, &text), &out));
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        let mut finished = false;
        for line in reader.lines() {
            let line = line.map_err(|e| fail(Error::io("reading the stream", e), &out))?;
            let line = line.trim();
            // Blank lines end events; `:` lines are keep-alive comments.
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                finished = true;
                break;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if out.generation_id.is_none()
                && let Some(id) = chunk["id"].as_str().filter(|s| !s.is_empty())
            {
                out.generation_id = Some(id.to_string());
                on_generation(id);
            }
            if out.provider.is_none()
                && let Some(p) = chunk["provider"].as_str()
            {
                out.provider = Some(p.to_string());
            }
            if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
                let code = err["code"].as_u64().unwrap_or(0) as u16;
                let msg = err["message"].as_str().unwrap_or("error mid-stream");
                return Err(fail(self.api_error(what, code, msg), &out));
            }
            let choice = &chunk["choices"][0];
            if let Some(t) = choice["delta"]["content"].as_str() {
                out.content.push_str(t);
            }
            if let Some(t) = choice["delta"]["reasoning"].as_str() {
                out.reasoning.push_str(t);
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                out.finish_reason = Some(f.to_string());
            }
            if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
                out.usage = Usage {
                    prompt_tokens: u["prompt_tokens"].as_u64(),
                    completion_tokens: u["completion_tokens"].as_u64(),
                    reasoning_tokens: u["completion_tokens_details"]["reasoning_tokens"].as_u64(),
                    cost: u["cost"].as_f64(),
                };
            }
        }
        if !finished && out.finish_reason.is_none() {
            return Err(fail(
                Error::OpenRouter {
                    what: what.into(),
                    status,
                    body: "the stream ended before the completion finished".into(),
                },
                &out,
            ));
        }
        Ok(out)
    }

    /// What OpenRouter charged for a generation. `Ok(None)` while it does not know yet
    /// (it answers 404 for a few seconds after a generation ends).
    pub fn generation_cost(&self, generation_id: &str) -> Result<Option<GenerationCost>> {
        let what = "the generation's charge";
        if generation_id.is_empty()
            || !generation_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::Refused(format!(
                "generation id {generation_id:?} is not an OpenRouter id"
            )));
        }
        let mut resp = self
            .agent
            .get(format!("{}/generation?id={generation_id}", self.base))
            .header("Authorization", &format!("Bearer {}", self.key))
            .call()
            .map_err(|e| Error::http(what, e))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| Error::http(what, e))?;
        if status == 404 {
            return Ok(None);
        }
        if status != 200 {
            return Err(self.api_error(what, status, &text));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::decode(what, e))?;
        let d = &v["data"];
        Ok(d["total_cost"].as_f64().map(|cost| GenerationCost {
            cost,
            tokens_in: d["tokens_prompt"].as_u64(),
            tokens_out: d["tokens_completion"].as_u64(),
            provider: d["provider_name"].as_str().map(str::to_string),
        }))
    }
}

/// The dearest prompt, completion (or internal reasoning) and request price across the
/// providers in an endpoints listing. `None` when no provider lists a price.
pub fn parse_price(v: &Value) -> Option<Price> {
    let endpoints = v["data"]["endpoints"].as_array()?;
    let num = |p: &Value, k: &str| -> f64 {
        match &p[k] {
            Value::String(s) => s.parse().unwrap_or(0.0),
            Value::Number(n) => n.as_f64().unwrap_or(0.0),
            _ => 0.0,
        }
    };
    let mut price = Price::default();
    for e in endpoints {
        let p = &e["pricing"];
        if !p.is_object() {
            continue;
        }
        price.endpoints += 1;
        price.prompt = price.prompt.max(num(p, "prompt"));
        price.completion = price
            .completion
            .max(num(p, "completion"))
            .max(num(p, "internal_reasoning"));
        price.request = price.request.max(num(p, "request"));
    }
    (price.endpoints > 0).then_some(price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kimi_k3_and_ai_jam_sessions_are_allowed() {
        assert!(model_allowed("moonshotai/kimi-k3"));
        assert!(!model_allowed("moonshotai/kimi-k2"));
        assert!(!model_allowed("anthropic/claude-opus"));
        assert!(!model_allowed("kimi-k3:cloud"));
        assert!(lane_allowed("ai-jam-sessions"));
        assert!(!lane_allowed("aspire-si"));
        assert!(!lane_allowed("ai-jam-sessions-1a2b"));
    }

    #[test]
    fn the_price_is_the_dearest_provider_and_reasoning_bills_as_output() {
        let v = json!({"data": {"endpoints": [
            {"pricing": {"prompt": "0.00000067", "completion": "0.000013"}},
            {"pricing": {"prompt": "0.00000072", "completion": "0.000015"}},
            {"pricing": {"prompt": "0.0000007", "completion": "0.000014", "internal_reasoning": "0.000016", "request": "0.001"}},
            {"name": "no pricing"}
        ]}});
        let p = parse_price(&v).expect("priced");
        assert_eq!(p.endpoints, 3);
        assert_eq!(p.prompt_per_m(), 0.72);
        assert_eq!(p.completion_per_m(), 16.0);
        assert_eq!(p.request, 0.001);
        assert!(parse_price(&json!({"data": {"endpoints": []}})).is_none());
        assert!(parse_price(&json!({})).is_none());
    }

    #[test]
    fn the_worst_case_bounds_input_and_output_and_rounds_up() {
        let p = Price {
            prompt: 0.72e-6,
            completion: 15e-6,
            request: 0.0,
            endpoints: 1,
        };
        // 1,700 tokens in and 200,000 out: $0.001224 + $3.00.
        assert_eq!(p.worst_case(1_700, 200_000), 3.01);
        assert_eq!(p.worst_case(0, 0), 0.0);
        // Amazing Grace's measured run: 1.7k in, 19.5k out, $0.30 actual.
        assert!(p.worst_case(1_700, 19_500) >= 0.30);
        assert_eq!(input_bound("ab", "cdé"), 6 + TEMPLATE_ALLOWANCE_TOKENS);
    }

    #[test]
    fn the_request_caps_price_and_tokens_and_asks_for_usage() {
        let r = Request {
            model: "moonshotai/kimi-k3".into(),
            system: "sys".into(),
            user: "brief".into(),
            max_tokens: 200_000,
            reasoning_effort: Some("high".into()),
            temperature: Some(0.0),
            price: Price {
                prompt: 0.72e-6,
                completion: 15e-6,
                request: 0.0,
                endpoints: 2,
            },
        };
        let b = r.body();
        assert_eq!(b["stream"], true);
        assert_eq!(b["max_tokens"], 200_000);
        assert_eq!(b["usage"]["include"], true);
        assert_eq!(b["reasoning"]["effort"], "high");
        assert_eq!(b["temperature"], 0.0);
        assert_eq!(b["provider"]["max_price"]["prompt"], 0.72);
        assert_eq!(b["provider"]["max_price"]["completion"], 15.0);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "brief");
        let bare = Request {
            system: String::new(),
            reasoning_effort: None,
            temperature: None,
            ..r
        };
        let b = bare.body();
        assert_eq!(b["messages"].as_array().map(Vec::len), Some(1));
        assert!(b.get("reasoning").is_none() && b.get("temperature").is_none());
    }

    #[test]
    fn a_generation_id_must_look_like_one() {
        let or = OpenRouter::new("k-123456", "http://127.0.0.1:9");
        assert!(matches!(
            or.generation_cost("gen-1&x=y"),
            Err(Error::Refused(_))
        ));
        assert!(matches!(or.generation_cost(""), Err(Error::Refused(_))));
    }
}
