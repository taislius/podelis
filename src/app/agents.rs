//! The agent activity window: what AI agents did through `podelis mcp`,
//! by session (one agent run), each session's calls, and one call in full;
//! and the note in the status bar when agents were refused, failed or
//! changed data since the user last looked.

use std::time::{Duration, Instant};

use super::*;
use crate::agents::Alerts;

/// A query an AI agent ran through `podelis mcp`, and what's worth a look
/// about it (refused, failed, a dry run, rows changed, a lint warning).
#[derive(Clone, Debug)]
pub struct AgentQuery {
    pub entry: crate::agents::Entry,
    /// Refused, failed, a dry run or rows changed.
    pub flag: Option<String>,
    /// The SQL's first lint warning, worked out when it's first needed (the
    /// row drawn, or `!`): a session can have thousands of calls.
    lint: std::cell::OnceCell<Option<String>>,
    /// The engine to read its SQL by.
    engine: crate::db::Engine,
}

impl AgentQuery {
    pub(super) fn new(entry: crate::agents::Entry, engine: crate::db::Engine) -> Self {
        let flag = if let Some(why) = &entry.refused {
            Some(format!("refused: {why}"))
        } else if let Some(e) = &entry.error {
            Some(format!("failed: {e}"))
        } else if entry.dry_run {
            Some(format!("dry run: would change {} row(s)", entry.changed.unwrap_or(0)))
        } else if entry.changed.is_some_and(|n| n > 0) {
            Some(format!("changed {} row(s)", entry.changed.unwrap_or(0)))
        } else {
            None
        };
        // Read as the engine it ran on, when the log says.
        let engine = entry.engine.unwrap_or(engine);
        Self { entry, flag, lint: std::cell::OnceCell::new(), engine }
    }

    /// What's worth a look about it, if anything: the flag, else a lint
    /// warning.
    pub fn notice(&self) -> Option<&str> {
        self.flag.as_deref().or_else(|| {
            self.lint
                .get_or_init(|| {
                    let script = crate::parse::parse(&self.entry.sql, self.engine);
                    crate::lint::lints(&script, "").first().map(|h| h.message.clone())
                })
                .as_deref()
        })
    }

    /// How it went, briefly: rows read or changed, or what's worth a look.
    pub fn outcome(&self) -> String {
        if let Some(notice) = self.notice() {
            return notice.to_string();
        }
        match (self.entry.rows, self.entry.changed) {
            (Some(rows), _) => format!("{rows} row(s)"),
            (None, Some(changed)) => format!("{changed} row(s) changed"),
            _ => "done".to_string(),
        }
    }
}

/// The agent activity window: the sessions, or one session's calls.
pub struct AgentsView {
    /// The session open (an index into [`App::agent_sessions`]); none
    /// shows the sessions.
    pub session: Option<usize>,
    pub selected: usize,
    /// The sessions' search while a session is open, for going back to it.
    pub sessions_filter: LineInput,
    pub filter: LineInput,
    pub filtering: bool,
    /// Only what's worth a look: refused, failed, writes (and, among
    /// calls, lint warnings).
    pub flagged_only: bool,
}

/// How often the log is checked for something new while agents may use
/// the project.
const ALERT_CHECK: Duration = Duration::from_secs(2);

/// `/home/me/src/shop` → `~/src/shop`.
pub fn short_dir(dir: &str) -> String {
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(home) if dir == home => "~".to_string(),
        Some(home) if dir.starts_with(&format!("{home}/")) => format!("~{}", &dir[home.len()..]),
        _ => dir.to_string(),
    }
}

/// `10-13 09:18` from an RFC 3339 time.
pub fn short_time(at: &str) -> String {
    at.get(5..16).unwrap_or(at).replace('T', " ")
}

impl App {
    /// Reads what AI agents ran in this project (`podelis mcp`), newest
    /// first: again only when the file has changed since, since a big log
    /// takes a moment to read and check. Whether it was read again.
    fn load_agent_log(&mut self) -> bool {
        let engine = self.engine();
        let path =
            self.current_project.as_deref().and_then(|id| Some(crate::agents::log_path(self.data_dir.as_ref()?, id)));
        let stamp = path.as_ref().and_then(|p| {
            let meta = std::fs::metadata(p).ok()?;
            Some((p.clone(), meta.modified().ok()?, meta.len()))
        });
        if stamp.is_some() && stamp == self.agent_log_stamp {
            return false;
        }
        self.agent_log = path
            .map(|p| crate::agents::read(&p))
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(|entry| AgentQuery::new(entry, engine))
            .collect();
        self.agent_log_stamp = stamp;
        self.agent_sessions = crate::agents::sessions(self.agent_log.iter().map(|q| &q.entry));
        true
    }

    fn agent_paths(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        let dir = self.data_dir.as_ref()?;
        let id = self.current_project.as_deref()?;
        Some((crate::agents::log_path(dir, id), crate::agents::seen_path(dir, id)))
    }

    /// Whether any connection of the project is shared with agents: then
    /// the log is watched for what's new.
    pub fn watching_agents(&self) -> bool {
        self.current_project
            .as_deref()
            .and_then(|id| self.store.project(id))
            .is_some_and(|p| p.connections.iter().any(|c| c.agents != crate::db::AgentAccess::None))
    }

    /// Every couple of seconds: what agents did since the user last looked.
    /// Not while the window is open: it shows what was there when it
    /// opened, and the rows it points at stay put.
    pub(super) fn agents_tick(&mut self) {
        if self.agents_view.is_some() || self.agent_checked.is_some_and(|at| at.elapsed() < ALERT_CHECK) {
            return;
        }
        self.agent_checked = Some(Instant::now());
        self.update_agent_alerts();
    }

    pub fn update_agent_alerts(&mut self) {
        let reread = self.load_agent_log();
        let seen = self.agent_paths().and_then(|(_, seen)| crate::agents::read_seen(&seen));
        if reread || seen != self.agent_seen {
            self.agent_alerts = crate::agents::alerts_since(self.agent_log.iter().map(|q| &q.entry), seen.as_deref());
            self.agent_seen = seen;
        }
    }

    /// Forgets the agent log and window of the project being left.
    pub(super) fn reset_agents(&mut self) {
        self.agent_log.clear();
        self.agent_log_stamp = None;
        self.agent_sessions.clear();
        self.agent_alerts = Alerts::default();
        self.agent_seen = None;
        self.agent_checked = None;
        self.agents_view = None;
    }

    /// The status bar's note: what agents did that's worth a look since the
    /// window was last opened (while anything is shared with them).
    pub fn agent_note(&self) -> Option<String> {
        let Alerts { refused, failed, wrote, dry_runs } = self.agent_alerts;
        if !self.agent_alerts.any() || !self.watching_agents() {
            return None;
        }
        let dry = if dry_runs == 1 { "dry run" } else { "dry runs" };
        let parts: Vec<String> = [(refused, "refused"), (failed, "failed"), (wrote, "wrote"), (dry_runs, dry)]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
        Some(format!("agents: {}", parts.join(" · ")))
    }

    /// Opens the agent activity window, and marks everything in it seen.
    pub fn open_agents(&mut self) {
        self.load_agent_log();
        if let (Some((_, seen)), Some(newest)) = (self.agent_paths(), self.agent_log.first()) {
            crate::agents::write_seen(&seen, &newest.entry.at);
            self.agent_seen = Some(newest.entry.at.clone());
        }
        self.agent_alerts = Alerts::default();
        self.agents_view = Some(AgentsView {
            session: None,
            selected: 0,
            sessions_filter: LineInput::default(),
            filter: LineInput::default(),
            filtering: false,
            flagged_only: false,
        });
    }

    /// The rows shown: sessions (indexes into `agent_sessions`) or, with a
    /// session open, its calls (indexes into `agent_log`), as filtered.
    pub fn agents_rows(&self) -> Vec<usize> {
        let Some(view) = &self.agents_view else { return Vec::new() };
        let text = view.filter.text();
        match view.session.and_then(|i| self.agent_sessions.get(i)) {
            None => (0..self.agent_sessions.len())
                .filter(|&i| {
                    let s = &self.agent_sessions[i];
                    let flagged = s.refused + s.failed + s.wrote + s.dry_runs > 0;
                    let dir = s.dir.as_deref().unwrap_or_default();
                    let mut fields = vec![s.label(), &s.agent, dir];
                    fields.extend(s.connections.iter().map(String::as_str));
                    (!view.flagged_only || flagged) && crate::library::matches(text, &fields)
                })
                .collect(),
            Some(session) => session
                .entries
                .iter()
                .copied()
                .filter(|&i| {
                    // Searching uses what's cheap to know; only `!` checks
                    // each call's SQL for lint warnings.
                    let q = &self.agent_log[i];
                    let flag = q.flag.as_deref().unwrap_or_default();
                    (!view.flagged_only || q.notice().is_some())
                        && crate::library::matches(text, &[&q.entry.sql, &q.entry.connection, &q.entry.tool, flag])
                })
                .collect(),
        }
    }

    pub fn handle_agents_key(&mut self, key: KeyEvent) {
        let rows = self.agents_rows();
        let Some(view) = &mut self.agents_view else { return };
        if view.filtering {
            match key.code {
                KeyCode::Enter => view.filtering = false,
                KeyCode::Esc => {
                    view.filtering = false;
                    view.filter = LineInput::default();
                    view.selected = 0;
                }
                _ => {
                    if view.filter.handle(key) == InputResult::Changed {
                        view.selected = 0;
                    }
                }
            }
            return;
        }
        if let Some(i) = list::step(view.selected, rows.len(), key, Nav::LIST) {
            view.selected = i;
            return;
        }
        let row = rows.get(view.selected).copied();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Back to the sessions as they were searched, on the one that
                // was open.
                if let Some(open) = view.session.take() {
                    view.filter = std::mem::take(&mut view.sessions_filter);
                    let rows = self.agents_rows();
                    let Some(view) = &mut self.agents_view else { return };
                    view.selected = rows.iter().position(|&i| i == open).unwrap_or(0);
                } else {
                    self.agents_view = None;
                }
            }
            KeyCode::Char('/') => view.filtering = true,
            KeyCode::Char('!') => {
                view.flagged_only = !view.flagged_only;
                view.selected = 0;
            }
            KeyCode::Enter if view.session.is_none() => {
                if let Some(i) = row {
                    view.sessions_filter = std::mem::take(&mut view.filter);
                    view.session = Some(i);
                    view.selected = 0;
                }
            }
            KeyCode::Enter => {
                if let Some(i) = row {
                    let text = self.agent_query_text(i);
                    self.cell_view = Some(CellView::new("AGENT QUERY".to_string(), text));
                }
            }
            KeyCode::Char('o') if view.session.is_some() => {
                let Some(q) = row.and_then(|i| self.agent_log.get(i)) else { return };
                let (sql, ran_on) = (q.entry.sql.clone(), q.entry.connection.clone());
                let name = q.entry.name.clone().unwrap_or_else(|| q.entry.agent.clone());
                self.agents_view = None;
                self.open_sql_in_tab(Some(name.clone()), &sql, None);
                self.set_status(format!("{name} ran this on {ran_on}; here it runs on this tab's connection"), false);
            }
            KeyCode::Char('y') if view.session.is_some() => {
                if let Some(q) = row.and_then(|i| self.agent_log.get(i)) {
                    let sql = q.entry.sql.clone();
                    self.copy_text(sql, "Copied the agent's SQL");
                }
            }
            _ => {}
        }
    }

    /// One call in full, for the text view.
    fn agent_query_text(&self, i: usize) -> String {
        let Some(q) = self.agent_log.get(i) else { return String::new() };
        let e = &q.entry;
        let mut agent = e.agent.clone();
        if let Some(v) = &e.agent_version {
            let _ = write!(agent, " {v}");
        }
        if let Some(t) = &e.agent_title {
            let _ = write!(agent, " ({t})");
        }
        let mut lines = vec![format!("When        {}", e.at.replace('T', " "))];
        if let Some(name) = &e.name {
            lines.push(format!("Name        {name}"));
        }
        lines.push(format!("Agent       {agent}"));
        if !e.session.is_empty() {
            lines.push(format!("Session     {}", e.session));
        }
        if let Some(dir) = &e.dir {
            lines.push(format!("Folder      {}", short_dir(dir)));
        }
        lines.push(format!("Connection  {}", e.connection));
        lines.push(format!("Tool        {} · {} ms", e.tool, e.ms));
        lines.push(format!("Outcome     {}", q.outcome()));
        if let Some(why) = &e.refused {
            lines.push(String::new());
            lines.push(format!("Refused: {why}"));
        }
        if let Some(err) = &e.error {
            lines.push(String::new());
            lines.push(format!("Error: {err}"));
        }
        let engine = e.engine.unwrap_or_else(|| self.engine());
        let sql = crate::sql::format(&e.sql, engine).unwrap_or_else(|| e.sql.clone());
        lines.push(String::new());
        lines.push(sql);
        lines.join("\n")
    }
}
