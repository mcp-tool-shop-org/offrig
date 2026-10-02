//! offrig-core: run models on RunPod and wire them into Zed, so they never touch
//! the local GPU.
//!
//! The pod runs a pinned Ollama on its loopback and exposes only SSH. The one path to
//! it is an SSH tunnel on a port that is not the local Ollama's, so if the pod or the
//! tunnel is down a request fails instead of landing on this machine. `guard` checks
//! that this still holds.

pub mod config;
pub mod cost;
pub mod error;
pub mod fsutil;
pub mod guard;
pub mod ollama;
pub mod proc;
pub mod remote;
pub mod runpod;
pub mod session;
pub mod spec;
pub mod sshconfig;
pub mod tunnel;
pub mod zed;

pub use error::{Error, Result};
