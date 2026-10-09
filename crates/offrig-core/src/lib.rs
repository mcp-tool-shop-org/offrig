//! offrig-core: run models on RunPod and wire them into Zed, so they never touch
//! the local GPU.
//!
//! The pod runs a pinned Ollama on its loopback and exposes only SSH. The one path to
//! it is an SSH tunnel on a port that is not the local Ollama's, so if the pod or the
//! tunnel is down a request fails instead of landing on this machine. `guard` checks
//! that this still holds.

pub mod calibrate;
pub mod calibrate_run;
pub mod checks;
pub mod config;
pub mod context;
pub mod cost;
pub mod error;
pub mod fsutil;
pub mod guard;
pub mod index;
pub mod job;
pub mod lanes;
pub mod ollama;
pub mod openrouter;
pub mod planning;
pub mod proc;
pub mod remote;
pub mod roles;
pub mod runner;
pub mod runpod;
pub mod session;
pub mod siblings;
pub mod sidecar_port;
pub mod spec;
pub mod sshconfig;
pub mod stage;
pub mod store;
pub mod trace;
pub mod tunnel;
pub mod verify;
pub mod watchdog;
pub mod zed;

pub use error::{Error, Result};
