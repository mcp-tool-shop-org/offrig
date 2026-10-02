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

    #[error("cancelled while {0}")]
    Cancelled(String),

    #[error("no capacity: {0}")]
    NoCapacity(String),
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
