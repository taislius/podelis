//! Talking to the gateway: connect and say hello, call methods, and
//! start it first when it isn't running (the user never has to).

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::protocol::{self, Outgoing, RpcError};
use super::server::{LineError, read_line};

/// How long to wait for a gateway that's just been started.
const START_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum CallError {
    /// The gateway answered with an error.
    Rpc(RpcError),
    /// The connection failed or the gateway went away.
    Io(std::io::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Its message says what went wrong; the code is for programs.
            CallError::Rpc(e) => write!(f, "{}", e.message),
            CallError::Io(e) => write!(f, "the gateway couldn't be reached: {e}"),
        }
    }
}

impl std::error::Error for CallError {}

impl From<std::io::Error> for CallError {
    fn from(e: std::io::Error) -> Self {
        CallError::Io(e)
    }
}

pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    /// The gateway's answer to hello: its version.
    pub hello: Value,
    /// Notifications that came while waiting for responses, oldest first.
    pub notifications: Vec<(String, Value)>,
}

impl Client {
    /// Connects to a running gateway and says hello as `name`. Fails if
    /// none is running.
    pub async fn connect(config_dir: &Path, name: &str) -> Result<Self, CallError> {
        Self::connect_speaking(config_dir, name, super::PROTOCOL).await
    }

    /// Connects saying hello in `protocol`: another version's, to stop
    /// that gateway.
    async fn connect_speaking(config_dir: &Path, name: &str, protocol: u32) -> Result<Self, CallError> {
        let stream = UnixStream::connect(super::usable_socket_path(config_dir)?).await?;
        let (read, writer) = stream.into_split();
        let mut client =
            Client { reader: BufReader::new(read), writer, next_id: 1, hello: Value::Null, notifications: Vec::new() };
        client.hello = client.call("hello", json!({ "client": name, "protocol": protocol })).await?;
        Ok(client)
    }

    /// Calls `method` and waits for its response.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        let mut kept = Vec::new();
        let result = self.call_streaming(method, params, |m, p| kept.push((m.to_string(), p.clone()))).await;
        self.notifications.extend(kept);
        result
    }

    /// Calls `method`, handing each notification that comes meanwhile to
    /// `on_note` as it arrives (an answer being written), and waits for
    /// the response.
    pub async fn call_streaming(
        &mut self,
        method: &str,
        params: Value,
        mut on_note: impl FnMut(&str, &Value),
    ) -> Result<Value, CallError> {
        let id = self.next_id;
        self.next_id += 1;
        self.writer.write_all(protocol::request(id, method, &params).as_bytes()).await?;
        loop {
            match self.read().await? {
                Outgoing::Response { id: got, outcome } if got == json!(id) => {
                    return outcome.map_err(CallError::Rpc);
                }
                // A response to a request given up on earlier.
                Outgoing::Response { .. } => {}
                Outgoing::Notification { method, params } => on_note(&method, &params),
            }
        }
    }

    /// The connection itself, for a protocol that takes it over after a
    /// call (`mcp.open`): what's read so far stays in the reader.
    pub fn into_stream(self) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
        (self.reader, self.writer)
    }

    /// The next notification: one that came during a call, else waits for
    /// one. (Tests only until a client listens for events.)
    #[cfg(test)]
    pub async fn next_notification(&mut self) -> Result<(String, Value), CallError> {
        if !self.notifications.is_empty() {
            return Ok(self.notifications.remove(0));
        }
        loop {
            if let Outgoing::Notification { method, params } = self.read().await? {
                return Ok((method, params));
            }
        }
    }

    async fn read(&mut self) -> Result<Outgoing, CallError> {
        let mut buf = Vec::new();
        match read_line(&mut self.reader, &mut buf).await {
            Ok(true) => {}
            Ok(false) => return Err(gone("the gateway closed the connection")),
            Err(LineError::TooLong) => return Err(gone("the gateway sent a line that's too long")),
            Err(LineError::Io(e)) => return Err(CallError::Io(e)),
        }
        protocol::parse_outgoing(&buf).map_err(|e| gone(&e))
    }
}

fn gone(why: &str) -> CallError {
    CallError::Io(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, why.to_string()))
}

/// Connects to the gateway, starting it with `start` when none answers,
/// then waiting for it to listen.
pub async fn connect_or_start(
    config_dir: &Path,
    name: &str,
    start: impl FnOnce() -> std::io::Result<()>,
) -> Result<Client, CallError> {
    match Client::connect(config_dir, name).await {
        Ok(client) => return Ok(client),
        // An older version's gateway (left running from before an
        // update): stopped, and this version's started. A newer one is
        // left alone: two installed versions mustn't stop each other's.
        Err(CallError::Rpc(e)) if e.code == super::protocol::code::VERSION => {
            if their_protocol(&e).is_some_and(|theirs| theirs > super::PROTOCOL) {
                return Err(CallError::Rpc(RpcError::new(
                    e.code,
                    "a newer Podėlis's gateway is running: use that version (or `podelis gateway stop` with it)",
                )));
            }
            replace_other(config_dir, &e).await?;
        }
        Err(e @ CallError::Rpc(_)) => return Err(e),
        // No socket could be made there (`usable_socket_path`).
        Err(CallError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput => return Err(CallError::Io(e)),
        Err(CallError::Io(_)) => {}
    }
    start()?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_millis(25)).await;
        match Client::connect(config_dir, name).await {
            Ok(client) => return Ok(client),
            // Another client is replacing the old gateway too: soon gone.
            Err(CallError::Rpc(e)) if e.code == super::protocol::code::VERSION && Instant::now() < deadline => {}
            Err(e @ CallError::Rpc(_)) => return Err(e),
            Err(CallError::Io(e)) if Instant::now() >= deadline => {
                return Err(CallError::Io(std::io::Error::new(
                    e.kind(),
                    format!("started the gateway, but it didn't answer within {} s: {e}", START_TIMEOUT.as_secs()),
                )));
            }
            Err(CallError::Io(_)) => {}
        }
    }
}

/// The protocol another version's gateway speaks, from its refusal: its
/// `data` (from version 2), else its message ("speaks protocol 1").
fn their_protocol(e: &RpcError) -> Option<u32> {
    if let Some(n) = e.data.as_ref().and_then(|d| d["protocol"].as_u64()) {
        return u32::try_from(n).ok();
    }
    let rest = e.message.split("speaks protocol ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Stops another version's gateway (its refusal says which), and waits
/// for it to let go of the socket.
pub async fn replace_other(config_dir: &Path, refused: &RpcError) -> Result<(), CallError> {
    let Some(theirs) = their_protocol(refused) else { return Err(CallError::Rpc(refused.clone())) };
    // Another client may be replacing it at the same moment: gone already
    // (or the new one answering in this version) is done, not an error.
    let Ok(mut old) = Client::connect_speaking(config_dir, "podelis (replacing an older gateway)", theirs).await else {
        return Ok(());
    };
    let _ = old.call("shutdown", json!({})).await;
    let deadline = Instant::now() + START_TIMEOUT;
    while super::socket_path(config_dir).exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// Starts `podelis gateway` in the background for `config_dir`: in a
/// process group of its own (closing the terminal doesn't end it), with
/// no terminal input or output. If two clients start one at once, the
/// lock lets one run; the other leaves and both connect to the first.
pub fn spawn_gateway(config_dir: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("gateway")
        .env("PODELIS_CONFIG_DIR", config_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // Reaped when it ends, so it doesn't linger as a zombie while the
    // client runs.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
