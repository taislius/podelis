//! Query tabs, the saved-query library window (F3) and their prompts.
//!
//! The active tab's state is `App::tab`; inactive tabs are "parked" as a
//! [`TabState`] and swapped in on switching.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::*;
use crate::config::library::{self, Library};

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
    /// Lines of the plan's text selected (where it started, where it is
    /// now; from 0): kept by line, so scrolling keeps it whole.
    pub plan_selected: Option<(usize, usize)>,
    pub tab_hscroll: u16,
    /// A plan that's a table (MySQL's classic EXPLAIN) as a grid, so the
    /// results' keys work on it. While QUERY PLAN shows it, it's swapped
    /// into `grid` and this holds the result's grid.
    pub plan_grid: Option<Box<ResultGrid>>,
    pub plan_swapped: bool,
    /// The results kept from its runs, oldest first (`runs.rs`); the one
    /// shown is `run_at`, and how many have been made numbers the next.
    pub runs: Vec<super::runs::KeptRun>,
    pub run_at: usize,
    pub(super) runs_made: usize,
    /// Why the run shown has no result: its error.
    pub run_error: Option<String>,
    /// The parameters the shown result's query ran with, to run it again.
    pub(super) last_filled: Option<Filled>,
    /// Its query: running, or the last one's state. Each tab has its own,
    /// so tabs on different connections run at the same time.
    pub run: Run,
}

/// The statement a plan or a result was made for.
#[derive(Clone, Debug)]
pub struct PlanSource {
    pub sql: String,
    /// EXPLAIN ANALYZE (it ran) rather than EXPLAIN.
    pub analyzed: bool,
    /// The editor line it started on then (from 0).
    pub line: usize,
    pub at: chrono::DateTime<chrono::Local>,
    /// `find_in` for an editor version, drawn every frame.
    found: std::cell::Cell<Option<(u64, Found)>>,
    /// A script's statements, each a source of its own: split once.
    parts: std::cell::OnceCell<Vec<PlanSource>>,
    /// The engine it ran on, which says how it splits (`None`: unknown).
    engine: Option<Engine>,
}

/// Where a statement is in the text: a byte offset and a line.
type Found = Option<(usize, usize)>;

impl PlanSource {
    /// For `sql`, on editor line `line`, made now.
    pub fn new(sql: String, analyzed: bool, line: usize) -> Self {
        PlanSource {
            sql,
            analyzed,
            line,
            at: chrono::Local::now(),
            found: std::cell::Cell::default(),
            parts: std::cell::OnceCell::new(),
            engine: None,
        }
    }

    /// Written for `engine`: its statements can be told apart (see
    /// [`Self::statement`]).
    pub fn on(self, engine: Engine) -> Self {
        PlanSource { engine: Some(engine), ..self }
    }

    /// Statement `n` (from 1) of this script, which ran as `of` statements:
    /// what a script's result set was made by. `None` for one statement, or
    /// when the script splits otherwise than it ran (a `CALL`'s extra
    /// result), so `n` can't be trusted to point into it.
    pub fn statement(&self, n: usize, of: usize) -> Option<&PlanSource> {
        let engine = self.engine?;
        let parts = self.parts.get_or_init(|| {
            // Where each is in the script, not a search (a repeat would fool it).
            crate::sql::statement_ranges(&self.sql, engine)
                .into_iter()
                .map(|range| {
                    let line = self.line + self.sql[..range.start].matches('\n').count();
                    let sql = self.sql[range].to_string();
                    PlanSource { at: self.at, ..PlanSource::new(sql, self.analyzed, line).on(engine) }
                })
                .collect()
        });
        if parts.len() < 2 || parts.len() != of || n == 0 {
            return None;
        }
        parts.get(n - 1)
    }

    /// `find_in` the editor's text, worked out again only once it changed.
    pub fn find_in_editor(&self, editor: &TextEditor) -> Option<(usize, usize)> {
        if let Some((version, found)) = self.found.get()
            && version == editor.version()
        {
            return found;
        }
        let found = self.find_in(&editor.text());
        self.found.set(Some((editor.version(), found)));
        found
    }

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
    /// Save this SQL (a statement or the selection) to the library as a
    /// query of its own: the tab stays as it is.
    SaveStatement(String),
}

pub struct TabPrompt {
    pub kind: TabPromptKind,
    pub text: LineInput,
}

/// One row of the library window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryItem {
    Saved(String),
    /// A history entry, and when it ran (if that's known).
    History(String, Option<i64>),
}

pub struct LibraryView {
    pub query: LineInput,
    pub selected: usize,
    /// Asking "delete this saved query?".
    pub confirm_delete: bool,
    /// Enter inserts the SQL at the cursor instead of opening it (Ctrl+E).
    pub insert: bool,
    /// The first row in sight (kept from frame to frame).
    pub offset: usize,
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
                        name: default_tab_name(1),
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

    /// Does `visit` as if tab `i` were shown (its editor, results and
    /// connection), then shows the one that was: for a run's outcome
    /// arriving while another tab is shown.
    pub(super) fn in_tab(&mut self, i: usize, visit: impl FnOnce(&mut Self)) {
        if i == self.active_tab {
            return visit(self);
        }
        let Some(mut parked) = self.parked.get_mut(i).and_then(Option::take) else { return };
        let (shown, shown_conn, focus) = (self.active_tab, self.conns.active, self.focus);
        swap_tab(self, &mut parked);
        self.active_tab = i;
        self.swap_conns(self.tabs[i].conn);
        visit(self);
        self.close_stream();
        self.swap_conns(shown_conn);
        self.active_tab = shown;
        swap_tab(self, &mut parked);
        self.parked[i] = Some(parked);
        self.focus = focus;
    }

    /// Closes an open stream (which holds the session) as its tab is
    /// hidden; the rows left can be loaded again with `M`.
    pub(super) fn close_stream(&mut self) {
        if self.tab.run.stream.is_some() {
            self.tab.grid.cut_short = true;
        }
        self.tab.run.set_stream(None);
        self.tab.run.fetching_more = false;
    }

    pub(super) fn switch_tab(&mut self, i: usize) {
        if i == self.active_tab || i >= self.tabs.len() {
            return;
        }
        if self.blocks_tabs() {
            return self.set_status(t!("tabs.wait_switch").into(), true);
        }
        self.stop_watch(&format!(": {}", t!("tabs.another_tab")));
        // An open stream would hold the session while its tab is hidden.
        self.close_stream();
        self.completion = None;
        self.cell_view = None;
        self.cell_edit = None;
        let mut incoming = self.parked[i].take().unwrap_or_default();
        swap_tab(self, &mut incoming);
        self.parked[self.active_tab] = Some(incoming);
        self.active_tab = i;
        self.focus = Focus::Editor;
        self.switch_conn(self.tabs[i].conn);
        // A query running in one tab: said in its results, else in the bar.
        if self.running_here() {
            self.set_result_status(t!("tabs.running").to_string(), false);
        } else if self.busy_conn_tab().is_some() {
            self.notes.status = self.running_elsewhere(t!("tabs.outcome_here"));
            (self.notes.status_is_error, self.notes.status_in_results) = (false, false);
        }
    }

    /// "Query N" with the smallest N not in use (one of the first
    /// `tabs.len() + 1` numbers is always free).
    fn unused_tab_name(&self) -> String {
        (1..=self.tabs.len() + 1)
            .map(default_tab_name)
            .find(|name| self.tabs.iter().all(|t| t.name != *name))
            .unwrap_or_else(|| default_tab_name(0))
    }

    /// Opens a new tab with `sql` and switches to it.
    pub(super) fn new_tab(&mut self, name: Option<String>, sql: &str, saved_id: Option<String>) {
        if self.blocks_tabs() {
            return self.set_status(t!("tabs.wait_open").into(), true);
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
        if self.blocks_tabs() {
            return self.set_status(t!("tabs.wait_open").into(), true);
        }
        let name = self.unused_tab_name();
        self.tabs.push(TabMeta { name, saved_id: None, conn: Some(conn) });
        self.parked.push(Some(TabState { editor: TextEditor::with_text(sql), ..TabState::default() }));
        self.switch_tab(self.tabs.len() - 1);
        self.save_draft();
        self.set_status(t!("tabs.opened_other").into(), false);
    }

    /// Closes the active tab, asking first if it has unsaved changes.
    pub(super) fn request_close_tab(&mut self) {
        if self.tab_dirty(self.active_tab) {
            self.confirm = Some(Confirm {
                message: t!("tabs.close_unsaved", name = self.tabs[self.active_tab].name),
                details: vec![t!("tabs.save_first").into()],
                danger: false,
                action: ConfirmAction::CloseTab,
                offer_transaction: false,
            });
        } else {
            self.close_tab();
        }
    }

    pub(super) fn close_tab(&mut self) {
        if self.tab.run.executing {
            return self.set_status(t!("tabs.wait_close").into(), true);
        }
        let closing = self.active_tab;
        // A question asked from this tab stops; one from a later tab follows
        // its place.
        self.tab_closing(closing);
        if self.tabs.len() == 1 {
            // Keep one (empty) tab.
            self.tabs[0] = TabMeta { name: default_tab_name(1), saved_id: None, conn: self.tabs[0].conn };
            let mut fresh = TabState::default();
            swap_tab(self, &mut fresh);
            self.tab.run.set_stream(None);
            // Its text goes from the tabs file too: it mustn't come back
            // on the next start.
            self.save_draft();
            return;
        }
        let next = if closing + 1 < self.tabs.len() { closing + 1 } else { closing - 1 };
        self.switch_tab(next);
        self.tabs.remove(closing);
        self.parked.remove(closing);
        // Each tab's connection from the last session, kept in step.
        if closing < self.conns.tab_connections.len() {
            self.conns.tab_connections.remove(closing);
        }
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
            let text = if is_default_tab_name(&name) { String::new() } else { name };
            self.tab_prompt = Some(TabPrompt { kind: TabPromptKind::SaveAs, text: LineInput::new(text) });
            return;
        };
        match self.library.update_sql(&id, &text) {
            Ok(()) => {
                let path = self.library.get(&id).map(library::SavedQuery::path).unwrap_or_default();
                self.set_status(t!("tabs.saved", path = path), false);
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// The selected text, else the statement at the cursor, trimmed.
    pub(super) fn statement_or_selection(&self) -> Option<String> {
        if let Some(selected) = self.tab.editor.selected_text().filter(|s| !s.trim().is_empty()) {
            return Some(selected.trim().to_string());
        }
        let text = self.tab.editor.text();
        let range = sql::statement_range(&text, self.tab.editor.cursor_offset(), self.engine())?;
        Some(text[range].trim().to_string())
    }

    /// The statement at the cursor (or the selection) into the library,
    /// under a name asked for; the tab isn't tied to it.
    pub(super) fn save_statement(&mut self) {
        let Some(sql) = self.statement_or_selection() else {
            return self.set_status(t!("editor.no_statement_here").into(), true);
        };
        self.tab_prompt = Some(TabPrompt { kind: TabPromptKind::SaveStatement(sql), text: LineInput::default() });
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
                self.set_status(t!("tabs.saved_to_library", name = text), false);
            }
            TabPromptKind::Rename => match self.tabs[self.active_tab].saved_id.clone() {
                Some(id) => {
                    self.library.rename(&id, text)?;
                    self.tabs[self.active_tab].name = library::split_path(text).1;
                }
                None if text.is_empty() => return Err(t!("tabs.need_name").into()),
                None => self.tabs[self.active_tab].name = text.to_string(),
            },
            TabPromptKind::SaveStatement(sql) => {
                self.library.add(text, sql)?;
                self.set_status(t!("tabs.saved_to_library", name = text), false);
            }
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
        self.library_view = Some(LibraryView {
            query: LineInput::default(),
            selected: 0,
            confirm_delete: false,
            insert: false,
            offset: 0,
        });
    }

    /// Ctrl+E: the library, with Enter inserting a saved query's SQL (or a
    /// history entry) at the cursor. Starts with the snippets group, if
    /// there is one: else with everything (a search for a group that isn't
    /// there showed nothing to pick until it was cleared).
    pub(super) fn open_snippets(&mut self) {
        let snippets = self.library.queries.iter().any(|q| q.group.eq_ignore_ascii_case(library::SNIPPETS));
        self.library_view = Some(LibraryView {
            query: LineInput::new(if snippets { library::SNIPPETS } else { "" }),
            selected: 0,
            confirm_delete: false,
            insert: true,
            offset: 0,
        });
        self.focus = Focus::Editor;
    }

    /// Rows of the library window for its search text: matching saved
    /// queries, then matching history (newest first, without duplicates).
    pub fn library_items(&self) -> Vec<LibraryItem> {
        let query = self.library_view.as_ref().map_or("", |v| v.query.text());
        let (words, when) = library::split_dates(query, chrono::Local::now());
        let mut items: Vec<LibraryItem> =
            self.library.search_in(&words, when).into_iter().map(|q| LibraryItem::Saved(q.id.clone())).collect();
        let mut seen = std::collections::HashSet::new();
        let mut from_history = 0;
        for (i, sql) in self.history.entries().iter().enumerate().rev() {
            if from_history >= 300 {
                break;
            }
            let in_span = when.is_none_or(|w| w.holds(self.history.time(i)));
            if in_span && library::matches(&words, &[sql]) && seen.insert(sql.as_str()) {
                items.push(LibraryItem::History(sql.clone(), self.history.time(i)));
                from_history += 1;
            }
        }
        items
    }

    /// A library row's SQL.
    pub fn library_item_sql(&self, item: &LibraryItem) -> Option<String> {
        match item {
            LibraryItem::Saved(id) => self.library.get(id).map(|q| q.sql.clone()),
            LibraryItem::History(sql, _) => Some(sql.clone()),
        }
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
            LibraryItem::History(sql, _) => sql.clone(),
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
            LibraryItem::History(sql, _) => (None, sql.clone(), None),
        };
        self.open_sql_in_tab(name, &sql, saved_id);
    }

    /// Puts `sql` in the current tab if it's empty and unsaved, otherwise
    /// in a new tab.
    pub(super) fn open_sql_in_tab(&mut self, name: Option<String>, sql: &str, saved_id: Option<String>) {
        let current_blank = self.tab.editor.is_empty() && self.tabs[self.active_tab].saved_id.is_none();
        if current_blank && !self.running_here() {
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

/// A new tab's name: "Query 1", in the user's language.
pub(super) fn default_tab_name(n: usize) -> String {
    t!("tabs.default_name", n = n)
}

/// Whether `name` is one [`default_tab_name`] gives, in any language (a
/// tab made before the language changed): nothing to keep when it's saved
/// under a name of its own.
fn is_default_tab_name(name: &str) -> bool {
    crate::lang::in_every_language("tabs.default_name").into_iter().any(|template| {
        let (before, after) = template.split_once("{n}").unwrap_or((template, ""));
        name.strip_prefix(before)
            .and_then(|rest| rest.strip_suffix(after))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_tab_names_in_any_language() {
        for name in ["Query 1", "Query 12", "Užklausa 3"] {
            assert!(super::is_default_tab_name(name), "{name}");
        }
        for name in ["Query", "Query one", "Queries 1", "monthly report 1"] {
            assert!(!super::is_default_tab_name(name), "{name}");
        }
    }
}
