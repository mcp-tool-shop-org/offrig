//! Edits Zed's settings.json (JSONC) through a concrete syntax tree, so comments and
//! layout survive. offrig owns exactly one key: `language_models.openai_compatible.<provider>`,
//! plus `agent.default_model` when asked to make a pod model the default.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::fsutil;

#[derive(Debug, Clone, PartialEq)]
pub struct ZedModel {
    pub name: String,
    pub display_name: String,
    pub max_tokens: u32,
    pub tools: bool,
    pub images: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DefaultModel {
    pub provider: String,
    pub model: String,
}

pub fn settings_path() -> Result<PathBuf> {
    #[cfg(windows)]
    let base = dirs::config_dir().map(|d| d.join("Zed"));
    #[cfg(not(windows))]
    let base = dirs::home_dir().map(|h| h.join(".config").join("zed"));
    base.map(|d| d.join("settings.json"))
        .ok_or_else(|| Error::Config("cannot locate Zed's settings directory".into()))
}

/// Zed reads an OpenAI-compatible provider's key from `<PROVIDER_ID>_API_KEY`.
pub fn api_key_env_name(provider: &str) -> String {
    let id: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{id}_API_KEY")
}

fn parse(text: &str, path: &Path) -> Result<CstRootNode> {
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| Error::Jsonc {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn s(v: &str) -> CstInputValue {
    CstInputValue::String(v.to_string())
}

fn obj(pairs: Vec<(&str, CstInputValue)>) -> CstInputValue {
    CstInputValue::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn provider_value(api_url: &str, models: &[ZedModel]) -> CstInputValue {
    let models = models
        .iter()
        .map(|m| {
            obj(vec![
                ("name", s(&m.name)),
                ("display_name", s(&m.display_name)),
                (
                    "max_tokens",
                    CstInputValue::Number(m.max_tokens.to_string()),
                ),
                (
                    "capabilities",
                    obj(vec![
                        ("tools", CstInputValue::Bool(m.tools)),
                        ("images", CstInputValue::Bool(m.images)),
                        ("parallel_tool_calls", CstInputValue::Bool(false)),
                        ("prompt_cache_key", CstInputValue::Bool(false)),
                    ]),
                ),
            ])
        })
        .collect();
    obj(vec![
        ("api_url", s(api_url)),
        ("available_models", CstInputValue::Array(models)),
    ])
}

fn set_prop(o: &CstObject, key: &str, value: CstInputValue) {
    match o.get(key) {
        Some(p) => p.set_value(value),
        None => {
            o.append(key, value);
        }
    }
}

/// Insert or replace offrig's provider. Everything else in the file is kept as is.
pub fn apply_provider(
    text: &str,
    path: &Path,
    provider: &str,
    api_url: &str,
    models: &[ZedModel],
) -> Result<String> {
    let root = parse(text, path)?;
    let top = root.object_value_or_set();
    let lm = top.object_value_or_set("language_models");
    let oc = lm.object_value_or_set("openai_compatible");
    set_prop(&oc, provider, provider_value(api_url, models));
    let out = root.to_string();
    // Andon: the result must parse and read back what was written.
    match read_provider(&out, path, provider)? {
        Some(v) if v["api_url"] == api_url => Ok(out),
        _ => Err(Error::Jsonc {
            path: path.to_path_buf(),
            message: "provider did not read back after the edit".into(),
        }),
    }
}

pub fn remove_provider(text: &str, path: &Path, provider: &str) -> Result<String> {
    let root = parse(text, path)?;
    if let Some(p) = root
        .object_value()
        .and_then(|t| t.object_value("language_models"))
        .and_then(|lm| lm.object_value("openai_compatible"))
        .and_then(|oc| oc.get(provider))
    {
        p.remove();
    }
    Ok(root.to_string())
}

pub fn read_provider(text: &str, path: &Path, provider: &str) -> Result<Option<Value>> {
    let root = parse(text, path)?;
    Ok(root
        .object_value()
        .and_then(|t| t.object_value("language_models"))
        .and_then(|lm| lm.object_value("openai_compatible"))
        .and_then(|oc| oc.get(provider))
        .and_then(|p| p.to_serde_value()))
}

pub fn default_model(text: &str, path: &Path) -> Result<Option<DefaultModel>> {
    let root = parse(text, path)?;
    let v = root
        .object_value()
        .and_then(|t| t.object_value("agent"))
        .and_then(|a| a.object_value("default_model"))
        .and_then(|d| d.to_serde_value());
    Ok(v.and_then(|v| {
        Some(DefaultModel {
            provider: v["provider"].as_str()?.to_string(),
            model: v["model"].as_str()?.to_string(),
        })
    }))
}

/// Point Zed's agent at a model. Other keys in `default_model` (e.g. `enable_thinking`)
/// are kept.
pub fn set_default_model(text: &str, path: &Path, m: &DefaultModel) -> Result<String> {
    let root = parse(text, path)?;
    let top = root.object_value_or_set();
    let dm = top
        .object_value_or_set("agent")
        .object_value_or_set("default_model");
    set_prop(&dm, "provider", s(&m.provider));
    set_prop(&dm, "model", s(&m.model));
    Ok(root.to_string())
}

/// Model names in Zed's local Ollama provider list. A pod model must not share one,
/// or picking it there would run it on this machine.
pub fn local_ollama_models(text: &str, path: &Path) -> Result<Vec<String>> {
    let root = parse(text, path)?;
    let v = root
        .object_value()
        .and_then(|t| t.object_value("language_models"))
        .and_then(|lm| lm.object_value("ollama"))
        .and_then(|o| o.array_value("available_models"))
        .and_then(|a| a.to_serde_value());
    Ok(v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect())
}

pub fn read_settings(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("{}\n".into()),
        Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
    }
}

/// Back up once, then write atomically. Zed hot-reloads settings.json.
pub fn write_settings(path: &Path, text: &str) -> Result<()> {
    fsutil::backup_once(path)?;
    fsutil::write_atomic(path, text.as_bytes())
}

pub fn api_key_env_present(provider: &str) -> bool {
    let name = api_key_env_name(provider);
    if std::env::var_os(&name).is_some() {
        return true;
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new("reg");
        cmd.args(["query", r"HKCU\Environment", "/v", &name]);
        if let Ok(out) =
            crate::proc::run_with_timeout(&mut cmd, Duration::from_secs(10), "reg query")
        {
            return out.success();
        }
    }
    false
}

/// Zed will not call an OpenAI-compatible provider without a key. The pod's Ollama
/// needs none (it is reachable only through the tunnel), so the value is a fixed
/// placeholder. Zed must be restarted to see a new user environment variable.
pub fn set_api_key_env(provider: &str) -> Result<()> {
    let name = api_key_env_name(provider);
    #[cfg(windows)]
    {
        let mut cmd = Command::new("setx");
        cmd.args([name.as_str(), "offrig-tunnel"]);
        let out = crate::proc::run_with_timeout(&mut cmd, Duration::from_secs(20), "setx")?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Config(format!(
                "setx {name} failed: {}",
                out.stderr.trim()
            )))
        }
    }
    #[cfg(not(windows))]
    {
        Err(Error::Config(format!(
            "set {name}=offrig-tunnel in your shell profile, or enter any key in Zed's provider settings"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZED: &str = r#"// Zed settings
//
// my notes about fonts
{
  "ui_font_size": 23.0, // keep this
  "language_models": {
    "ollama": {
      "api_url": "http://127.0.0.1:11434",
      "available_models": [
        { "name": "qwen3:14b", "max_tokens": 175000 }, // local only
      ]
    }
  },
  "agent": {
    "default_model": {
      "enable_thinking": true,
      "provider": "ollama",
      "model": "qwen3:14b"
    }
  }
}
"#;

    fn p() -> PathBuf {
        PathBuf::from("settings.json")
    }

    fn models() -> Vec<ZedModel> {
        vec![ZedModel {
            name: "qwen3-coder:30b-a3b-q8_0".into(),
            display_name: "RunPod · qwen3-coder:30b-a3b-q8_0".into(),
            max_tokens: 65_536,
            tools: true,
            images: false,
        }]
    }

    #[test]
    fn apply_keeps_comments_and_other_settings() {
        let out = apply_provider(ZED, &p(), "offrig", "http://127.0.0.1:11435/v1", &models())
            .expect("apply");
        for kept in [
            "// Zed settings",
            "// my notes about fonts",
            "// keep this",
            "// local only",
            "\"ui_font_size\": 23.0",
        ] {
            assert!(out.contains(kept), "lost {kept:?}:\n{out}");
        }
        let v = read_provider(&out, &p(), "offrig")
            .expect("parse")
            .expect("provider present");
        assert_eq!(v["api_url"], "http://127.0.0.1:11435/v1");
        assert_eq!(v["available_models"][0]["max_tokens"], 65_536);
        assert_eq!(v["available_models"][0]["capabilities"]["tools"], true);
        assert_eq!(
            local_ollama_models(&out, &p()).expect("parse"),
            ["qwen3:14b"]
        );
    }

    #[test]
    fn apply_is_idempotent_and_replaces_the_whole_provider() {
        let once = apply_provider(ZED, &p(), "offrig", "http://127.0.0.1:11435/v1", &models())
            .expect("apply");
        let twice = apply_provider(
            &once,
            &p(),
            "offrig",
            "http://127.0.0.1:11435/v1",
            &models(),
        )
        .expect("apply");
        assert_eq!(once, twice);
        let other =
            apply_provider(&once, &p(), "offrig", "http://127.0.0.1:11499/v1", &[]).expect("apply");
        let v = read_provider(&other, &p(), "offrig")
            .expect("parse")
            .expect("present");
        assert_eq!(v["api_url"], "http://127.0.0.1:11499/v1");
        assert_eq!(v["available_models"].as_array().map(Vec::len), Some(0));
        assert_eq!(other.matches("\"offrig\"").count(), 1);
    }

    #[test]
    fn remove_restores_the_original_text() {
        let with = apply_provider(ZED, &p(), "offrig", "http://127.0.0.1:11435/v1", &models())
            .expect("apply");
        let without = remove_provider(&with, &p(), "offrig").expect("remove");
        assert!(
            read_provider(&without, &p(), "offrig")
                .expect("parse")
                .is_none()
        );
        assert!(without.contains("// keep this") && without.contains("// local only"));
    }

    #[test]
    fn default_model_swap_keeps_other_keys_and_reports_previous() {
        let before = default_model(ZED, &p())
            .expect("parse")
            .expect("has default");
        assert_eq!(
            before,
            DefaultModel {
                provider: "ollama".into(),
                model: "qwen3:14b".into()
            }
        );
        let dm = DefaultModel {
            provider: "offrig".into(),
            model: "qwen3-coder:30b-a3b-q8_0".into(),
        };
        let out = set_default_model(ZED, &p(), &dm).expect("set");
        assert_eq!(default_model(&out, &p()).expect("parse"), Some(dm));
        assert!(out.contains("\"enable_thinking\": true"));
        let back = set_default_model(&out, &p(), &before).expect("restore");
        assert_eq!(default_model(&back, &p()).expect("parse"), Some(before));
    }

    #[test]
    fn empty_and_missing_settings_get_a_provider() {
        for start in ["{}\n", "", "// only a comment\n"] {
            let out = apply_provider(
                start,
                &p(),
                "offrig",
                "http://127.0.0.1:11435/v1",
                &models(),
            )
            .unwrap_or_else(|e| panic!("start {start:?}: {e}"));
            assert!(
                read_provider(&out, &p(), "offrig")
                    .expect("parse")
                    .is_some()
            );
        }
    }

    #[test]
    fn broken_jsonc_is_an_error_not_a_rewrite() {
        let err = apply_provider("{ \"a\": ", &p(), "offrig", "x", &[]).expect_err("must refuse");
        assert!(matches!(err, Error::Jsonc { .. }));
    }

    #[test]
    fn env_name_follows_zed_rule() {
        assert_eq!(api_key_env_name("offrig"), "OFFRIG_API_KEY");
        assert_eq!(api_key_env_name("my-provider"), "MY_PROVIDER_API_KEY");
    }
}
