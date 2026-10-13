//! Query tabs, the saved-query library window (F3) and their prompts.
//!
//! The active tab's state is `App::tab`; inactive tabs are "parked" as a
//! [`TabState`] and swapped in on switching.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::*;
use crate::library::{self, Library};

/// A tab's state: the active tab's is `App::tab`, the others are parked.
#[derive(Default)]
pub struct TabState {
    pub editor: TextEditor,
    pub grid: ResultGrid,
    /// The last SQL run and its editor offset, for "load more rows" and
    /// cell editing, with what its statements do.
    pub(super) last_sql: Option<(String, usize)>,
    pub(super) last_query_infos: Vec<StatementInfo>,
    /// Editor text when the last query was sent.
    pub(super) last_run_text: String,
    /// Byte offset in the editor where the SQL last sent to the server
    /// starts, so error positions can be mapped back.
    pub(super) last_run_offset: usize,
    /// Range the server reported an error at, underlined in the editor.
    pub error_marker: Option<(Pos, Pos)>,
    /// A possible syntax problem found by `sqlparser` (advisory only).
    pub syntax_hint: Option<SyntaxHint>,
    /// The last EXPLAIN output, formatted, for the QUERY PLAN tab.
    pub plan: Option<crate::db::Plan>,
    /// What that plan is for, and when it was made.
    pub plan_source: Option<PlanSource>,
    pub result_tab: ResultTab,
    /// Scroll offset of the MESSAGES / QUERY PLAN tabs, down and across.
    pub tab_scroll: u16,
    pub tab_hscroll: u16,
    /// A plan that's a table (MySQL's classic EXPLAIN) as a grid, so the
    /// results' keys work on it. While QUERY PLAN shows it, it's swapped
    /// into `grid` and this holds the result's grid.
    pub plan_grid: Option<Box<ResultGrid>>,
    pub plan_swapped: bool,
}

/// The statement a plan was made for.
#[derive(Clone, Debug)]
pub struct PlanSource {
    pub sql: String,
    /// EXPLAIN ANALYZE (it ran) rather than EXPLAIN.
    pub analyzed: bool,
    /// The editor line it started on then (from 0).
    pub line: usize,
    pub at: chrono::DateTime<chrono::Local>,
}

impl PlanSource {
    /// Where the statement is in `text` now: the copy nearest the line it
    /// was on (lines above it may have come or gone), as a byte offset and
    /// line. `None` once it's been edited away.
    pub fn find_in(&self, text: &str) -> Option<(usize, usize)> {
        if self.sql.is_empty() {
            return None;
        }
        let mut line = 0;
        let mut counted = 0;
        text.match_indices(self.sql.as_str())
            .map(|(at, _)| {
                line += text[counted..at].matches('\n').count();
                counted = at;
                (at, line)
            })
            .min_by_key(|&(_, l)| l.abs_diff(self.line))
    }
}

fn swap_tab(app: &mut App, state: &mut TabState) {
    std::mem::swap(&mut app.tab, state);
}

/// A tab's name and the saved query it belongs to, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabMeta {
    pub name: String,
    pub saved_id: Option<String>,
    /// The tab's connection (an id into the app's connections).
    pub conn: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabPromptKind {
    /// Save the active tab to the library as "group/name".
    SaveAs,
    /// Rename the active tab (and its saved query, if any).
    Rename,
    /// Rename a saved query from the library window.
    RenameSaved(String),
}

pub struct TabPrompt {
    pub kind: TabPromptKind,
    pub text: LineInput,
}

/// One row of the library window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryItem {
    Saved(String),
    /// An AI agent's query, by its index in [`App::agent_log`].
    Agent(usize),
    History(String),
}

/// A query an AI agent ran through `podelis mcp`, and what's worth a look
/// about it (refused, failed, a dry run, rows changed, a lint warning).
#[derive(Clone, Debug)]
pub struct AgentQuery {
    pub entry: crate::agents::Entry,
    /// Refused, failed, a dry run or rows changed.
    pub flag: Option<String>,
    /// The SQL's first lint warning, worked out the first time the row is
    /// shown: a big log has thousands of entries, the library shows 200.
    lint: std::cell::OnceCell<Option<String>>,
    /// The engine to read its SQL by.
    engine: crate::db::Engine,
}

impl AgentQuery {
    fn new(entry: crate::agents::Entry, engine: crate::db::Engine) -> Self {
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

pub struct LibraryView {
    pub query: LineInput,
    pub selected: usize,
    /// Asking "delete this saved query?".
    pub confirm_delete: bool,
    /// Enter inserts the SQL at the cursor instead of opening it (Ctrl+E).
    pub insert: bool,
}

/// Numbers the tabs-file writes so the newest always wins.
#[derive(Default)]
pub struct Drafts {
    /// The last ticket handed out.
    next: std::sync::atomic::AtomicU64,
    /// The newest ticket written; held while writing.
    written: Arc<std::sync::Mutex<u64>>,
}

/// One write of the tabs file.
pub(super) struct DraftJob {
    path: PathBuf,
    /// The pre-tabs draft file, superseded by this one.
    old: Option<PathBuf>,
    file: TabsFile,
    ticket: u64,
    written: Arc<std::sync::Mutex<u64>>,
}

impl DraftJob {
    pub(super) fn write(self) {
        let Ok(mut written) = self.written.lock() else { return };
        if *written > self.ticket {
            return;
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.file) {
            let _ = store::write_atomic(&self.path, &json);
        }
        if let Some(old) = self.old {
            let _ = std::fs::remove_file(old);
        }
        *written = self.ticket;
    }
}

/// What's stored per project so tabs come back after a restart.
#[derive(Serialize, Deserialize)]
struct TabsFile {
    active: usize,
    tabs: Vec<SavedTab>,
    #[serde(default)]
    session: session::SessionUi,
}

#[derive(Serialize, Deserialize)]
struct SavedTab {
    name: String,
    sql: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_id: Option<String>,
    /// Name of the saved connection the tab used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection: Option<String>,
    /// Cursor (row, column) and first visible line.
    #[serde(default)]
    cursor: (usize, usize),
    #[serde(default)]
    scroll: usize,
}

impl App {
    pub(super) fn tabs_path(&self, project_id: &str) -> Option<PathBuf> {
        Some(self.data_dir.as_ref()?.join("drafts").join(format!("{project_id}.json")))
    }

    fn library_path(&self, project_id: &str) -> Option<PathBuf> {
        Some(self.data_dir.as_ref()?.join("queries").join(format!("{project_id}.json")))
    }

    /// Loads a project's tabs (or its pre-tabs draft) and saved queries.
    pub(super) fn load_tabs(&mut self, project_id: &str) {
        self.library = Library::load_from(self.library_path(project_id));
        let file: Option<TabsFile> = self
            .tabs_path(project_id)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok());
        let saved = match file {
            Some(f) if !f.tabs.is_empty() => f,
            _ => {
                // Before tabs there was one draft per project.
                let draft = self.draft_path(project_id).and_then(|p| std::fs::read_to_string(p).ok());
                TabsFile {
                    active: 0,
                    tabs: vec![SavedTab {
                        name: "Query 1".into(),
                        sql: draft.unwrap_or_default(),
                        saved_id: None,
                        connection: None,
                        cursor: (0, 0),
                        scroll: 0,
                    }],
                    session: session::SessionUi::default(),
                }
            }
        };
        let active = saved.active.min(saved.tabs.len() - 1);
        self.tabs.clear();
        self.parked.clear();
        self.conns.tab_connections.clear();
        self.conns.saved_session = saved.session;
        for (i, tab) in saved.tabs.into_iter().enumerate() {
            // Links to saved queries that were deleted elsewhere are dropped.
            let saved_id = tab.saved_id.filter(|id| self.library.get(id).is_some());
            self.tabs.push(TabMeta { name: tab.name, saved_id, conn: None });
            self.conns.tab_connections.push(tab.connection);
            let mut editor = TextEditor::with_text(&tab.sql);
            editor.restore_cursor(tab.cursor, tab.scroll);
            let state = TabState { editor, ..TabState::default() };
            if i == active {
                let mut state = state;
                swap_tab(self, &mut state);
                self.parked.push(None);
            } else {
                self.parked.push(Some(state));
            }
        }
        self.active_tab = active;
        // An open connection stays with the active tab.
        self.tabs[active].conn = self.conns.active;
        self.release_unused_conns();
    }

    /// Writes every tab of the open project, so they survive a restart.
    pub fn save_draft(&self) {
        if let Some(job) = self.draft_job() {
            job.write();
        }
    }

    /// Saves the tabs on a background thread: after every typing pause, and
    /// writing a long script took a while. Where there is no runtime (some
    /// tests), right away.
    pub(super) fn save_draft_in_background(&self) {
        let Some(job) = self.draft_job() else { return };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(move || job.write())),
            Err(_) => job.write(),
        }
    }

    /// The tabs file to write, captured now (with a ticket so an older write
    /// that finishes late can't replace a newer one).
    pub(super) fn draft_job(&self) -> Option<DraftJob> {
        let id = self.current_project.as_deref()?;
        let path = self.tabs_path(id)?;
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, meta)| {
                let editor = if i == self.active_tab {
                    Some(&self.tab.editor)
                } else {
                    self.parked[i].as_ref().map(|s| &s.editor)
                };
                SavedTab {
                    name: meta.name.clone(),
                    sql: self.tab_text(i),
                    saved_id: meta.saved_id.clone(),
                    connection: self.tab_connection_name(i),
                    cursor: editor.map_or((0, 0), |e| (e.cursor_row, e.cursor_col)),
                    scroll: editor.map_or(0, |e| e.scroll_row),
                }
            })
            .collect();
        let file = TabsFile { active: self.active_tab, tabs, session: self.session_ui() };
        let ticket = self.drafts.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        Some(DraftJob { path, old: self.draft_path(id), file, ticket, written: Arc::clone(&self.drafts.written) })
    }

    pub fn tab_text(&self, i: usize) -> String {
        if i == self.active_tab {
            self.tab.editor.text()
        } else {
            self.parked.get(i).and_then(Option::as_ref).map(|s| s.editor.text()).unwrap_or_default()
        }
    }

    /// Unsaved changes: differs from its saved query, or (never saved) not empty.
    pub fn tab_dirty(&self, i: usize) -> bool {
        let text = self.tab_text(i);
        match self.tabs.get(i).and_then(|t| t.saved_id.as_ref()).and_then(|id| self.library.get(id)) {
            Some(saved) => saved.sql != text,
            None => !text.trim().is_empty(),
        }
    }

    pub(super) fn switch_tab(&mut self, i: usize) {
        if i == self.active_tab || i >= self.tabs.len() {
            return;
        }
        if self.executing {
            return self.set_status("Wait for the query to finish (or press Esc) before switching tabs".into(), true);
        }
        // An open stream would hold the session while its tab is hidden.
        self.row_stream = None;
        self.fetching_more = false;
        self.completion = None;
        self.cell_view = None;
        self.cell_edit = None;
        let mut incoming = self.parked[i].take().unwrap_or_default();
        swap_tab(self, &mut incoming);
        self.parked[self.active_tab] = Some(incoming);
        self.active_tab = i;
        self.focus = Focus::Editor;
        self.switch_conn(self.tabs[i].conn);
    }

    /// "Query N" with the smallest N not in use (one of the first
    /// `tabs.len() + 1` numbers is always free).
    fn unused_tab_name(&self) -> String {
        (1..=self.tabs.len() + 1)
            .map(|n| format!("Query {n}"))
            .find(|name| self.tabs.iter().all(|t| t.name != *name))
            .unwrap_or_else(|| "Query".to_string())
    }

    /// Opens a new tab with `sql` and switches to it.
    pub(super) fn new_tab(&mut self, name: Option<String>, sql: &str, saved_id: Option<String>) {
        if self.executing {
            return self.set_status("Wait for the query to finish (or press Esc) before opening a tab".into(), true);
        }
        let name = name.unwrap_or_else(|| self.unused_tab_name());
        // A new tab shares the current tab's connection.
        self.tabs.push(TabMeta { name, saved_id, conn: self.conns.active });
        self.parked.push(Some(TabState { editor: TextEditor::with_text(sql), ..TabState::default() }));
        self.switch_tab(self.tabs.len() - 1);
        self.save_draft();
    }

    /// Opens a new tab with `sql` on connection `conn` (another tab's), so
    /// SQL meant for that connection runs there.
    pub(super) fn open_tab_on(&mut self, conn: u64, sql: &str) {
        if self.executing {
            return self.set_status("Wait for the query to finish (or press Esc) before opening a tab".into(), true);
        }
        let name = self.unused_tab_name();
        self.tabs.push(TabMeta { name, saved_id: None, conn: Some(conn) });
        self.parked.push(Some(TabState { editor: TextEditor::with_text(sql), ..TabState::default() }));
        self.switch_tab(self.tabs.len() - 1);
        self.save_draft();
        self.set_status("Opened on the other connection: review it, then run it there".into(), false);
    }

    /// Closes the active tab, asking first if it has unsaved changes.
    pub(super) fn request_close_tab(&mut self) {
        if self.tab_dirty(self.active_tab) {
            self.confirm = Some(Confirm {
                message: format!("Close '{}'? Its unsaved changes will be lost.", self.tabs[self.active_tab].name),
                details: vec!["Ctrl+S saves it to the library first.".into()],
                danger: false,
                action: ConfirmAction::CloseTab,
                offer_transaction: false,
            });
        } else {
            self.close_tab();
        }
    }

    pub(super) fn close_tab(&mut self) {
        if self.executing {
            return self.set_status("Wait for the query to finish before closing the tab".into(), true);
        }
        let closing = self.active_tab;
        if self.tabs.len() == 1 {
            // Keep one (empty) tab.
            self.tabs[0] = TabMeta { name: "Query 1".into(), saved_id: None, conn: self.tabs[0].conn };
            let mut fresh = TabState::default();
            swap_tab(self, &mut fresh);
            self.row_stream = None;
            return;
        }
        let next = if closing + 1 < self.tabs.len() { closing + 1 } else { closing - 1 };
        self.switch_tab(next);
        self.tabs.remove(closing);
        self.parked.remove(closing);
        if self.active_tab > closing {
            self.active_tab -= 1;
        }
        self.release_unused_conns();
        self.save_draft();
    }

    /// Ctrl+S: update the tab's saved query, or ask for a name.
    pub(super) fn save_tab(&mut self) {
        let text = self.tab.editor.text();
        let Some(id) = self.tabs[self.active_tab].saved_id.clone() else {
            let name = self.tabs[self.active_tab].name.clone();
            let text = if name.starts_with("Query ") { String::new() } else { name };
            self.tab_prompt = Some(TabPrompt { kind: TabPromptKind::SaveAs, text: LineInput::new(text) });
            return;
        };
        match self.library.update_sql(&id, &text) {
            Ok(()) => {
                let path = self.library.get(&id).map(library::SavedQuery::path).unwrap_or_default();
                self.set_status(format!("Saved '{path}'"), false);
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// F4: rename the tab (and its saved query, as "group/name").
    pub(super) fn rename_tab(&mut self) {
        let meta = &self.tabs[self.active_tab];
        let text = meta
            .saved_id
            .as_ref()
            .and_then(|id| self.library.get(id))
            .map_or_else(|| meta.name.clone(), library::SavedQuery::path);
        self.tab_prompt = Some(TabPrompt { kind: TabPromptKind::Rename, text: LineInput::new(text) });
    }

    pub(super) fn handle_tab_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.tab_prompt else { return };
        match prompt.text.handle(key) {
            InputResult::Cancel => self.tab_prompt = None,
            InputResult::Submit => {
                let Some(prompt) = self.tab_prompt.take() else { return };
                if let Err(e) = self.submit_tab_prompt(&prompt) {
                    self.set_status(e, true);
                    self.tab_prompt = Some(prompt);
                } else {
                    self.save_draft();
                }
            }
            InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
        }
    }

    fn submit_tab_prompt(&mut self, prompt: &TabPrompt) -> Result<(), String> {
        let text = prompt.text.text().trim();
        match &prompt.kind {
            TabPromptKind::SaveAs => {
                let id = self.library.add(text, &self.tab.editor.text())?;
                let (_, name) = library::split_path(text);
                self.tabs[self.active_tab] = TabMeta { name, saved_id: Some(id), ..self.tabs[self.active_tab].clone() };
                self.set_status(format!("Saved '{text}' to the library (F3 to search)"), false);
            }
            TabPromptKind::Rename => match self.tabs[self.active_tab].saved_id.clone() {
                Some(id) => {
                    self.library.rename(&id, text)?;
                    self.tabs[self.active_tab].name = library::split_path(text).1;
                }
                None if text.is_empty() => return Err("Give the tab a name".into()),
                None => self.tabs[self.active_tab].name = text.to_string(),
            },
            TabPromptKind::RenameSaved(id) => {
                self.library.rename(id, text)?;
                let name = library::split_path(text).1;
                for tab in self.tabs.iter_mut().filter(|t| t.saved_id.as_deref() == Some(id)) {
                    tab.name.clone_from(&name);
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------- library

    pub(super) fn open_library(&mut self) {
        self.load_agent_log();
        self.library_view =
            Some(LibraryView { query: LineInput::default(), selected: 0, confirm_delete: false, insert: false });
    }

    /// Ctrl+E: the library, with Enter inserting a saved query's SQL (or a
    /// history entry) at the cursor. Starts with the snippets group.
    pub(super) fn open_snippets(&mut self) {
        self.load_agent_log();
        self.library_view = Some(LibraryView {
            query: LineInput::new(library::SNIPPETS),
            selected: 0,
            confirm_delete: false,
            insert: true,
        });
        self.focus = Focus::Editor;
    }

    /// Reads what AI agents ran in this project (`podelis mcp`), newest
    /// first: again only when the file has changed since, since a big log
    /// takes a moment to read and check.
    fn load_agent_log(&mut self) {
        let engine = self.engine();
        let path =
            self.current_project.as_deref().and_then(|id| Some(crate::agents::log_path(self.data_dir.as_ref()?, id)));
        let stamp = path.as_ref().and_then(|p| {
            let meta = std::fs::metadata(p).ok()?;
            Some((p.clone(), meta.modified().ok()?, meta.len()))
        });
        if stamp.is_some() && stamp == self.agent_log_stamp {
            return;
        }
        self.agent_log = path
            .map(|p| crate::agents::read(&p))
            .unwrap_or_default()
            .into_iter()
            .rev()
            .map(|entry| AgentQuery::new(entry, engine))
            .collect();
        self.agent_log_stamp = stamp;
    }

    /// Rows of the library window for its search text: matching saved
    /// queries, then what AI agents ran (newest first; the agent's or the
    /// connection's name matches too), then matching history (newest
    /// first, without duplicates).
    pub fn library_items(&self) -> Vec<LibraryItem> {
        let query = self.library_view.as_ref().map_or("", |v| v.query.text());
        let mut items: Vec<LibraryItem> =
            self.library.search(query).into_iter().map(|q| LibraryItem::Saved(q.id.clone())).collect();
        // Each part has its own limit, so the agents' queries don't push
        // your history out; every entry is searched, only matches count.
        let mut by_agents = 0;
        for (i, agent) in self.agent_log.iter().enumerate() {
            if by_agents >= 200 {
                break;
            }
            let e = &agent.entry;
            let flag = agent.flag.as_deref().unwrap_or_default();
            if library::matches(query, &[&e.sql, &e.agent, &e.connection, flag]) {
                items.push(LibraryItem::Agent(i));
                by_agents += 1;
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut from_history = 0;
        for sql in self.history.entries().iter().rev() {
            if from_history >= 300 {
                break;
            }
            if library::matches(query, &[sql]) && seen.insert(sql.as_str()) {
                items.push(LibraryItem::History(sql.clone()));
                from_history += 1;
            }
        }
        items
    }

    pub(super) fn handle_library_key(&mut self, key: KeyEvent) {
        let items = self.library_items();
        let Some(view) = &mut self.library_view else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if view.confirm_delete {
            view.confirm_delete = false;
            if matches!(key.code, KeyCode::Char('y' | 'Y'))
                && let Some(LibraryItem::Saved(id)) = items.get(view.selected)
            {
                let id = id.clone();
                if let Err(e) = self.library.delete(&id) {
                    self.set_status(e, true);
                }
                for tab in self.tabs.iter_mut().filter(|t| t.saved_id.as_deref() == Some(id.as_str())) {
                    tab.saved_id = None;
                }
            }
            return;
        }
        if let Some(i) = list::step(view.selected, items.len(), key, Nav::SEARCHED) {
            view.selected = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.library_view = None,
            KeyCode::Char('d') if ctrl => {
                if matches!(items.get(view.selected), Some(LibraryItem::Saved(_))) {
                    view.confirm_delete = true;
                }
            }
            KeyCode::F(4) => {
                if let Some(LibraryItem::Saved(id)) = items.get(view.selected) {
                    let text = self.library.get(id).map(library::SavedQuery::path).unwrap_or_default();
                    self.tab_prompt =
                        Some(TabPrompt { kind: TabPromptKind::RenameSaved(id.clone()), text: LineInput::new(text) });
                }
            }
            KeyCode::Enter => {
                let Some(item) = items.get(view.selected).cloned() else { return };
                let insert = view.insert;
                self.library_view = None;
                if insert {
                    self.insert_library_item(&item);
                } else {
                    self.open_library_item(&item);
                }
            }
            _ => {
                if view.query.handle(key) == InputResult::Changed {
                    view.selected = 0;
                }
            }
        }
    }

    /// Inserts a saved query's SQL (or a history entry) at the cursor.
    fn insert_library_item(&mut self, item: &LibraryItem) {
        let sql = match item {
            LibraryItem::Saved(id) => match self.library.get(id) {
                Some(q) => q.sql.clone(),
                None => return,
            },
            LibraryItem::History(sql) => sql.clone(),
            LibraryItem::Agent(i) => match self.agent_log.get(*i) {
                Some(a) => a.entry.sql.clone(),
                None => return,
            },
        };
        self.tab.editor.insert_str(sql.trim_end());
        self.edited();
        self.focus = Focus::Editor;
    }

    /// Opens a saved query (or history entry): in the current tab if it's
    /// empty and unsaved, otherwise in a new tab. A saved query that's
    /// already open is switched to instead.
    fn open_library_item(&mut self, item: &LibraryItem) {
        let (name, sql, saved_id) = match item {
            LibraryItem::Saved(id) => {
                if let Some(i) = self.tabs.iter().position(|t| t.saved_id.as_ref() == Some(id)) {
                    return self.switch_tab(i);
                }
                let Some(q) = self.library.get(id) else { return };
                (Some(q.name.clone()), q.sql.clone(), Some(id.clone()))
            }
            LibraryItem::History(sql) => (None, sql.clone(), None),
            LibraryItem::Agent(i) => {
                let Some(a) = self.agent_log.get(*i) else { return };
                // Named for the agent alone: the tab runs it on its own
                // connection, which may not be where the agent did.
                let (agent, ran_on) = (a.entry.agent.clone(), a.entry.connection.clone());
                let sql = a.entry.sql.clone();
                self.open_sql_in_tab(Some(agent.clone()), &sql, None);
                return self
                    .set_status(format!("{agent} ran this on {ran_on}; here it runs on this tab's connection"), false);
            }
        };
        self.open_sql_in_tab(name, &sql, saved_id);
    }

    /// Puts `sql` in the current tab if it's empty and unsaved, otherwise
    /// in a new tab.
    fn open_sql_in_tab(&mut self, name: Option<String>, sql: &str, saved_id: Option<String>) {
        let current_blank = self.tab.editor.is_empty() && self.tabs[self.active_tab].saved_id.is_none();
        if current_blank && !self.executing {
            let name = name.unwrap_or_else(|| self.tabs[self.active_tab].name.clone());
            self.tabs[self.active_tab] = TabMeta { name, saved_id, ..self.tabs[self.active_tab].clone() };
            self.tab.editor.set_text(sql);
            self.edited();
        } else {
            self.new_tab(name, sql, saved_id);
        }
        self.focus = Focus::Editor;
    }
}
