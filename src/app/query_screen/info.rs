//! The connection info panel, and keep-alive checks of idle connections.

use crate::app::*;

/// How soon after the last check something the user does checks a lost
/// connection again (the checks on their own wait longer each time:
/// `KeepAlive::next_lost_check`).
const LOST_RECHECK_AFTER_INPUT: Duration = Duration::from_secs(2);
/// How long a check waits for the server.
const PING_WITHIN: Duration = Duration::from_secs(5);

impl App {
    fn connection_check_status(&mut self, message: String, is_error: bool, manual: bool) {
        if manual && self.focus == Focus::Results {
            self.notes.results_note = None;
            self.set_result_status(message, is_error);
        } else {
            self.set_status(message, is_error);
        }
    }

    /// Explicitly checks a lost connection now; a successful backend ping
    /// opens a replacement session when the old one has gone away.
    pub(in crate::app) fn reconnect_now(&mut self) {
        if self.connection.database.is_none() {
            return self.set_status(t!("info.not_connected").to_string(), true);
        }
        if self.conn_busy() {
            return self.set_status(t!("info.stop_query_first").to_string(), true);
        }
        if self.connection.keepalive.pinging() {
            return self.set_status(t!("info.reconnecting").to_string(), false);
        }
        self.connection.keepalive.begin_reconnect();
        self.set_status(t!("info.reconnecting").to_string(), false);
        self.ping();
    }

    /// Shows who and where the open connection is: user, database,
    /// settings, TLS, latency.
    pub(in crate::app) fn show_connection_info(&mut self) {
        let Some(db) = self.connection.database.clone() else {
            return self.set_status(t!("info.not_connected").to_string(), true);
        };
        let form = &self.connection.form;
        let mut local = vec![
            (
                t!("info.connection").to_string(),
                if form.name.is_empty() { t!("info.unsaved").to_string() } else { form.name.clone() },
            ),
            (t!("info.target").to_string(), form.summary()),
            (t!("info.mode").to_string(), form.mode.label().to_string()),
        ];
        if let Ok(Some(t)) = form.query_timeout() {
            local.push((t!("info.query_timeout").to_string(), format!("{} s", t.as_secs())));
        } else if form.timeout.trim().is_empty() && self.settings.query_timeout > 0 {
            // The one for every connection (`query_timeout`).
            let every = t!("info.every_connection", s = self.settings.query_timeout);
            local.push((t!("info.query_timeout").to_string(), every));
        }
        if let Some(l) = self.connection.latency {
            local.push((t!("info.latency").to_string(), format!("{} ms", l.as_millis())));
        }
        let title = format!("{} · {}", t!("info.title"), local[0].1);
        let send = self.sender();
        tokio::spawn(async move {
            let started = Instant::now();
            let _ = db.run(db.engine().backend().ping(), 1).await;
            let ping = started.elapsed();
            let mut rows = local;
            let latency = t!("info.latency").to_string();
            rows.retain(|(k, _)| *k != latency);
            rows.push((latency, t!("info.latency_now", ms = format!("{:.1}", ping.as_secs_f64() * 1000.0))));
            rows.extend(db.session_info().await.into_iter().map(session_row));
            let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
            let mut text = String::new();
            for (k, v) in rows {
                let _ = writeln!(text, "{k:<width$}  {v}");
            }
            send(DbEventKind::Definition { title, text: Ok(text) });
        });
    }

    /// `Ctrl+K s l`: the server's load, as its own views say it
    /// (connections, running queries, lock waits, cache hits…), in the text
    /// view; again to read it again.
    pub(in crate::app) fn show_server_load(&mut self) {
        let Some(db) = self.connection.database.clone() else {
            return self.set_status(t!("info.not_connected").to_string(), true);
        };
        if db.engine().dialect().is_none_or(|d| d.load_queries().is_empty()) {
            return self.set_status(t!("info.no_load", engine = db.engine().label()), true);
        }
        let title = format!("{} · {}", t!("info.load_title"), self.connection.form.short_name());
        self.set_status(t!("info.reading_load").to_string(), false);
        let send = self.sender();
        tokio::spawn(async move {
            let rows = db.server_load().await;
            let at = chrono::Local::now().format("%H:%M:%S");
            let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
            let mut text = String::new();
            for (k, v) in rows {
                let _ = writeln!(text, "{k:<width$}  {v}");
            }
            let _ = write!(text, "\n{}", t!("info.load_at", at = at));
            send(DbEventKind::Definition { title, text: Ok(text) });
        });
    }

    /// Facts for a bug report: versions, terminal, settings, the kind of
    /// connection. No hosts, users, passwords or SQL.
    pub(in crate::app) fn diagnostics(&self) -> String {
        let mut rows = crate::system::environment::environment(&self.settings);
        if let Some(db) = &self.connection.database {
            let form = &self.connection.form;
            let ssh = if form.ssh.trim().is_empty() { "" } else { ", via SSH" };
            rows.push((
                "Connection".to_string(),
                format!(
                    "{} {}, {} mode, SSL {}{ssh}",
                    form.engine.label(),
                    db.version(),
                    form.mode.label(),
                    form.ssl_mode.label()
                ),
            ));
        }
        let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
        let mut out = String::new();
        for (k, v) in &rows {
            let _ = writeln!(out, "{k:<width$}  {v}");
        }
        out
    }

    /// Whether the main loop should tick every second (a query is running,
    /// keep-alive checks are on, the connection is lost…).
    pub fn wants_ticks(&self) -> bool {
        !self.running_tabs().is_empty()
            || self.popups.activity.is_some()
            // An export paused behind another question: its own comes after.
            || self.exports.task.as_ref().is_some_and(|run| run.paused.is_some())
            // An open sandbox's age, said once it's long.
            || self.connection.sandbox.is_some()
            || (self.settings.keepalive > 0 && self.connection.database.is_some())
            // A lost connection, checked again until it's back.
            || (self.connection.state.is_lost() && self.connection.database.is_some())
            // The other tabs' connections, checked as the shown one is.
            || self.connection.all().any(|(_, slot)| {
                slot.database.is_some() && (self.settings.keepalive > 0 || slot.state.is_lost())
            })
            // A plan's age, kept current.
            || (self.tab.result_tab == ResultTab::Plan && self.tab.plan_source.is_some())
            // What agents did, for the status bar's note.
            || self.watching_agents()
            || self.watch.is_some()
            // The sharing question, waiting behind another confirmation.
            || self.ask.pending_share.is_some()
    }

    /// Pings an idle connection every `keepalive` seconds: notices a dropped
    /// connection (and reconnects) before the next query, and keeps the
    /// latency current.
    pub(in crate::app) fn keepalive_tick(&mut self) {
        // A lost connection is checked again every few seconds, heartbeat
        // or not, so it's said as soon as it's back (and reconnected).
        let every = if self.connection.state.is_lost() {
            self.connection.keepalive.next_lost_check()
        } else {
            Duration::from_secs(self.settings.keepalive)
        };
        if every.is_zero()
            || self.conn_busy()
            || self.connection.keepalive.pinging()
            || self.connection.keepalive.last_activity.elapsed() < every
        {
            return self.check_hidden_connections();
        }
        self.ping();
        self.check_hidden_connections();
    }

    /// The other tabs' connections are checked as the shown one is (with
    /// the heartbeat on, and a lost one every few seconds until it's
    /// back), unless one of their tabs is using it: so a tab shown again
    /// says how its connection is, rather than finding out then.
    fn check_hidden_connections(&mut self) {
        let heartbeat = Duration::from_secs(self.settings.keepalive);
        let shown = self.connection.slots.active;
        let due: Vec<ConnId> = self
            .connection
            .all()
            .filter(|(id, slot)| {
                let every = if slot.state.is_lost() { slot.keepalive.next_lost_check() } else { heartbeat };
                Some(*id) != shown
                    && slot.database.is_some()
                    && !every.is_zero()
                    && !slot.keepalive.pinging()
                    && slot.keepalive.last_activity.elapsed() >= every
            })
            .map(|(id, _)| id)
            .collect();
        let due: Vec<ConnId> = due.into_iter().filter(|&id| !self.conn_in_use(id)).collect();
        for id in due {
            self.ping_conn(id);
        }
    }

    /// A query, or an export of every row, is on connection `id`'s
    /// session (a check would wait behind it).
    pub(in crate::app) fn conn_in_use(&self, id: ConnId) -> bool {
        (0..self.tabs.len()).any(|i| self.tabs[i].conn == Some(id) && self.tab_run(i).is_some_and(|r| r.executing))
            || self.exports.task.as_ref().is_some_and(|run| run.conn == Some(id))
    }

    /// Something done while the connection is lost (a key, a click):
    /// checks it again now rather than at the next tick, once in a while.
    pub(in crate::app) fn recheck_lost_connection(&mut self) {
        if self.connection.state.is_lost()
            && !self.conn_busy()
            && !self.connection.keepalive.pinging()
            && self.connection.keepalive.last_activity.elapsed() >= LOST_RECHECK_AFTER_INPUT
        {
            self.ping();
        }
    }

    /// `SELECT 1` on the session (a dropped one is reopened, as for a
    /// query). No answer within `PING_WITHIN` is said as such; whether the
    /// session is given up for it is `pinged`'s to decide.
    fn ping(&mut self) {
        let Some(id) = self.connection.slots.active else { return };
        self.ping_conn(id);
    }

    /// Checks connection `id`, shown or parked: its answer is for it,
    /// wherever it is then.
    fn ping_conn(&mut self, id: ConnId) {
        let Some(slot) = self.connection.slot_mut(id) else { return };
        let Some(db) = slot.database.clone() else { return };
        slot.keepalive.begin_check();
        slot.keepalive.last_activity = Instant::now();
        let send = self.conn_sender(id);
        tokio::spawn(async move {
            let started = Instant::now();
            let result =
                if let Ok(result) = tokio::time::timeout(PING_WITHIN, db.run(db.engine().backend().ping(), 1)).await {
                    result
                        .map(|o| (started.elapsed(), o.reconnected))
                        .map_err(|e| (crate::db::error_message(&e), Ping::Failed))
                } else {
                    Err((t!("info.no_answer", s = PING_WITHIN.as_secs()), Ping::NoAnswer))
                };
            send(ConnEvent::Pinged(result));
        });
    }

    /// Switches the connection heartbeat on (every `keepalive` seconds, 30
    /// by default) or off. Not saved: `keepalive` in config.toml sets the
    /// default.
    pub(super) fn toggle_heartbeat(&mut self) {
        if self.settings.keepalive > 0 {
            self.heartbeat_secs = self.settings.keepalive;
            self.settings.keepalive = 0;
            self.set_status(t!("info.heartbeat_off").to_string(), false);
        } else {
            self.settings.keepalive = self.heartbeat_secs.max(1);
            // Check right away.
            self.connection.keepalive.last_activity = Instant::now()
                .checked_sub(Duration::from_secs(self.settings.keepalive))
                .unwrap_or(self.connection.keepalive.last_activity);
            let msg = t!("info.heartbeat_on", s = self.settings.keepalive);
            self.set_status(msg, false);
            self.keepalive_tick();
        }
    }

    pub(in crate::app) fn pinged(&mut self, result: Result<(Duration, bool), (String, Ping)>) {
        let was_lost = self.connection.state.is_lost();
        let reconnect_requested = self.connection.keepalive.finish_check();
        if result.is_ok() {
            self.connection.state.answered();
        } else {
            self.connection.state.lose();
        }
        let lost = self.connection.state.is_lost();
        self.connection.keepalive.checked(lost);
        // No answer while rechecking a lost connection: the session is
        // given up (the next check opens a new one rather than waiting on
        // a dead link), unless a query now runs on it or a transaction is
        // open (that would lose it without a word). A heartbeat that
        // stalled once only says so.
        if matches!(result, Err((_, Ping::NoAnswer)))
            && was_lost
            && !self.conn_busy()
            && self.connection.txn == TxnState::Idle
            && let Some(db) = &self.connection.database
        {
            db.abandon_session();
        }
        let result = result.map_err(|(e, _)| e);
        match result {
            Ok((latency, reconnected)) => {
                self.connection.latency = Some(latency);
                let name = self.connection.form.short_name();
                if was_lost {
                    crate::system::connection_log::record("back", &name, &format!("{} ms", latency.as_millis()));
                }
                if reconnected {
                    crate::system::connection_log::record("reconnected", &name, "the session was reopened");
                }
                if (was_lost || reconnect_requested) && !reconnected {
                    let message = if was_lost { t!("info.back") } else { t!("info.check_ok") }.to_string();
                    self.log(message.clone(), false);
                    self.connection_check_status(message, false, reconnect_requested);
                }
                if reconnected {
                    // A reconnect loses an open transaction: said, never quietly.
                    let lost = if self.connection.txn == TxnState::Idle {
                        String::new()
                    } else {
                        format!(" · {}", t!("info.transaction_lost"))
                    };
                    self.connection.txn = TxnState::Idle;
                    let message = format!("{}: {}{lost}", t!("info.reconnected"), t!("info.settings_reset"));
                    self.log(t!("info.reconnected").to_string(), true);
                    self.connection_check_status(message, true, reconnect_requested);
                }
            }
            // Said once: while it stays lost, the checks every few seconds
            // keep quiet (the status bar says "connection lost").
            Err(_) if was_lost && !reconnect_requested => {}
            Err(e) => {
                crate::system::connection_log::record("check failed", &self.connection.form.short_name(), &e);
                let msg = t!("info.check_failed", e = crate::db::with_hint(&e).replace('\n', " · "));
                self.log(msg.clone(), true);
                self.connection_check_status(msg, true, reconnect_requested);
            }
        }
    }
}

/// A row of `Database::session_info`, in the app's language. Its names are
/// English there, as `podelis doctor` reads them by name.
fn session_row((name, value): (String, String)) -> (String, String) {
    let label = match name.as_str() {
        "Server version" => t!("session_info.server_version"),
        "Version" => t!("session_info.version"),
        "Git version" => t!("session_info.git_version"),
        "User" => t!("session_info.user"),
        "Session user" => t!("session_info.session_user"),
        "Database" => t!("session_info.database"),
        "Schemas" => t!("session_info.schemas"),
        "Time zone" => t!("session_info.time_zone"),
        "Encoding" => t!("session_info.encoding"),
        "Isolation" => t!("session_info.isolation"),
        "Statement timeout" => t!("session_info.statement_timeout"),
        "Read-only" => t!("session_info.read_only"),
        "Server address" => t!("session_info.server_address"),
        "Server started" => t!("session_info.server_started"),
        "Server" => t!("session_info.server"),
        "Journal mode" => t!("session_info.journal_mode"),
        "Foreign keys" => t!("session_info.foreign_keys"),
        "Size (bytes)" => t!("session_info.size_bytes"),
        "Access mode" => t!("session_info.access_mode"),
        "Threads" => t!("session_info.threads"),
        "Memory limit" => t!("session_info.memory_limit"),
        "Connections" => t!("session_info.connections"),
        _ => return (name, value),
    };
    let value = match value.as_str() {
        "yes" if matches!(name.as_str(), "Read-only" | "Foreign keys") => t!("session_info.yes").to_string(),
        "no" if matches!(name.as_str(), "Read-only" | "Foreign keys") => t!("session_info.no").to_string(),
        _ => value,
    };
    (label.to_string(), value)
}
