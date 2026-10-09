//! One error enum for the library. Binaries wrap it in `anyhow` with context.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("RUNPOD_API_KEY is not set")]
    MissingApiKey,

    #[error("runpod api returned {status} for {what}: {body}")]
    Api {
        what: String,
        status: u16,
        body: String,
    },

    #[error("OPENROUTER_API_KEY is not set")]
    MissingOpenRouterKey,

    #[error("openrouter returned {status} for {what}: {body}")]
    OpenRouter {
        what: String,
        status: u16,
        body: String,
    },

    #[error("runpod graphql error for {what}: {message}")]
    GraphQl { what: String, message: String },

    #[error("http request failed for {what}")]
    Http {
        what: String,
        #[source]
        source: Box<ureq::Error>,
    },

    #[error("could not decode {what}")]
    Decode {
        what: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("io error while {what}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not valid JSONC: {message}", path.display())]
    Jsonc { path: PathBuf, message: String },

    #[error("config error: {0}")]
    Config(String),

    #[error("no pod named or with id {0}")]
    PodNotFound(String),

    #[error("pod {name} has no ssh endpoint yet (it needs a public ip and a 22/tcp mapping)")]
    NoSshEndpoint { name: String },

    #[error("guard refused: {0}")]
    Guard(String),

    #[error("timed out waiting for {0}")]
    Timeout(String),

    #[error("ssh failed: {0}")]
    Ssh(String),

    #[error("ollama: {0}")]
    Ollama(String),

    #[error("engine: {0}")]
    Engine(String),

    /// The model stopped at its token limit before it finished (`done_reason: length`).
    /// What it wrote is not an answer. `thinking` keeps what a thinking model had reasoned
    /// by then, for diagnosis only; it is never part of the message.
    #[error("truncated: {message}")]
    Truncated { message: String, thinking: String },

    /// A verifier reply that failed its schema, twice. Never reported as a verdict.
    #[error("bad verdict reply: {0}")]
    BadVerdict(String),

    #[error("cancelled while {0}")]
    Cancelled(String),

    #[error("no capacity: {0}")]
    NoCapacity(String),

    /// A pod was rented, and bills, but never became ready (issue #25). Unlike
    /// `NoCapacity`, money was at stake: the pod existed from the moment it was created.
    #[error("pod {pod_id} was rented and billed but was not ready: {what}")]
    PodNotReady { pod_id: String, what: String },

    #[error("database error while {what}")]
    Db {
        what: String,
        #[source]
        source: rusqlite::Error,
    },

    #[error("refused: {0}")]
    Refused(String),

    #[error("over budget: {0}")]
    Budget(String),

    #[error("illegal transition for handoff {id}: {from} -> {to}{hint}")]
    Transition {
        id: i64,
        from: String,
        to: String,
        hint: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            what: what.into(),
            source,
        }
    }

    pub(crate) fn http(what: impl Into<String>, source: ureq::Error) -> Self {
        Error::Http {
            what: what.into(),
            source: Box::new(source),
        }
    }

    pub(crate) fn decode(what: impl Into<String>, source: serde_json::Error) -> Self {
        Error::Decode {
            what: what.into(),
            source,
        }
    }
}

impl Error {
    /// This error as seen from a launch whose pod `pod_id` already exists: a timeout
    /// waiting for the pod's address, ssh or readiness becomes `PodNotReady`, so it
    /// cannot be mistaken for the `no_capacity` of a launch that rented nothing. Any
    /// other error is returned as it was.
    pub fn after_rental(self, pod_id: &str) -> Error {
        match self {
            Error::Timeout(what) => Error::PodNotReady {
                pod_id: pod_id.to_string(),
                what: format!("timed out waiting for {what}"),
            },
            other => other,
        }
    }
}

impl Error {
    /// A stable snake_case code for this error, for machine callers (the side-car's
    /// `code` field and the CLI's exit status). Codes never change once released.
    /// A truncation with no thinking kept.
    pub fn truncated(message: impl Into<String>) -> Self {
        Error::Truncated {
            message: message.into(),
            thinking: String::new(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Error::MissingApiKey | Error::MissingOpenRouterKey => "missing_api_key",
            Error::OpenRouter { .. } => "openrouter_api",
            Error::Api { .. } | Error::GraphQl { .. } => "runpod_api",
            Error::Http { .. } if self.is_client_timeout() => "timeout",
            Error::Http { .. } => "network",
            Error::Decode { .. } => "internal",
            Error::Io { .. } => "io",
            Error::Jsonc { .. } | Error::Config(_) => "config",
            Error::PodNotFound(_) => "not_found",
            Error::NoSshEndpoint { .. } | Error::Ssh(_) => "ssh",
            Error::Guard(_) => "guard_refused",
            Error::Timeout(_) => "timeout",
            Error::Ollama(_) | Error::Engine(_) => "model_server",
            Error::Truncated { .. } => "truncated",
            Error::BadVerdict(_) => "bad_verdict",
            Error::Cancelled(_) => "cancelled",
            Error::NoCapacity(_) => "no_capacity",
            Error::PodNotReady { .. } => "pod_not_ready",
            Error::Db { .. } => "database",
            Error::Refused(_) => "refused",
            Error::Budget(_) => "budget_exceeded",
            Error::Transition { .. } => "invalid_transition",
        }
    }

    /// Whether this is an HTTP call that hit the client's global time limit (as
    /// opposed to a refused or reset connection). Its code is `timeout`.
    pub fn is_client_timeout(&self) -> bool {
        matches!(
            self,
            Error::Http { source, .. }
                if matches!(**source, ureq::Error::Timeout(ureq::Timeout::Global))
        )
    }

    /// Whether the same call can reasonably succeed if tried again later.
    pub fn retryable(&self) -> bool {
        match self {
            Error::Api { status, .. } | Error::OpenRouter { status, .. } => {
                *status == 429 || *status >= 500
            }
            Error::Http { .. }
            | Error::NoSshEndpoint { .. }
            | Error::Ssh(_)
            | Error::Timeout(_)
            | Error::Ollama(_)
            | Error::Engine(_)
            | Error::NoCapacity(_)
            | Error::PodNotReady { .. } => true,
            _ => false,
        }
    }
}

/// Render an error with its whole `source()` chain on one line: `outer: cause: cause`.
pub fn chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(c) = cur {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cur = c.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_snake_case() {
        assert_eq!(Error::MissingApiKey.code(), "missing_api_key");
        assert_eq!(Error::Guard("x".into()).code(), "guard_refused");
        assert_eq!(Error::Budget("x".into()).code(), "budget_exceeded");
        assert_eq!(Error::NoCapacity("x".into()).code(), "no_capacity");
        assert_eq!(Error::Timeout("x".into()).code(), "timeout");
        let slow = Error::http("chat", ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!((slow.code(), slow.is_client_timeout()), ("timeout", true));
        let refused = Error::http("chat", ureq::Error::ConnectionFailed);
        assert_eq!(
            (refused.code(), refused.is_client_timeout()),
            ("network", false)
        );
        assert_eq!(Error::Config("x".into()).code(), "config");
        assert_eq!(Error::PodNotFound("x".into()).code(), "not_found");
        let api = |status| Error::Api {
            what: "w".into(),
            status,
            body: String::new(),
        };
        assert_eq!(api(500).code(), "runpod_api");
        let or = |status| Error::OpenRouter {
            what: "w".into(),
            status,
            body: String::new(),
        };
        assert_eq!(or(402).code(), "openrouter_api");
        assert!(or(503).retryable() && !or(402).retryable());
        assert_eq!(Error::MissingOpenRouterKey.code(), "missing_api_key");
        for e in [
            Error::MissingApiKey,
            Error::Cancelled("x".into()),
            Error::Refused("x".into()),
            api(400),
        ] {
            let c = e.code();
            assert!(
                !c.is_empty() && c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{c}"
            );
        }
    }

    #[test]
    fn only_transient_errors_are_retryable() {
        let api = |status| Error::Api {
            what: "w".into(),
            status,
            body: String::new(),
        };
        assert!(api(503).retryable());
        assert!(api(429).retryable());
        assert!(!api(401).retryable());
        assert!(Error::NoCapacity("x".into()).retryable());
        assert!(Error::Timeout("x".into()).retryable());
        assert!(!Error::MissingApiKey.retryable());
        assert!(!Error::Budget("x".into()).retryable());
        assert!(!Error::Guard("x".into()).retryable());
    }

    fn every_variant() -> Vec<(Error, &'static str, bool)> {
        let decode = serde_json::from_str::<i32>("x").expect_err("not json");
        let io = || std::io::Error::other("disk");
        vec![
            (Error::MissingApiKey, "missing_api_key", false),
            (
                Error::GraphQl {
                    what: "w".into(),
                    message: "m".into(),
                },
                "runpod_api",
                false,
            ),
            (
                Error::http("w", ureq::Error::ConnectionFailed),
                "network",
                true,
            ),
            (Error::decode("w", decode), "internal", false),
            (Error::io("w", io()), "io", false),
            (
                Error::Jsonc {
                    path: "a.json".into(),
                    message: "m".into(),
                },
                "config",
                false,
            ),
            (Error::NoSshEndpoint { name: "n".into() }, "ssh", true),
            (Error::Ssh("x".into()), "ssh", true),
            (
                Error::PodNotReady {
                    pod_id: "p1".into(),
                    what: "ssh".into(),
                },
                "pod_not_ready",
                true,
            ),
            (Error::Ollama("x".into()), "model_server", true),
            (Error::Engine("x".into()), "model_server", true),
            (Error::truncated("x"), "truncated", false),
            (Error::BadVerdict("x".into()), "bad_verdict", false),
            (
                Error::Db {
                    what: "w".into(),
                    source: rusqlite::Error::InvalidQuery,
                },
                "database",
                false,
            ),
            (Error::Refused("x".into()), "refused", false),
            (
                Error::Transition {
                    id: 1,
                    from: "a".into(),
                    to: "b".into(),
                    hint: String::new(),
                },
                "invalid_transition",
                false,
            ),
        ]
    }

    #[test]
    fn every_variant_has_a_stable_code_and_a_retry_answer() {
        for (e, code, retry) in every_variant() {
            assert_eq!(e.code(), code, "{e}");
            assert_eq!(e.retryable(), retry, "{e}");
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn a_timeout_after_the_pod_exists_is_pod_not_ready_and_says_it_was_billed() {
        let e = Error::Timeout("pod p1 to get an ssh endpoint".into()).after_rental("p1");
        assert_eq!(e.code(), "pod_not_ready");
        assert!(e.retryable());
        let text = e.to_string();
        assert!(
            text.contains("pod p1 was rented and billed")
                && text.contains("timed out waiting for pod p1 to get an ssh endpoint"),
            "{text}"
        );
        // Nothing else changes: only a timeout means "rented but not ready".
        assert_eq!(
            Error::Ssh("refused".into()).after_rental("p1").code(),
            "ssh"
        );
        assert_eq!(
            Error::Cancelled("x".into()).after_rental("p1").code(),
            "cancelled"
        );
        // A launch that rented nothing keeps its own code.
        assert_eq!(Error::NoCapacity("x".into()).code(), "no_capacity");
    }

    #[test]
    fn display_names_what_failed() {
        let shown = |e: Error| e.to_string();
        assert_eq!(shown(Error::MissingApiKey), "RUNPOD_API_KEY is not set");
        assert_eq!(
            shown(Error::Jsonc {
                path: "s.json".into(),
                message: "bad comma".into()
            }),
            "s.json is not valid JSONC: bad comma"
        );
        assert_eq!(
            shown(Error::Transition {
                id: 7,
                from: "pending".into(),
                to: "complete".into(),
                hint: " (allowed: dispatched)".into()
            }),
            "illegal transition for handoff 7: pending -> complete (allowed: dispatched)"
        );
        assert!(shown(Error::NoSshEndpoint { name: "p".into() }).contains("pod p has no ssh"));
    }

    #[test]
    fn chain_joins_the_whole_source_chain() {
        let e = Error::io("reading x", std::io::Error::other("disk gone"));
        assert_eq!(chain(&e), "io error while reading x: disk gone");
        let leaf = Error::MissingApiKey;
        assert_eq!(chain(&leaf), "RUNPOD_API_KEY is not set");
        let db = Error::Db {
            what: "saving".into(),
            source: rusqlite::Error::InvalidQuery,
        };
        assert!(chain(&db).starts_with("database error while saving: "));
    }
}
