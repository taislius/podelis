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
//! - `models`, `ai`: the models it talks to (Anthropic's API, and OpenAI's
//!   format for GPT and local servers), and the `models` and `complete`
//!   methods.
//!
//! Context, tools and memory come next.

mod ai;
pub mod client;
pub mod command;
pub mod context;
pub mod memory;
mod migrations;
pub mod models;
mod names;
pub mod protocol;
pub mod server;
#[cfg(test)]
pub(crate) mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The protocol's version: a client and a gateway must agree on it. A
/// client meeting another version stops that gateway and starts its own
/// (`client::replace_other`). 2: models, context, ask, memory, mcp.open.
pub const PROTOCOL: u32 = 2;

/// The gateway's folder in the config folder: its socket, lock and log,
/// readable only by the user.
pub fn dir(config_dir: &Path) -> PathBuf {
    config_dir.join("gateway")
}

pub fn socket_path(config_dir: &Path) -> PathBuf {
    dir(config_dir).join("gateway.sock")
}

/// The longest socket path the OS takes (`sun_path` is 104 bytes on
/// macOS, 108 on Linux; one for the final NUL).
const MAX_SOCKET_PATH: usize = 103;

/// The socket's path, if the system can take it: a config folder deep
/// enough makes it too long, and no gateway could listen there.
pub fn usable_socket_path(config_dir: &Path) -> std::io::Result<PathBuf> {
    let socket = socket_path(config_dir);
    let len = socket.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the gateway's socket path is too long for the OS ({len} bytes, at most {MAX_SOCKET_PATH}): {}; \
                 set PODELIS_CONFIG_DIR to a shorter folder",
                socket.display()
            ),
        ));
    }
    Ok(socket)
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
