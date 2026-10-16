//! Who's connected to the server and what they're running, and stopping a
//! query or session (Postgres, MySQL).

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    /// Postgres backend pid / MySQL connection id.
    pub id: String,
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
    pub blocked_by: Vec<String>,
    /// The session of this app's tab.
    pub own: bool,
}

impl Database {
    /// Sessions on the server, busiest first. Runs on a pool connection, so
    /// it works while this tab's query is running.
    pub async fn activity(&self) -> Result<Vec<Activity>> {
        let mut sessions = on_engine!(self.0.engine, engine::queries::activity(self).await)?;
        let own = self.0.backend_id.load(Ordering::SeqCst).to_string();
        for a in &mut sessions {
            a.own = a.id == own;
        }
        Ok(sessions)
    }

    /// Cancels session `id`'s running query, or (`terminate`) ends the
    /// session.
    pub async fn stop_session(&self, id: &str, terminate: bool) -> Result<()> {
        let id: i64 = id.parse().context("not a session id")?;
        on_engine!(self.0.engine, engine::queries::stop_session(self, id, terminate).await)
    }
}
