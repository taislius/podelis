//! The connection info panel, and keep-alive checks of idle connections.

use crate::app::*;

/// How long a check asked for (`r`) waits for its turn on the session.
const CHECK_WAIT: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(6) };

impl App {
    fn connection_check_status(&mut self, message: String, is_error: bool, manual: bool) {
        // "Checking…" has had its answer.
        self.notes.editor_note = None;
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
        if self.connection.checking {
            return self.set_status(t!("info.reconnecting").to_string(), false);
        }
        let (Some(id), Some(db)) = (self.connection.slots.active, self.connection.database.clone()) else { return };
        self.connection.checking = true;
        self.connection.checked_lost = self.connection.state.is_lost();
        self.set_status(t!("info.reconnecting").to_string(), false);
        // The supervisor checks it in the session's turn (and goes on
        // checking a lost one, paused or not).
        let send = self.conn_sender(id);
        tokio::spawn(async move {
            // Its turn not waited for for good: a result's rows still open
            // hold the session (given up on before it, the check isn't
            // run). Once it's started, its answer is waited for.
            let result = match db.check_within_turn(CHECK_WAIT).await {
                Ok(Some(checked)) => Ok(checked),
                Ok(None) => Err((t!("info.check_busy").to_string(), Ping::Busy)),
                Err(e) => {
                    let ping = if crate::db::is_no_answer(&e) { Ping::NoAnswer } else { Ping::Failed };
                    Err((crate::db::error_message(&e), ping))
                }
            };
            send(ConnEvent::Pinged(result));
        });
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
            // A reconnect left for a run that stopped without its answer.
            || self.reconnect_unsaid()
            || self.popups.activity.is_some()
            // An export paused behind another question: its own comes after.
            || self.exports.task.as_ref().is_some_and(|run| run.paused.is_some())
            // An open sandbox's age, said once it's long.
            || self.connection.sandbox.is_some()
            // A lost connection: its countdown to the next check.
            || (self.connection.state.is_lost() && self.connection.database.is_some())
            // A heartbeat switched on or off, for the open connections.
            || self.applied_heartbeat != self.settings.keepalive
            // A plan's age, kept current.
            || (self.tab.result_tab == ResultTab::Plan && self.tab.plan_source.is_some())
            // What agents did, for the status bar's note.
            || self.watching_agents()
            || self.watch.is_some()
            // The sharing question, waiting behind another confirmation.
            || self.ask.pending_share.is_some()
    }

    /// The heartbeat as set (`keepalive` seconds, 0 for none), passed on
    /// to every open connection's supervisor when it changes: each checks
    /// its own idle connection, a hidden tab's too.
    pub(in crate::app) fn apply_heartbeat(&mut self) {
        if self.applied_heartbeat == self.settings.keepalive {
            return;
        }
        let switched_on = self.applied_heartbeat == 0 && self.settings.keepalive > 0;
        self.applied_heartbeat = self.settings.keepalive;
        for (_, slot) in self.connection.all() {
            if let Some(db) = &slot.database {
                self.heartbeat_for(db);
                // Switched on: checked at once, not a heartbeat from now.
                if switched_on {
                    db.check_soon();
                }
            }
        }
    }

    /// Connection `db`'s heartbeat, as set.
    pub(in crate::app) fn heartbeat_for(&self, db: &crate::db::Database) {
        let every = (self.settings.keepalive > 0).then(|| Duration::from_secs(self.settings.keepalive));
        db.set_heartbeat(every);
    }

    /// Something done while the connection is lost (a key, a click): its
    /// supervisor checks it again soon (not if it was just checked, or
    /// paused: `r` resumes).
    pub(in crate::app) fn recheck_lost_connection(&mut self) {
        if self.connection.state.is_lost()
            && let Some(db) = &self.connection.database
        {
            db.nudge();
        }
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
            let msg = t!("info.heartbeat_on", s = self.settings.keepalive);
            self.set_status(msg, false);
        }
    }

    /// The check asked for (`r`) came back: said. Where the connection
    /// stands is its supervisor's report; this says what the check found.
    pub(in crate::app) fn pinged(&mut self, result: Result<(Duration, bool), (String, Ping)>) {
        self.connection.checking = false;
        let asked_lost = std::mem::take(&mut self.connection.checked_lost);
        let still_lost = self.connection.state.is_lost();
        match result {
            Ok((latency, reconnected)) => {
                self.connection.latency = Some(latency);
                self.connection.state.answered();
                // A reconnect is said by its report (what the old session
                // had, a transaction lost). "Back" is logged once: by its report
                // if that came first (shown here too, where it was asked),
                // else here.
                let (message, log) = match () {
                    () if reconnected => (None, false),
                    () if still_lost => (Some(t!("info.back")), true),
                    () if asked_lost => (Some(t!("info.back")), false),
                    () => (Some(t!("info.check_ok")), true),
                };
                if let Some(message) = message {
                    if log {
                        self.log(message.to_string(), false);
                    }
                    self.connection_check_status(message.to_string(), false, true);
                }
            }
            // The session's held: said, not taken for lost.
            Err((e, Ping::Busy)) => self.connection_check_status(e, false, true),
            Err((e, _)) => {
                self.connection.state.lose();
                crate::system::connection_log::record("check failed", &self.connection.form.short_name(), &e);
                let msg = t!("info.check_failed", e = crate::db::with_hint(&e).replace('\n', " · "));
                self.log(msg.clone(), true);
                self.connection_check_status(msg, true, true);
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
