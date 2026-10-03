//! The "never on my GPU" checks. Each is a fact offrig can observe, not a promise.
//! `evaluate` is pure so every rule is unit tested; `gather` collects the facts.

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{Config, LOCAL_OLLAMA_PORT};
use crate::error::Result;
use crate::ollama::Ollama;
use crate::runpod::Pod;
use crate::{remote, zed};

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// What the checks look at. `None` means the fact could not be read.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub tunnel_port: u16,
    pub expected_api_url: String,
    pub zed_api_url: Option<String>,
    pub zed_models: Vec<String>,
    pub zed_local_ollama_models: Vec<String>,
    pub pod_ports: Vec<String>,
    /// Model names read through the tunnel.
    pub tunnel_models: Option<Vec<String>>,
    /// Model names read on the pod over SSH.
    pub pod_models: Option<Vec<String>>,
    /// Model names in the local Ollama; `None` when it is not running.
    pub local_models: Option<Vec<String>>,
}

fn set(v: &[String]) -> BTreeSet<&str> {
    v.iter().map(String::as_str).collect()
}

fn list(v: &BTreeSet<&str>) -> String {
    v.iter().copied().collect::<Vec<_>>().join(", ")
}

pub fn evaluate(f: &Facts) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(Check {
        name: "Tunnel avoids the local Ollama port",
        ok: f.tunnel_port != LOCAL_OLLAMA_PORT,
        detail: format!(
            "tunnel on 127.0.0.1:{}, local Ollama on {LOCAL_OLLAMA_PORT}",
            f.tunnel_port
        ),
    });

    out.push(match &f.zed_api_url {
        Some(u) if *u == f.expected_api_url => Check {
            name: "Zed sends pod models through the tunnel",
            ok: true,
            detail: u.clone(),
        },
        Some(u) => Check {
            name: "Zed sends pod models through the tunnel",
            ok: false,
            detail: format!(
                "Zed's provider points at {u}, expected {}",
                f.expected_api_url
            ),
        },
        None => Check {
            name: "Zed sends pod models through the tunnel",
            ok: false,
            detail: "offrig's provider is not in Zed's settings".into(),
        },
    });

    let exposed: Vec<&String> = f
        .pod_ports
        .iter()
        .filter(|p| p.starts_with("11434"))
        .collect();
    out.push(Check {
        name: "Pod's Ollama is not exposed to the internet",
        ok: exposed.is_empty(),
        detail: if exposed.is_empty() {
            format!("pod exposes only {}", f.pod_ports.join(", "))
        } else {
            format!("pod exposes {exposed:?}")
        },
    });

    out.push(match (&f.tunnel_models, &f.pod_models) {
        (Some(t), Some(p)) if set(t) == set(p) => Check {
            name: "The tunnel ends at the pod",
            ok: true,
            detail: format!(
                "same {} model(s) through the tunnel and on the pod",
                t.len()
            ),
        },
        (Some(t), Some(p)) => Check {
            name: "The tunnel ends at the pod",
            ok: false,
            detail: format!(
                "tunnel lists [{}], pod lists [{}]",
                list(&set(t)),
                list(&set(p))
            ),
        },
        _ => Check {
            name: "The tunnel ends at the pod",
            ok: false,
            detail: "could not read both model lists".into(),
        },
    });

    let pod = f.pod_models.as_deref().map(set).unwrap_or_default();
    out.push(match &f.local_models {
        None => Check {
            name: "Pod models are not on this machine",
            ok: true,
            detail: "local Ollama is not running".into(),
        },
        Some(local) => {
            let both: BTreeSet<&str> = set(local).intersection(&pod).copied().collect();
            Check {
                name: "Pod models are not on this machine",
                ok: both.is_empty(),
                detail: if both.is_empty() {
                    format!(
                        "none of the pod's models are in the local Ollama ({} local)",
                        local.len()
                    )
                } else {
                    format!("also in the local Ollama: {}", list(&both))
                },
            }
        }
    });

    let clash: BTreeSet<&str> = set(&f.zed_models)
        .intersection(&set(&f.zed_local_ollama_models))
        .copied()
        .collect();
    out.push(Check {
        name: "No pod model shares a name with a local Zed model",
        ok: clash.is_empty(),
        detail: if clash.is_empty() {
            "names are distinct".into()
        } else {
            format!("in both lists: {}", list(&clash))
        },
    });

    let missing: BTreeSet<&str> = set(&f.zed_models).difference(&pod).copied().collect();
    out.push(Check {
        name: "Every model Zed offers is on the pod",
        ok: missing.is_empty() && !f.zed_models.is_empty(),
        detail: if f.zed_models.is_empty() {
            "Zed's provider lists no models".into()
        } else if missing.is_empty() {
            format!("{} model(s) offered, all present", f.zed_models.len())
        } else {
            format!("not on the pod: {}", list(&missing))
        },
    });

    out
}

/// Model ids from an OpenAI `/v1/models` body (Ollama and SGLang both serve it).
fn tag_names(json: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v["data"].as_array()?;
    Some(crate::ollama::model_ids(&v))
}

pub fn gather(cfg: &Config, pod: &Pod, zed_settings: &Path) -> Result<Facts> {
    let text = zed::read_settings(zed_settings)?;
    let provider = zed::read_provider(&text, zed_settings, &cfg.zed_provider)?;
    let zed_models = provider
        .as_ref()
        .and_then(|p| p["available_models"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect();
    let tunnel = Ollama::new(&cfg.tunnel_base_url());
    let local = Ollama::new(&format!("http://127.0.0.1:{LOCAL_OLLAMA_PORT}"));
    Ok(Facts {
        tunnel_port: cfg.tunnel_port,
        expected_api_url: cfg.zed_api_url(),
        zed_api_url: provider
            .as_ref()
            .and_then(|p| p["api_url"].as_str().map(str::to_string)),
        zed_models,
        zed_local_ollama_models: zed::local_ollama_models(&text, zed_settings)?,
        pod_ports: pod.ports.clone(),
        tunnel_models: tunnel.openai_models().ok(),
        pod_models: remote::pod_models_json(&cfg.ssh_alias)
            .ok()
            .as_deref()
            .and_then(tag_names),
        local_models: local
            .tags()
            .ok()
            .map(|t| t.into_iter().map(|m| m.name).collect()),
    })
}

pub fn all_ok(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    fn good() -> Facts {
        Facts {
            tunnel_port: 11435,
            expected_api_url: "http://127.0.0.1:11435/v1".into(),
            zed_api_url: Some("http://127.0.0.1:11435/v1".into()),
            zed_models: v(&["qwen3-coder:30b-a3b-q8_0"]),
            zed_local_ollama_models: v(&["qwen3:14b", "gemma4:31b"]),
            pod_ports: v(&["22/tcp"]),
            tunnel_models: Some(v(&["qwen3-coder:30b-a3b-q8_0", "gpt-oss:120b"])),
            pod_models: Some(v(&["gpt-oss:120b", "qwen3-coder:30b-a3b-q8_0"])),
            local_models: Some(v(&["qwen3:14b", "gemma4:31b"])),
        }
    }

    fn failing(f: &Facts) -> Vec<&'static str> {
        evaluate(f)
            .into_iter()
            .filter(|c| !c.ok)
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn healthy_setup_passes_every_check() {
        let checks = evaluate(&good());
        assert!(all_ok(&checks), "{checks:#?}");
        assert_eq!(checks.len(), 7);
    }

    #[test]
    fn each_failure_is_caught_by_its_own_check() {
        let cases: Vec<(Facts, &str)> = vec![
            (
                Facts {
                    tunnel_port: 11434,
                    ..good()
                },
                "Tunnel avoids the local Ollama port",
            ),
            (
                Facts {
                    zed_api_url: Some("http://127.0.0.1:11434/v1".into()),
                    ..good()
                },
                "Zed sends pod models through the tunnel",
            ),
            (
                Facts {
                    zed_api_url: None,
                    ..good()
                },
                "Zed sends pod models through the tunnel",
            ),
            (
                Facts {
                    pod_ports: v(&["22/tcp", "11434/http"]),
                    ..good()
                },
                "Pod's Ollama is not exposed to the internet",
            ),
            (
                Facts {
                    tunnel_models: Some(v(&["qwen3:14b"])),
                    ..good()
                },
                "The tunnel ends at the pod",
            ),
            (
                Facts {
                    tunnel_models: None,
                    ..good()
                },
                "The tunnel ends at the pod",
            ),
            (
                Facts {
                    local_models: Some(v(&["qwen3-coder:30b-a3b-q8_0"])),
                    ..good()
                },
                "Pod models are not on this machine",
            ),
            (
                Facts {
                    zed_local_ollama_models: v(&["qwen3-coder:30b-a3b-q8_0"]),
                    ..good()
                },
                "No pod model shares a name with a local Zed model",
            ),
            (
                Facts {
                    zed_models: v(&["missing:1b"]),
                    ..good()
                },
                "Every model Zed offers is on the pod",
            ),
            (
                Facts {
                    zed_models: vec![],
                    ..good()
                },
                "Every model Zed offers is on the pod",
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(
                failing(&facts),
                [expected],
                "expected only {expected:?} to fail"
            );
        }
    }

    #[test]
    fn local_ollama_off_is_safe() {
        let f = Facts {
            local_models: None,
            ..good()
        };
        assert!(all_ok(&evaluate(&f)));
    }

    #[test]
    fn tag_names_reads_the_openai_model_list() {
        assert_eq!(
            tag_names(r#"{"object":"list","data":[{"id":"a:1"},{"id":"b:2"}]}"#),
            Some(v(&["a:1", "b:2"]))
        );
        assert_eq!(tag_names("not json"), None);
        assert_eq!(
            tag_names("{\"detail\":\"Not Found\"}"),
            None,
            "an error body is not an empty list"
        );
    }
}
