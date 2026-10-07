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

    #[error("cancelled while {0}")]
    Cancelled(String),

    #[error("no capacity: {0}")]
    NoCapacity(String),

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
    /// A stable snake_case code for this error, for machine callers (the side-car's
    /// `code` field and the CLI's exit status). Codes never change once released.
    pub fn code(&self) -> &'static str {
        match self {
            Error::MissingApiKey => "missing_api_key",
            Error::Api { .. } | Error::GraphQl { .. } => "runpod_api",
            Error::Http { .. } => "network",
            Error::Decode { .. } => "internal",
            Error::Io { .. } => "io",
            Error::Jsonc { .. } | Error::Config(_) => "config",
            Error::PodNotFound(_) => "not_found",
            Error::NoSshEndpoint { .. } | Error::Ssh(_) => "ssh",
            Error::Guard(_) => "guard_refused",
            Error::Timeout(_) => "timeout",
            Error::Ollama(_) | Error::Engine(_) => "model_server",
            Error::Cancelled(_) => "cancelled",
            Error::NoCapacity(_) => "no_capacity",
            Error::Db { .. } => "database",
            Error::Refused(_) => "refused",
            Error::Budget(_) => "budget_exceeded",
            Error::Transition { .. } => "invalid_transition",
        }
    }

    /// Whether the same call can reasonably succeed if tried again later.
    pub fn retryable(&self) -> bool {
        match self {
            Error::Api { status, .. } => *status == 429 || *status >= 500,
            Error::Http { .. }
            | Error::NoSshEndpoint { .. }
            | Error::Ssh(_)
            | Error::Timeout(_)
            | Error::Ollama(_)
            | Error::Engine(_)
            | Error::NoCapacity(_) => true,
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
        assert_eq!(Error::Config("x".into()).code(), "config");
        assert_eq!(Error::PodNotFound("x".into()).code(), "not_found");
        let api = |status| Error::Api {
            what: "w".into(),
            status,
            body: String::new(),
        };
        assert_eq!(api(500).code(), "runpod_api");
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
}
