//! Who's connected to the server and what they're running, and stopping a
//! query or session (Postgres, MySQL).

use super::*;

/// A session on the server: PostgreSQL's backend pid, MySQL's connection
/// id (0 where there's none: SQLite, DuckDB). What a cancel or kill is
/// for, and how a reconnect is told from the same session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub(crate) i64);

impl SessionId {
    /// As the server lists it (`pid::text`, `CAST(id AS CHAR)`).
    pub(crate) fn parse(text: &str) -> Option<Self> {
        text.trim().parse().ok().map(Self)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    /// Postgres backend pid / MySQL connection id.
    pub id: SessionId,
    pub user: String,
    pub database: String,
    pub client: String,
    /// active, idle, idle in transaction… (MySQL: the command and state).
    pub state: String,
    /// Seconds in the current state or query.
    pub seconds: Option<i64>,
    /// What it's waiting on, if anything.
    pub wait: String,
    pub query: String,
    /// Sessions holding locks this one waits for.
    pub blocked_by: Vec<SessionId>,
    /// The session of this app's tab.
    pub own: bool,
}

/// What the server says of this connection's own session while a query
/// of it runs (`Database::own_session`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnSession {
    /// Running something: the query (or its rows on their way).
    Busy,
    /// Waiting for a statement, this many seconds: the query never got
    /// there, or its answer never got back.
    Idle(i64),
    /// Not on the server at all: ended (a restart, a timeout, killed).
    Gone,
}

impl Database {
    /// The server's word on this connection's session, from a connection
    /// of the pool: whether the query being waited for still runs there.
    /// `None` where sessions can't be listed (or the session isn't known).
    pub async fn own_session(&self) -> Result<Option<OwnSession>> {
        if !self.0.engine.capabilities().activity {
            return Ok(None);
        }
        let own = self.session_id();
        if own == SessionId::default() {
            return Ok(None);
        }
        let sessions = on_driver!(self.0.engine, engine::queries::activity(self).await)?;
        Ok(Some(match sessions.iter().find(|a| a.id == own) {
            None => OwnSession::Gone,
            // PostgreSQL's idle (in transaction, too); MySQL's Sleep.
            Some(a) if a.state.starts_with("idle") || a.state == "sleep" => OwnSession::Idle(a.seconds.unwrap_or(0)),
            Some(_) => OwnSession::Busy,
        }))
    }

    /// Sessions on the server, busiest first. Runs on a pool connection, so
    /// it works while this tab's query is running.
    pub async fn activity(&self) -> Result<Vec<Activity>> {
        let mut sessions = on_driver!(self.0.engine, engine::queries::activity(self).await)?;
        let own = self.session_id();
        for a in &mut sessions {
            a.own = a.id == own;
        }
        Ok(sessions)
    }

    /// Cancels session `id`'s running query, or (`terminate`) ends the
    /// session.
    pub async fn stop_session(&self, id: SessionId, terminate: bool) -> Result<()> {
        on_driver!(self.0.engine, engine::queries::stop_session(self, id.0, terminate).await)
    }
}
