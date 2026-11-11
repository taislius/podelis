//! What's done once a second while something runs: each tab's query (its
//! time limit, saying how to cancel it), watch mode, keepalives, agents'
//! approvals, the sandbox, and noticing a background task that panicked.

use super::*;

/// A query running this long has its session looked at on the server…
const SESSION_WATCH_AFTER: Duration = Duration::from_secs(20);
/// …this often…
const SESSION_WATCH_EVERY: Duration = Duration::from_secs(20);
/// …with this long for an answer: longer than a connection of the pool
/// may take to come free (its acquire timeout, 10 s), so a busy pool isn't
/// a server that doesn't answer.
const SESSION_WATCH_WITHIN: Duration = Duration::from_secs(15);
/// A session waiting for a statement this long (seconds) while its query
/// runs here never got it, or its answer never got back.
const SESSION_IDLE_LOST: i64 = 10;

/// What the server said of a running query's session.
#[derive(Debug)]
pub(in crate::app) enum SessionSeen {
    Said(Option<crate::db::OwnSession>),
    Failed(String),
    NoAnswer,
}

impl App {
    /// Called every second while a query runs: enforces the connection's
    /// timeout and notices background tasks that panicked.
    pub fn tick_running(&mut self) {
        self.offer_export_question();
        self.watch_tick();
        self.keepalive_tick();
        self.activity_tick();
        if self.watching_agents() {
            self.approvals_tick();
            self.agents_tick();
        }
        self.share_tick();
        self.sandbox_tick();
        // Each running tab's query: the shown one's, and those in other tabs.
        self.run_tick();
        let shown = self.active_tab;
        for i in self.running_tabs().into_iter().filter(|&i| i != shown) {
            self.in_tab(i, Self::run_tick);
        }
        self.check_background_panic();
    }

    /// The shown tab's running query, once a second: its timeout, and
    /// saying how to cancel it once it's run long.
    fn run_tick(&mut self) {
        if let Some((deadline, limit)) = self.tab.run.extra.deadline
            && self.tab.run.executing
            && Instant::now() >= deadline
        {
            self.tab.run.extra.deadline = None;
            self.tab.run.extra.timed_out = Some(limit);
            self.cancel_query();
        }
        self.offer_cancel_tick();
        self.watch_session_tick();
    }

    /// A query that's run a while: the server is asked (on a connection of
    /// the pool) whether its session still runs it, every so often. A
    /// session that's gone, or that's waited for a statement all along,
    /// means the link dropped without a word (a VPN, a laptop's sleep, a
    /// firewall's idle timeout): the run would otherwise wait for good
    /// (found by hand, 2026-11-11). PostgreSQL and MySQL, which list their
    /// sessions.
    fn watch_session_tick(&mut self) {
        let extra = &self.tab.run.extra;
        let ran = self.tab.run.since.map_or(Duration::ZERO, |s| s.elapsed());
        if !self.tab.run.executing
            || !extra.on_session
            || extra.session_asking
            || ran < SESSION_WATCH_AFTER
            || extra.session_asked.is_some_and(|at| at.elapsed() < SESSION_WATCH_EVERY)
        {
            return;
        }
        let Some(db) = self.tab.run.db.clone().filter(|db| db.engine().capabilities().activity) else { return };
        self.tab.run.extra.session_asked = Some(Instant::now());
        self.tab.run.extra.session_asking = true;
        let send = self.run_sender();
        tokio::spawn(async move {
            let seen = match tokio::time::timeout(SESSION_WATCH_WITHIN, db.own_session()).await {
                Ok(Ok(seen)) => SessionSeen::Said(seen),
                Ok(Err(e)) => SessionSeen::Failed(crate::db::error_message(&e)),
                Err(_) => SessionSeen::NoAnswer,
            };
            send(DbEventKind::SessionSeen(seen));
        });
    }

    /// The server's word on the running query's session (in its tab).
    pub(in crate::app) fn session_seen(&mut self, seen: SessionSeen) {
        self.tab.run.extra.session_asking = false;
        if !self.tab.run.executing {
            return;
        }
        let said = match seen {
            SessionSeen::Said(Some(crate::db::OwnSession::Gone)) => t!("run.session_gone").to_string(),
            SessionSeen::Said(Some(crate::db::OwnSession::Idle(secs))) if secs >= SESSION_IDLE_LOST => {
                t!("run.session_idle", s = secs)
            }
            SessionSeen::Said(_) => return,
            // The server answered, with an error (sessions it won't list,
            // say): no word on the link. Not asked again for this run.
            SessionSeen::Failed(e) => {
                tracing::debug!(error = e, "the server's sessions couldn't be read");
                self.tab.run.extra.on_session = false;
                return;
            }
            // Not even on a new connection: the server can't be reached.
            SessionSeen::NoAnswer => {
                t!("run.server_silent", why = t!("info.no_answer", s = SESSION_WATCH_WITHIN.as_secs()))
            }
        };
        // Said once (the top bar keeps "connection lost"), and Esc stops
        // waiting at once, giving the session up (the next run reconnects).
        // Not stopped here: behind a pooler that hands each transaction
        // another server connection (PgBouncer's transaction mode), the
        // session the server lists may not be the one running this.
        self.tab.run.extra.on_session = false;
        self.tab.run.cancel_failed = true;
        if self.hidden_tab.is_none() {
            self.keepalive.connection_lost = true;
        }
        self.log(said.clone(), true);
        self.set_status(said, true);
    }

    /// The check before this run found its session doesn't answer: the
    /// run goes on reconnecting, said, and Esc stops waiting at once (a
    /// session held by something else still stuck can't be taken over).
    pub(in crate::app) fn session_replaced(&mut self, why: &str) {
        if !self.tab.run.executing {
            return;
        }
        self.tab.run.cancel_failed = true;
        if self.hidden_tab.is_none() {
            self.keepalive.connection_lost = true;
        }
        let said = t!("run.session_replaced", why = one_line(why));
        self.log(said.clone(), false);
        self.set_result_status(said, false);
    }

    /// A query running past `long_query` says, once, how to cancel it: in
    /// the status bar (and a notification while you're in another window),
    /// never as a question that takes the keys being typed.
    fn offer_cancel_tick(&mut self) {
        if !self.tab.run.executing {
            self.tab.run.extra.offered_cancel = false;
            return;
        }
        let after = self.settings.long_query;
        let long = self.tab.run.since.is_some_and(|s| s.elapsed() >= Duration::from_secs(after));
        if after == 0 || self.tab.run.cancelling || self.tab.run.extra.offered_cancel || !long {
            return;
        }
        self.tab.run.extra.offered_cancel = true;
        let secs = self.tab.run.since.map_or(0, |s| s.elapsed().as_secs());
        let tab = self.run_tab().unwrap_or(self.active_tab);
        let name = self.tabs.get(tab).map(|t| t.name.clone()).unwrap_or_default();
        let on = self.tabs.get(tab).and_then(|t| t.conn).and_then(|id| self.conn_name(id)).unwrap_or_default();
        let said = t!("run.long_running", time = mm_ss(secs), tab = name, on = on);
        self.set_status(said.clone(), false);
        if self.terminal_focused != Some(true) {
            self.for_terminal.notification = Some(said);
        }
    }

    /// Reports a background task that panicked (it never reports back).
    fn check_background_panic(&mut self) {
        if let Some(message) = crate::system::crash::take_background() {
            self.tab.run.executing = false;
            self.tab.run.task = None;
            self.tab.run.set_stream(None);
            let log = crate::system::crash::log_path();
            self.set_status(t!("run.internal_error", message = message, log = log.display()), true);
        }
    }
}
