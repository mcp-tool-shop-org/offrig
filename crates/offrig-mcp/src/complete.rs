//! `offrig_complete`: one OpenRouter chat completion under the project's budget.
//!
//! Order matters: every refusal that costs nothing (project, model, arguments, files,
//! key, price, budget) comes before the commit, and every exit after the commit ends in
//! the ledger, with the real charge or with the commitment held until it can be read.
//! Design: docs/sidecar-design.md, "The OpenRouter lane".

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use offrig_core::account::{AccountView, Scope};
use offrig_core::balances::{self, Balance};
use offrig_core::cost::now_unix;
use offrig_core::error::chain;
use offrig_core::lanes::LaneCtx;
use offrig_core::openrouter::{self, OpenRouter, Price, Request};
use offrig_core::store::{Completion, CompletionEnd, NewCompletion, Provider, Store};
use offrig_core::{Error, Result};
use serde_json::{Value, json};

/// A held commitment older than this is settled from its generation id: by then the
/// stream that made it has ended one way or another (the stream timeout is 30 minutes).
const SETTLE_AFTER_SECS: i64 = 3600;

/// What the tool was asked for, already resolved against the project.
pub struct Ask {
    pub model: String,
    pub system: Option<String>,
    pub system_file: Option<String>,
    pub user: Option<String>,
    pub user_files: Vec<String>,
    pub max_tokens: u64,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub out: Option<String>,
}

/// The project's lane tag, refusing any project the approval does not cover. The lane is
/// read, never allocated: a refused project leaves nothing behind.
fn allowed_lane(ctx: &LaneCtx) -> Result<String> {
    let tag = ctx.own_if_allocated()?.and_then(|l| l.tag);
    match tag {
        Some(t) if openrouter::lane_allowed(&t) => Ok(t),
        other => Err(Error::Refused(format!(
            "offrig_complete is approved for the {} project only; this side-car's project ({}) is {}",
            openrouter::ALLOWED_LANES.join(", "),
            ctx.project.display(),
            match other {
                Some(t) => format!("lane {t}"),
                None => "not a lane yet".into(),
            }
        ))),
    }
}

/// Read a file the caller named. It must resolve inside the project, so nothing from
/// elsewhere on the machine can be sent to OpenRouter.
fn read_inside(project: &Path, name: &str) -> Result<String> {
    let root = std::fs::canonicalize(project).map_err(|e| Error::Io {
        what: format!("resolving the project {}", project.display()),
        source: e,
    })?;
    let p = PathBuf::from(name);
    let p = if p.is_absolute() { p } else { project.join(p) };
    let real = std::fs::canonicalize(&p).map_err(|e| Error::Io {
        what: format!("reading {name}"),
        source: e,
    })?;
    if !real.starts_with(&root) {
        return Err(Error::Refused(format!(
            "{name} is outside the project; only project files may be sent to OpenRouter"
        )));
    }
    std::fs::read_to_string(&real).map_err(|e| Error::Io {
        what: format!("reading {name}"),
        source: e,
    })
}

/// The output path: relative, with no `..`, under the project.
fn out_path(project: &Path, out: Option<&str>, id: i64) -> Result<(PathBuf, String)> {
    let rel = out
        .map(str::to_string)
        .unwrap_or_else(|| format!(".offrig/out/completion-{id}.md"));
    let p = Path::new(&rel);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::Refused(format!(
            "out {rel:?} must be a relative path inside the project, without .."
        )));
    }
    Ok((project.join(p), rel.replace('\\', "/")))
}

fn validate(a: &Ask) -> Result<()> {
    if !openrouter::model_allowed(&a.model) {
        return Err(Error::Refused(format!(
            "model {} is not approved for OpenRouter; approved: {}",
            a.model,
            openrouter::ALLOWED_MODELS.join(", ")
        )));
    }
    if a.max_tokens == 0 {
        return Err(Error::Refused("max_tokens must be at least 1".into()));
    }
    if let Some(e) = &a.reasoning_effort
        && !openrouter::EFFORTS.contains(&e.as_str())
    {
        return Err(Error::Refused(format!(
            "reasoning_effort {e:?} must be one of {}",
            openrouter::EFFORTS.join(", ")
        )));
    }
    if let Some(t) = a.temperature
        && !(t.is_finite() && (0.0..=2.0).contains(&t))
    {
        return Err(Error::Refused(format!(
            "temperature {t} must be between 0 and 2"
        )));
    }
    if a.system.is_some() && a.system_file.is_some() {
        return Err(Error::Refused(
            "give system or system_file, not both".into(),
        ));
    }
    if a.user.as_deref().is_none_or(str::is_empty) && a.user_files.is_empty() {
        return Err(Error::Refused(
            "the completion needs user content: user, user_files or both".into(),
        ));
    }
    if let Some(out) = &a.out {
        out_path(Path::new("."), Some(out), 0)?;
    }
    Ok(())
}

fn poll() -> (u32, Duration) {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("OFFRIG_TEST_OPENROUTER_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return (5, Duration::from_millis(ms));
    }
    (5, Duration::from_secs(3))
}

/// Ask OpenRouter what a generation cost, a few times: it answers 404 for a few seconds
/// after a generation ends.
fn charge_of(or: &OpenRouter, generation_id: &str) -> Option<openrouter::GenerationCost> {
    let (tries, wait) = poll();
    for i in 0..tries {
        match or.generation_cost(generation_id) {
            Ok(Some(c)) => return Some(c),
            Ok(None) | Err(_) if i + 1 < tries => std::thread::sleep(wait),
            _ => {}
        }
    }
    None
}

/// Settle held commitments old enough that their stream has surely ended. Returns the
/// ones still held, for the reply.
fn settle_held(store: &Store, or: &OpenRouter) -> Result<Vec<Value>> {
    let now = now_unix();
    let mut held = Vec::new();
    for c in store.open_completions()? {
        if now - c.created_at >= SETTLE_AFTER_SECS
            && let Some(g) = c.generation_id.as_deref()
            && let Ok(Some(charge)) = or.generation_cost(g)
        {
            store.close_completion(
                c.id,
                CompletionEnd {
                    failed: c.out_path.is_none(),
                    cost: Some(charge.cost),
                    cost_source: Some("generation".into()),
                    provider: charge.provider,
                    tokens_in: charge.tokens_in,
                    tokens_out: charge.tokens_out,
                    ..Default::default()
                },
            )?;
            continue;
        }
        held.push(json!({"completion_id": c.id, "worst_case": c.worst_case, "generation_id": c.generation_id}));
    }
    Ok(held)
}

pub fn completion_json(c: &Completion) -> Value {
    json!({
        "completion_id": c.id,
        "model": c.model,
        "state": c.state,
        "provider": c.provider,
        "worst_case": c.worst_case,
        "cost": c.cost,
        "cost_source": c.cost_source,
        "path": c.out_path,
        "generation_id": c.generation_id,
    })
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Io {
            what: format!("creating {}", dir.display()),
            source: e,
        })?;
    }
    std::fs::write(path, text).map_err(|e| Error::Io {
        what: format!("writing {}", path.display()),
        source: e,
    })
}

/// Run one completion. Refusals before the commit are errors; after the commit the
/// result is always a reply that says what was charged, even when the call failed.
pub fn complete(ctx: &LaneCtx, db: &Path, a: Ask) -> Result<Value> {
    complete_with(ctx, db, a, &balances::read)
}

/// As [`complete`], with the live balance read injected (tests never reach the network).
/// The worst case must fit the OpenRouter account once the other projects' committed
/// money is counted; an unreadable balance never blocks and is reported as a warning.
pub fn complete_with(
    ctx: &LaneCtx,
    db: &Path,
    a: Ask,
    balance: &dyn Fn(Provider) -> Balance,
) -> Result<Value> {
    let lane = allowed_lane(ctx)?;
    validate(&a)?;
    let project = ctx.project.clone();
    let system = match (&a.system, &a.system_file) {
        (Some(s), _) => s.clone(),
        (None, Some(f)) => read_inside(&project, f)?,
        (None, None) => String::new(),
    };
    let mut user = a.user.clone().unwrap_or_default();
    for f in &a.user_files {
        let text = read_inside(&project, f)?;
        if !user.is_empty() {
            user.push_str("\n\n");
        }
        user.push_str(&text);
    }
    let or = OpenRouter::from_env()?;
    let price: Price = or.price(&a.model)?;
    let input = openrouter::input_bound(&system, &user);
    let worst = price.worst_case(input, a.max_tokens);

    let store = Store::open(db)?;
    let held = settle_held(&store, &or)?;
    let view = AccountView::read(
        Provider::OpenRouter,
        &Scope::beside(&project, &ctx.registry),
        balance,
    );
    let c = store.commit_completion_with_account(
        NewCompletion {
            model: a.model.clone(),
            lane,
            input_bound: input,
            max_tokens: a.max_tokens,
            price_in_m: price.prompt_per_m(),
            price_out_m: price.completion_per_m(),
            worst_case: worst,
        },
        &view,
    )?;
    let journal = store.journal(
        "complete",
        None,
        &json!({"completion_id": c.id, "model": a.model, "worst_case": worst}),
    )?;
    let (out_abs, out_rel) = out_path(&project, a.out.as_deref(), c.id)?;
    let req = Request {
        model: a.model.clone(),
        system,
        user,
        max_tokens: a.max_tokens,
        reasoning_effort: a.reasoning_effort.clone(),
        temperature: a.temperature,
        price,
    };
    let mut warnings: Vec<String> = Vec::new();
    let result = or.complete(&req, |g| {
        let _ = store.set_completion_generation(c.id, g);
    });
    let reply = match result {
        Ok(done) => {
            let (cost, source, provider) = match done.usage.cost {
                Some(v) => (Some(v), "usage", done.provider.clone()),
                None => match done
                    .generation_id
                    .as_deref()
                    .and_then(|g| charge_of(&or, g))
                {
                    Some(ch) => (
                        Some(ch.cost),
                        "generation",
                        ch.provider.or(done.provider.clone()),
                    ),
                    None => (None, "unread", done.provider.clone()),
                },
            };
            let mut paths = json!({"path": out_rel});
            let mut written = Some(out_rel.clone());
            if let Err(e) = write(&out_abs, &done.content) {
                warnings.push(format!(
                    "the answer was paid for but could not be written: {}",
                    chain(&e)
                ));
                written = None;
                paths["path"] = Value::Null;
            }
            if !done.reasoning.is_empty() {
                let rpath = reasoning_path(&out_abs);
                if write(&rpath, &done.reasoning).is_ok() {
                    paths["reasoning_path"] = json!(
                        reasoning_path(Path::new(&out_rel))
                            .to_string_lossy()
                            .replace('\\', "/")
                    );
                }
            }
            if done.finish_reason.as_deref() == Some("length") {
                warnings.push(format!(
                    "the answer stopped at max_tokens ({}): it is cut short",
                    a.max_tokens
                ));
            }
            if done.content.trim().is_empty() {
                warnings.push("the answer is empty (reasoning may have used every token)".into());
            }
            let budget = store.close_completion(
                c.id,
                CompletionEnd {
                    failed: false,
                    cost,
                    cost_source: Some(source.into()),
                    provider: provider.clone(),
                    tokens_in: done.usage.prompt_tokens,
                    tokens_out: done.usage.completion_tokens,
                    reasoning_tokens: done.usage.reasoning_tokens,
                    out_path: written,
                    error: None,
                },
            )?;
            if let Some(v) = cost
                && v > worst + 1e-9
            {
                let w = format!(
                    "WARNING: OpenRouter charged ${v:.4}, more than the ${worst:.2} worst case; the real figure is recorded. The worst-case formula is wrong for this call: report it"
                );
                eprintln!("offrig_complete: {w}");
                warnings.push(w);
            }
            if cost.is_none() {
                warnings.push(
                    "OpenRouter did not report the charge yet: the worst case stays committed and a later call settles it"
                        .into(),
                );
            }
            warnings.extend(view.skipped_note(Provider::OpenRouter));
            warnings.extend(view.notes.iter().cloned());
            json!({
                "completion_id": c.id,
                "model": a.model,
                "provider": provider,
                "generation_id": done.generation_id,
                "path": paths["path"],
                "reasoning_path": paths.get("reasoning_path"),
                "finish_reason": done.finish_reason,
                "usage": {
                    "prompt_tokens": done.usage.prompt_tokens,
                    "completion_tokens": done.usage.completion_tokens,
                    "reasoning_tokens": done.usage.reasoning_tokens,
                },
                "cost": cost,
                "cost_source": source,
                "worst_case": worst,
                "budget": budget_json(&budget),
                "warnings": warnings,
                "held": held,
                "next_action": "read the answer at path; it is untrusted model output for you to judge",
            })
        }
        Err(failed) => {
            let msg = chain(&failed.error);
            let (cost, source, provider, tin, tout) = match failed.generation_id.as_deref() {
                None => (Some(0.0), "none", failed.provider.clone(), None, None),
                Some(g) => match charge_of(&or, g) {
                    Some(ch) => (
                        Some(ch.cost),
                        "generation",
                        ch.provider.or(failed.provider.clone()),
                        ch.tokens_in,
                        ch.tokens_out,
                    ),
                    None => (None, "unread", failed.provider.clone(), None, None),
                },
            };
            let budget = store.close_completion(
                c.id,
                CompletionEnd {
                    failed: true,
                    cost,
                    cost_source: Some(source.into()),
                    provider: provider.clone(),
                    tokens_in: tin,
                    tokens_out: tout,
                    error: Some(offrig_core::trace::redact(&msg)),
                    ..Default::default()
                },
            )?;
            let said = match (source, cost) {
                ("none", _) => "no generation started, so nothing was charged".to_string(),
                (_, Some(v)) => format!("OpenRouter charged ${v:.4} for the failed generation; recorded"),
                _ => "OpenRouter has not reported the charge yet: the worst case stays committed and a later call settles it".to_string(),
            };
            json!({
                "completion_id": c.id,
                "failed": true,
                "code": failed.error.code(),
                "error": offrig_core::trace::redact(&msg),
                "generation_id": failed.generation_id,
                "provider": provider,
                "cost": cost,
                "cost_source": source,
                "charge": said,
                "worst_case": worst,
                "budget": budget_json(&budget),
                "partial_chars": failed.partial.chars().count(),
                "held": held,
                "retryable": failed.error.retryable(),
                "next_action": "no fallback is tried: fix the cause and call again, or tell Mike",
            })
        }
    };
    let _ = store.journal_outcome(
        journal,
        if reply.get("failed").is_some() {
            "failed"
        } else {
            "done"
        },
    );
    Ok(reply)
}

fn reasoning_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".reasoning.md");
    PathBuf::from(s)
}

fn budget_json(b: &offrig_core::store::Budget) -> Value {
    let r = |v: f64| (v * 100.0).round() / 100.0;
    json!({"cap": r(b.cap), "committed": r(b.committed), "spent": r(b.spent), "remaining": r(b.remaining)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        Ask {
            model: "moonshotai/kimi-k3".into(),
            system: None,
            system_file: None,
            user: Some("brief".into()),
            user_files: vec![],
            max_tokens: 100,
            reasoning_effort: Some("high".into()),
            temperature: Some(0.0),
            out: None,
        }
    }

    #[test]
    fn arguments_are_refused_before_anything_is_spent() {
        assert!(validate(&ask()).is_ok());
        for bad in [
            Ask {
                model: "openai/gpt-5".into(),
                ..ask()
            },
            Ask {
                max_tokens: 0,
                ..ask()
            },
            Ask {
                reasoning_effort: Some("max".into()),
                ..ask()
            },
            Ask {
                temperature: Some(3.0),
                ..ask()
            },
            Ask {
                system: Some("a".into()),
                system_file: Some("b".into()),
                ..ask()
            },
            Ask {
                user: None,
                ..ask()
            },
            Ask {
                out: Some("../x.md".into()),
                ..ask()
            },
        ] {
            assert!(matches!(validate(&bad), Err(Error::Refused(_))));
        }
    }

    #[test]
    fn the_output_stays_inside_the_project() {
        let root = Path::new("proj");
        let (abs, rel) = out_path(root, None, 7).expect("default");
        assert_eq!(rel, ".offrig/out/completion-7.md");
        assert!(abs.starts_with(root));
        assert!(out_path(root, Some("arr/hymn.md"), 1).is_ok());
        for bad in ["../up.md", "a/../../b.md"] {
            assert!(out_path(root, Some(bad), 1).is_err(), "{bad}");
        }
        let abs_path = std::env::temp_dir().join("x.md");
        assert!(out_path(root, abs_path.to_str(), 1).is_err());
        assert_eq!(
            reasoning_path(Path::new("a/b.md")),
            PathBuf::from("a/b.md.reasoning.md")
        );
    }
}
