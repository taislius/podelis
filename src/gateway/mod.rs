//! The gateway: the one place for models, context, tools and memory
//! (planned in podelis-planai, ai-gateway.md). A separate long-running
//! process, `podelis gateway`, that clients (the TUI, `podelis ask`,
//! `podelis mcp`) talk to over a private Unix socket.
//!
//! - `protocol`: JSON-RPC 2.0, one JSON message per line; the first
//!   request must be `hello`, with the protocol version.
//! - `server`: one gateway per config folder (a lock file), the socket,
//!   stopping when idle, the call log.
//! - `client`: connecting, and starting the gateway on first use.
//!
//! This first part has the process and the protocol only (`hello`,
//! `status`, `shutdown`); models, context, tools and memory come next.

pub mod client;
pub mod command;
pub mod protocol;
pub mod server;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The protocol's version: a client and a gateway must agree on it.
pub const PROTOCOL: u32 = 1;

/// The gateway's folder in the config folder: its socket, lock and log,
/// readable only by the user.
pub fn dir(config_dir: &Path) -> PathBuf {
    config_dir.join("gateway")
}

pub fn socket_path(config_dir: &Path) -> PathBuf {
    dir(config_dir).join("gateway.sock")
}

fn lock_path(config_dir: &Path) -> PathBuf {
    dir(config_dir).join("gateway.lock")
}

fn log_path(config_dir: &Path) -> PathBuf {
    dir(config_dir).join("calls.jsonl")
}

/// How long the gateway waits with no client before it stops; `None`
/// keeps it running. From `gateway_idle` (minutes) in `config.toml`.
pub fn idle_from_minutes(minutes: u64) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
}
