//! What a method gets while it handles a request: the config folder, the
//! caches, a way to push notifications to its client (and nothing else to
//! write there), and what the call log may keep about it.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

use super::caches::Caches;
use super::methods::{Delta, Note};

/// Sends a client notifications, the only thing a method may write to it.
pub(super) struct Notifier<'a>(&'a mut (dyn AsyncWrite + Unpin + Send + Sync));

impl<'a> Notifier<'a> {
    pub(super) fn new(write: &'a mut (dyn AsyncWrite + Unpin + Send + Sync)) -> Self {
        Notifier(write)
    }

    /// Sends `note`. A client that went away still gets its work done (and
    /// logged): a failed write isn't an error.
    pub(super) async fn send(&mut self, note: &Note) {
        let _ = self.0.write_all(note.line().as_bytes()).await;
    }
}

/// One request being handled.
pub(super) struct Call<'a> {
    pub(super) config_dir: &'a Path,
    pub(super) caches: &'a Caches,
    /// The request's id, which its notifications carry.
    id: Value,
    notes: Notifier<'a>,
    /// What the call log keeps about it besides the method and outcome:
    /// the project, connection, model, tokens. Never a question, SQL or
    /// a fact's text.
    log: Map<String, Value>,
}

impl<'a> Call<'a> {
    pub(super) fn new(config_dir: &'a Path, caches: &'a Caches, id: Value, notes: Notifier<'a>) -> Self {
        Call { config_dir, caches, id, notes, log: Map::new() }
    }

    pub(super) fn id(&self) -> &Value {
        &self.id
    }

    /// Notes `value` as `key` in the call's log line.
    pub(super) fn log(&mut self, key: &str, value: impl Serialize) {
        self.log.insert(key.to_string(), serde_json::to_value(value).unwrap_or_default());
    }

    pub(super) fn into_log(self) -> Map<String, Value> {
        self.log
    }

    pub(super) async fn notify(&mut self, note: &Note) {
        self.notes.send(note).await;
    }

    /// Runs `work`, sending what comes on `pieces` to the client as `note`s
    /// meanwhile (an answer being written). `work` holds the sending side,
    /// so the pieces end when it does.
    pub(super) async fn relay<T>(
        &mut self,
        note: fn(Delta) -> Note,
        mut pieces: UnboundedReceiver<String>,
        work: impl Future<Output = T>,
    ) -> T {
        let id = self.id.clone();
        let notes = &mut self.notes;
        let forward = async move {
            while let Some(text) = pieces.recv().await {
                notes.send(&note(Delta { id: id.clone(), text })).await;
            }
        };
        tokio::join!(work, forward).0
    }
}
