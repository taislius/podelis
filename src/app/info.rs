//! The connection info panel, and keep-alive checks of idle connections.

use super::*;

/// How often a lost connection is checked again.
const LOST_RECHECK: Duration = Duration::from_secs(5);
/// … and how soon after the last check something the user does checks it.
const LOST_RECHECK_AFTER_INPUT: Duration = Duration::from_secs(2);
/// How long a check waits for the server.
const PING_WITHIN: Duration = Duration::from_secs(5);

impl App {
    /// Shows who and where the open connection is: user, database,
    /// settings, TLS, latency.
    pub(super) fn show_connection_info(&mut self) {
        let Some(db) = self.database.clone() else {
            return self.set_status("This tab isn't connected".to_string(), true);
        };
        let form = &self.connect_form;
        let mut local = vec![
            ("Connection".to_string(), if form.name.is_empty() { "(unsaved)".to_string() } else { form.name.clone() }),
            ("Target".to_string(), form.summary()),
            ("Mode".to_string(), form.mode.label().to_string()),
        ];
        if let Ok(Some(t)) = form.query_timeout() {
            local.push(("Query timeout".to_string(), format!("{} s", t.as_secs())));
        }
        if let Some(l) = self.connect_latency {
            local.push(("Latency".to_string(), format!("{} ms", l.as_millis())));
        }
        let title = format!("CONNECTION · {}", local[0].1);
        let send = self.sender();
        tokio::spawn(async move {
            let started = Instant::now();
            let _ = db.run("SELECT 1", 1).await;
            let ping = started.elapsed();
            let mut rows = local;
            rows.retain(|(k, _)| k != "Latency");
            rows.push(("Latency".to_string(), format!("{:.1} ms (now)", ping.as_secs_f64() * 1000.0)));
            rows.extend(db.session_info().await);
            let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
            let mut text = String::new();
            for (k, v) in rows {
                let _ = writeln!(text, "{k:<width$}  {v}");
            }
            send(DbEventKind::Definition(title, Ok(text)));
        });
    }

    /// Facts for a bug report: versions, terminal, settings, the kind of
    /// connection. No hosts, users, passwords or SQL.
    pub(super) fn diagnostics(&self) -> String {
        let mut rows = crate::cli::doctor::environment(&self.settings);
        if let Some(db) = &self.database {
            let form = &self.connect_form;
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
        self.run.executing
            || self.activity.is_some()
            || (self.settings.keepalive > 0 && self.database.is_some())
            // A lost connection, checked again until it's back.
            || (self.connection_lost && self.database.is_some())
            // A plan's age, kept current.
            || (self.tab.result_tab == ResultTab::Plan && self.tab.plan_source.is_some())
            // What agents did, for the status bar's note.
            || self.watching_agents()
            || self.watch.is_some()
            // The sharing question, waiting behind another confirmation.
            || self.pending_share.is_some()
    }

    /// Pings an idle connection every `keepalive` seconds: notices a dropped
    /// connection (and reconnects) before the next query, and keeps the
    /// latency current.
    pub(super) fn keepalive_tick(&mut self) {
        // A lost connection is checked again every few seconds, heartbeat
        // or not, so it's said as soon as it's back (and reconnected).
        let every = if self.connection_lost { LOST_RECHECK } else { Duration::from_secs(self.settings.keepalive) };
        if every.is_zero() || self.run.executing || self.pinging || self.last_activity.elapsed() < every {
            return;
        }
        self.ping();
    }

    /// Something done while the connection is lost (a key, a click):
    /// checks it again now rather than at the next tick, once in a while.
    pub(super) fn recheck_lost_connection(&mut self) {
        if self.connection_lost
            && !self.run.executing
            && !self.pinging
            && self.last_activity.elapsed() >= LOST_RECHECK_AFTER_INPUT
        {
            self.ping();
        }
    }

    /// `SELECT 1` on the session (a dropped one is reopened, as for a
    /// query). No answer within `PING_WITHIN` is said as such; whether the
    /// session is given up for it is `pinged`'s to decide.
    fn ping(&mut self) {
        let Some(db) = self.database.clone() else { return };
        self.pinging = true;
        self.last_activity = Instant::now();
        let send = self.sender();
        tokio::spawn(async move {
            let started = Instant::now();
            let result = if let Ok(result) = tokio::time::timeout(PING_WITHIN, db.run("SELECT 1", 1)).await {
                result
                    .map(|o| (started.elapsed(), o.reconnected))
                    .map_err(|e| (crate::db::error_message(&e), Ping::Failed))
            } else {
                Err((format!("no answer in {} s", PING_WITHIN.as_secs()), Ping::NoAnswer))
            };
            send(DbEventKind::Pinged(result));
        });
    }

    /// Switches the connection heartbeat on (every `keepalive` seconds, 30
    /// by default) or off. Not saved: `keepalive` in config.toml sets the
    /// default.
    pub(super) fn toggle_heartbeat(&mut self) {
        if self.settings.keepalive > 0 {
            self.heartbeat_secs = self.settings.keepalive;
            self.settings.keepalive = 0;
            self.set_status("Connection heartbeat off".to_string(), false);
        } else {
            self.settings.keepalive = self.heartbeat_secs.max(1);
            // Check right away.
            self.last_activity =
                Instant::now().checked_sub(Duration::from_secs(self.settings.keepalive)).unwrap_or(self.last_activity);
            let msg = format!("Connection heartbeat on: checking every {} s while idle", self.settings.keepalive);
            self.set_status(msg, false);
            self.keepalive_tick();
        }
    }

    pub(super) fn pinged(&mut self, result: Result<(Duration, bool), (String, Ping)>) {
        self.pinging = false;
        let was_lost = std::mem::replace(&mut self.connection_lost, result.is_err());
        // No answer while rechecking a lost connection: the session is
        // given up (the next check opens a new one rather than waiting on
        // a dead link), unless a query now runs on it or a transaction is
        // open (that would lose it without a word). A heartbeat that
        // stalled once only says so.
        if matches!(result, Err((_, Ping::NoAnswer)))
            && was_lost
            && !self.run.executing
            && self.txn == TxnState::Idle
            && let Some(db) = &self.database
        {
            db.abandon_session();
        }
        let result = result.map_err(|(e, _)| e);
        match result {
            Ok((latency, reconnected)) => {
                self.connect_latency = Some(latency);
                if was_lost && !reconnected {
                    self.log("The connection is back".to_string(), false);
                    self.set_status("The connection is back".to_string(), false);
                }
                if reconnected {
                    self.txn = TxnState::Idle;
                    self.log("The connection had dropped: reconnected".to_string(), true);
                    self.set_status(
                        "The connection had dropped: reconnected (session settings were reset)".to_string(),
                        true,
                    );
                }
            }
            // Said once: while it stays lost, the checks every few seconds
            // keep quiet (the status bar says "connection lost").
            Err(_) if was_lost => {}
            Err(e) => {
                let msg = format!("Connection check failed: {}", crate::db::with_hint(&e).replace('\n', " · "));
                self.log(msg.clone(), true);
                self.set_status(msg, true);
            }
        }
    }
}
