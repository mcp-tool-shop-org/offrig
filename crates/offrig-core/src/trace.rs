//! Output levels and secret redaction, shared by the binaries.
//!
//! The level is process-wide and set once at start-up by the CLI. Detail goes to
//! stderr so stdout stays the command's result. Every line is redacted first: the
//! `RUNPOD_API_KEY` value and any `Bearer` token never reach the terminal at any level.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// How much a command says. Ordered: each level includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Errors only.
    Quiet = 0,
    /// Progress and results.
    Normal = 1,
    /// Plus which API calls are made and how long they take.
    Verbose = 2,
    /// Plus full error chains and response bodies of failed calls.
    Debug = 3,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Normal as u8);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Quiet,
        2 => Level::Verbose,
        3 => Level::Debug,
        _ => Level::Normal,
    }
}

pub fn enabled(at: Level) -> bool {
    level() >= at
}

const MASK: &str = "[redacted]";

/// `text` with each of `secrets` and every `Bearer <token>` replaced by `[redacted]`.
/// Secrets shorter than 4 characters are ignored: masking them would shred the text.
pub fn redact_with(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| s.trim().len() >= 4) {
        out = out.replace(s.trim(), MASK);
    }
    // `Bearer <token>`: the token runs to the next whitespace, quote or delimiter.
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(at) = rest.to_ascii_lowercase().find("bearer ") {
        let (head, tail) = rest.split_at(at + "bearer ".len());
        result.push_str(head);
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '}' | ')'))
            .unwrap_or(tail.len());
        if end > 0 {
            result.push_str(MASK);
        }
        rest = &tail[end..];
    }
    result.push_str(rest);
    result
}

/// `redact_with` using the live `RUNPOD_API_KEY`.
pub fn redact(text: &str) -> String {
    let key = std::env::var("RUNPOD_API_KEY").unwrap_or_default();
    redact_with(text, &[&key])
}

/// A line for `--verbose` (and `--debug`).
pub fn verbose(msg: &str) {
    if enabled(Level::Verbose) {
        eprintln!("[verbose] {}", redact(msg));
    }
}

/// A line for `--debug` only.
pub fn debug(msg: &str) {
    if enabled(Level::Debug) {
        eprintln!("[debug] {}", redact(msg));
    }
}

/// Record one RunPod API call: what it was, the status and how long it took.
pub(crate) fn api(what: &str, status: u16, started: Instant) {
    verbose(&format!(
        "runpod: {what} -> {status} in {}",
        millis(started.elapsed())
    ));
}

fn millis(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_masked_wherever_it_appears() {
        let key = "rpa_SECRETKEY123";
        let text = format!("runpod api returned 401: bad token {key} for {key}");
        let out = redact_with(&text, &[key]);
        assert!(!out.contains(key), "{out}");
        assert_eq!(out.matches(MASK).count(), 2);
    }

    #[test]
    fn bearer_tokens_are_masked_even_when_the_key_is_unknown() {
        let out = redact_with(r#"{"Authorization":"Bearer abc.def-123"}"#, &[]);
        assert!(!out.contains("abc.def-123"), "{out}");
        assert!(out.contains("Bearer [redacted]"), "{out}");
        let out = redact_with("header authorization: BEARER tok123 sent", &[]);
        assert!(!out.contains("tok123"), "{out}");
        assert!(out.ends_with("sent"), "{out}");
    }

    #[test]
    fn short_or_empty_secrets_do_not_shred_the_text() {
        assert_eq!(redact_with("hello world", &["", "  ", "o"]), "hello world");
    }

    #[test]
    fn plain_text_is_unchanged() {
        let t = "pod abc at $1.20/hr";
        assert_eq!(redact_with(t, &["rpa_SECRETKEY123"]), t);
    }

    #[test]
    fn levels_are_ordered() {
        assert!(Level::Debug > Level::Verbose);
        assert!(Level::Verbose > Level::Normal);
        assert!(Level::Normal > Level::Quiet);
    }
}
