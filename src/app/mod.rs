use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthStr;

use crate::config::history::{self, History};
use crate::config::settings::{EditorMode, Settings};
use crate::config::store::{self, ConnectionProfile, ConnectionStore, Project};
use crate::db::{
    CheckOutcome, ColumnInfo, ConnectionMode, Database, Engine, ErrorPos, Page, QueryOutcome, RoutineInfo, RowStream,
    TableDetails, TableInfo, TxEnd, TxnState,
};
use crate::editor::{Pos, TextEditor};
use crate::sql;
use crate::sql::analyze::{self, HintSource, Kind, StatementInfo, SyntaxHint, TxnOp};
use crate::sql::complete::{self, Candidate, TableRef};
use crate::sql::params::Filled;
use crate::sql::parse::{ParsedScript, parse};
pub use agents::{AgentQuery, Agents, short_dir, short_time};
pub use find::FindBar;
pub use grid::ResultGrid;
pub use hits::Hits;
pub use input::{InputResult, LineInput};
use list::Nav;
pub use overlay::Overlay;
pub use params::ParamForm;
pub use sandbox::Sandbox;
pub use tabs::{LibraryItem, LibraryView, TabMeta, TabPrompt, TabPromptKind};
pub use tabs::{PlanSource, TabState};
use vim::Vim;
pub use vim::VimMode;

mod activity;
mod agents;
pub mod ask;
pub mod commands;
mod compare;
mod confirm;
mod connections;
mod edit;
mod events;
mod find;
mod follow;
mod grid;
mod hits;
mod info;
mod input;
mod list;
pub mod memory_view;
mod menu;
mod messages;
mod mouse;
mod overlay;
pub mod palette;
mod panes;
mod params;
mod pipes;
mod plan_lines;
mod projects;
mod query;
mod results;
mod review;
mod rows;
mod runs;
mod sandbox;
mod schema;
mod session;
pub mod settings_view;
mod tabs;
#[cfg(test)]
mod tests;
mod vim;
mod watch;

use crate::config::draft::{ConnectionDraft, looks_like_url};
pub use confirm::{Confirm, ConfirmAction};
pub use connections::{ConnectField, ConnectForm, fields_for};
use events::{Background, DbEventKind};
pub use events::{DbEvent, Ping};
pub use query::{Dest, Format, Run, RunPurpose};
pub use results::{CellEdit, CellView};
pub use results::{NULL_TEXT, display_prefix, format_elapsed, visible_columns};
use results::{column_widths, compute_view, one_line, pretty_value, widen_columns};
pub use review::TxMode;
pub use schema::{SchemaNode, SchemaPanel, SchemaSection, SchemaSort, SchemaTable};
#[cfg(test)]
pub use session::Splash;

/// What has the keys. Projects, Connections and Connect are the three
/// panes of one screen for managing projects and connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Connections,
    Connect,
    Query,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Results,
    Schema,
}

/// The database containers found by `c` in the connections, to pick one.
#[derive(Default)]
pub struct ContainerPicker {
    /// Which search this is: a result for an earlier, closed one is dropped.
    pub request: u64,
    pub loading: bool,
    pub found: Vec<crate::system::containers::Container>,
    pub selected: usize,
    pub error: Option<String>,
    /// About the selected container (why it can't be used), until the
    /// selection moves.
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    NewProject,
    /// Renaming the project with this id.
    RenameProject(String),
}

/// A one-line text input shown on the projects screen.
pub struct Prompt {
    pub kind: PromptKind,
    pub text: LineInput,
}

/// Upper bound for "load more rows".
const MAX_ROW_LIMIT: usize = 1_000_000;

/// How long typing must pause before the syntax hint is recomputed.
const LINT_DELAY: Duration = Duration::from_millis(700);
/// Keys this soon after an agent's write is put to the user don't answer it.
const AGENT_SETTLE: Duration = Duration::from_millis(800);

/// Which view the results pane shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResultTab {
    #[default]
    Rows,
    Messages,
    Plan,
}

/// A line in the results pane's MESSAGES tab.
pub struct Message {
    pub time: String,
    pub text: String,
    pub is_error: bool,
}

/// Outcome of checking the editor text with the server.
pub enum ServerCheck {
    /// The session stayed busy; fall back to the local parser.
    Busy,
    /// Checked: the first problem, if any, and whether it's a syntax error
    /// or unknown function (maybe a feature the server lacks).
    Done(Option<(SyntaxHint, bool)>),
}

/// The autocomplete popup.
pub struct Completion {
    pub items: Vec<Candidate>,
    pub selected: usize,
    /// Chars before the cursor that the chosen item replaces.
    pub prefix_len: usize,
}

pub struct App {
    /// The popups that can open over the screens.
    pub popups: overlay::Popups,
    /// `Ctrl+K q`'s question being answered, and what goes with it.
    pub ask: ask::AskState,
    /// What's said about the run starting now.
    next_run: runs::NextRun,
    /// Keeping the connection alive.
    pub keepalive: session::KeepAlive,
    /// What's said to the user.
    pub notes: messages::Notes,
    /// The panes' layout.
    pub panes: panes::Panes,
    /// The projects and connections screen's state.
    pub manager: projects::Manager,
    /// The shown tab's connection; the other tabs' are parked (`Conns`).
    pub connection: session::Connection,
    /// What agents did and asked for (the agents view).
    pub agents: Agents,
    pub screen: Screen,
    pub store: ConnectionStore,
    /// Id of the open project.
    pub current_project: Option<String>,
    /// Config folder holding per-project history and editor drafts; `None`
    /// disables persistence (tests).
    data_dir: Option<PathBuf>,
    /// Where a sandbox's script goes once it's rolled back.
    sandbox_then: Option<sandbox::Then>,
    /// A sandbox's first step, waiting for the file's copy to be made.
    sandbox_waiting: Option<(String, usize, Option<Filled>, RunPurpose)>,
    /// The find/replace bar (Ctrl+F).
    pub find: Option<find::FindBar>,
    last_pipe: String,
    /// A viewer to run with the terminal (`o`), picked up by the main loop.
    external: Option<pipes::External>,
    /// When and where the editor last got a typed Space, for
    /// `leader_double_space`.
    last_space: Option<(Instant, Pos)>,
    /// Esc was the last key in the editor: Tab then moves to the next pane
    /// even right after a word.
    esc_then_tab: bool,
    /// Values typed for each parameter name, offered again next time.
    param_values: HashMap<String, String>,
    /// Tabs-file writes, so a late background one can't undo a newer one.
    drafts: tabs::Drafts,
    /// Tests that press random keys: nothing that reaches outside the app
    /// (connecting, `ssh`, shell commands).
    #[cfg(test)]
    pub(crate) sandbox: bool,
    /// Handling a key, click or paste (see `on_input`).
    handling_input: bool,
    lint_due: Option<Instant>,
    pub settings: Settings,
    pub keymap: crate::config::keys::KeyMap,
    history: History,
    pub focus: Focus,
    pub help_visible: bool,
    /// How far every key's window is scrolled.
    pub help_scroll: u16,
    /// Reopening the session's connections (Esc or any key hides it).
    pub splash: Option<session::Splash>,
    connect_started: Option<Instant>,
    /// The open cell-edit prompt.
    pub cell_edit: Option<CellEdit>,
    /// Vim state, used when `editor_mode = "vim"`.
    vim: Vim,
    /// Tabs' connections and the session being restored.
    conns: session::Conns,
    /// Open query tabs; the active one's state is in the fields above.
    pub tabs: Vec<TabMeta>,
    /// The active tab's editor, results and run.
    pub tab: TabState,
    /// Inactive tabs' state (`None` at `active_tab`).
    parked: Vec<Option<TabState>>,
    pub active_tab: usize,
    /// The project's saved queries.
    pub library: crate::config::library::Library,
    /// Watch mode: the last query run again every few seconds.
    pub watch: Option<watch::Watch>,
    /// "Export all rows" in progress: what stops it, and the file it writes.
    export_task: Option<(tokio::sync::oneshot::Sender<()>, PathBuf)>,
    /// The last run id handed out ([`App::next_run_id`]).
    run_ids: u64,
    /// While a hidden tab's event is applied (`apply_in_tab`): that tab,
    /// whose run ending is announced even with the terminal in front.
    hidden_tab: Option<usize>,
    /// A function's definition on its way is for the navigator's popup.
    routine_definition_pending: bool,
    /// Clickable areas from the last frame.
    pub hits: Hits,
    /// `f` is waiting for the result table's foreign keys to load.
    pending_follow: bool,
    pub should_quit: bool,
    clipboard: Option<String>,
    /// A long query ended: announced by the main loop (bell, notification).
    notification: Option<String>,
    /// Whether the terminal window has focus, once the terminal says.
    terminal_focused: Option<bool>,
    /// Bumped whenever in-flight work should be ignored (disconnect, cancelled
    /// connect). Every task's event carries the value it was started under.
    generation: u64,
    db_tx: mpsc::UnboundedSender<DbEvent>,
    db_rx: mpsc::UnboundedReceiver<DbEvent>,
}

/// What the status bar says about AI ([`App::ai_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiState {
    /// No model in `[[models]]`.
    Off,
    /// The default model; `asks_to_share` when it isn't on this machine and
    /// the project keeps its schemas to local models (asking asks first).
    On { model: String, asks_to_share: bool },
}

impl App {
    pub fn new(store: ConnectionStore, store_warning: Option<String>, data_dir: Option<PathBuf>) -> Self {
        let (db_tx, db_rx) = mpsc::unbounded_channel();
        let projects_selected =
            store.last_project.as_ref().and_then(|id| store.projects.iter().position(|p| &p.id == id)).unwrap_or(0);
        let projects_message = store_warning.map(|w| (w, true)).or_else(|| {
            store.migrated_project.as_ref().map(|_| (t!("projects.migrated", project = store::DEFAULT_PROJECT), false))
        });
        Self {
            popups: overlay::Popups::default(),
            ask: ask::AskState::default(),
            next_run: runs::NextRun::default(),
            keepalive: session::KeepAlive::default(),
            screen: Screen::Projects,
            store,
            current_project: None,
            history: History::load_from(None),
            data_dir,
            last_pipe: String::new(),
            external: None,
            last_space: None,
            esc_then_tab: false,
            find: None,
            sandbox_then: None,
            sandbox_waiting: None,
            param_values: HashMap::new(),
            drafts: tabs::Drafts::default(),
            #[cfg(test)]
            sandbox: false,
            handling_input: false,
            lint_due: None,
            settings: Settings::default(),
            keymap: crate::config::keys::KeyMap::default(),
            focus: Focus::Editor,
            help_visible: false,
            help_scroll: 0,
            splash: None,
            connect_started: None,
            cell_edit: None,
            vim: Vim::default(),
            conns: session::Conns::default(),
            tabs: vec![TabMeta { name: tabs::default_tab_name(1), saved_id: None, conn: None }],
            tab: TabState::default(),
            parked: vec![None],
            active_tab: 0,
            library: crate::config::library::Library::load_from(None),
            watch: None,
            export_task: None,
            run_ids: 0,
            hidden_tab: None,
            hits: Hits::default(),
            pending_follow: false,
            routine_definition_pending: false,
            should_quit: false,
            clipboard: None,
            notification: None,
            terminal_focused: None,
            generation: 0,
            notes: messages::Notes::default(),
            panes: panes::Panes { editor_percent: 40, editor_height: 1, ..panes::Panes::default() },
            manager: projects::Manager {
                project_selected: projects_selected,
                project_message: projects_message,
                ..projects::Manager::default()
            },
            connection: session::Connection {
                form: ConnectForm::new(),
                mode: ConnectionMode::Normal,
                tx_mode: TxMode::AutoCommit,
                txn: TxnState::Idle,
                ..session::Connection::default()
            },
            agents: Agents::default(),
            db_tx,
            db_rx,
        }
    }

    pub async fn poll_db_event(&mut self) -> Option<DbEvent> {
        self.db_rx.recv().await
    }

    /// Text the UI layer should copy to the system clipboard.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    /// Something the main loop must act on before more input: a program to
    /// hand the terminal to, text to copy, a notification, or quitting.
    pub fn has_pending_output(&self) -> bool {
        self.external.is_some() || self.clipboard.is_some() || self.notification.is_some() || self.should_quit
    }

    /// A long query's end, for the main loop to announce.
    pub fn take_notification(&mut self) -> Option<String> {
        self.notification.take()
    }

    /// The terminal window gained or lost focus.
    pub fn set_terminal_focus(&mut self, focused: bool) {
        self.terminal_focused = Some(focused);
    }

    /// Announces the end of a run that took at least `notify_after`, unless
    /// the terminal is known to have focus and its tab is shown (then you
    /// saw it end). A hidden tab's run is announced with its tab's name.
    fn notify_if_long(&mut self, what: impl FnOnce(Duration) -> String) {
        let Some(started) = self.tab.run.extra.started.take() else { return };
        let after = self.settings.notify_after;
        let seen = self.terminal_focused == Some(true) && self.hidden_tab.is_none();
        if after == 0 || started.elapsed() < Duration::from_secs(after) || seen {
            return;
        }
        let said = what(started.elapsed());
        let name = self.hidden_tab.and_then(|i| self.tabs.get(i)).map(|t| t.name.clone());
        self.notification = Some(match name {
            Some(name) => format!("{name}: {said}"),
            None => said,
        });
    }

    pub fn engine(&self) -> Engine {
        self.connection.database.as_ref().map_or(self.connection.form.engine, Database::engine)
    }

    /// The engine's SQL dialect; `None` where its language isn't SQL, the
    /// status saying what's asked needs SQL.
    pub(super) fn sql_dialect(&mut self) -> Option<&'static dyn crate::db::SqlDialect> {
        let engine = self.engine();
        let dialect = engine.dialect();
        if dialect.is_none() {
            self.set_status(t!("editor.no_sql", engine = engine.label()), true);
        }
        dialect
    }

    /// Whether asking a model works here, for the status bar: no model
    /// set up, or the default one, and whether the open project's schemas
    /// may go to it (`share_schema`).
    pub fn ai_state(&self) -> AiState {
        let Some(model) = self.settings.models.first() else {
            return AiState::Off;
        };
        let shared = podelis_ai::models::on_this_machine(model)
            || self.project().is_some_and(|p| p.share_schema == crate::config::store::ShareSchema::All);
        AiState::On { model: model.name.clone(), asks_to_share: !shared }
    }

    pub fn project(&self) -> Option<&Project> {
        self.current_project.as_deref().and_then(|id| self.store.project(id))
    }

    /// Connections of the open project.
    pub fn profiles(&self) -> &[ConnectionProfile] {
        self.project().map(|p| p.connections.as_slice()).unwrap_or_default()
    }

    /// Projects a connection can be moved to (all but the open one).
    pub fn move_targets(&self) -> Vec<&Project> {
        self.store.projects.iter().filter(|p| Some(&p.id) != self.current_project.as_ref()).collect()
    }

    /// Opens a project by name (case-insensitive), e.g. from the command line.
    pub fn open_project_by_name(&mut self, name: &str) -> Result<(), String> {
        let id = self
            .store
            .project_by_name(name)
            .map(|p| p.id.clone())
            .ok_or_else(|| t!("projects.no_such", name = name))?;
        self.open_project(&id);
        Ok(())
    }

    fn open_project(&mut self, id: &str) {
        if let Err(e) = self.store.set_last_project(id) {
            self.manager.connection_message = Some((format!("Couldn't save last project: {e:#}"), true));
        } else {
            self.manager.connection_message = None;
        }
        if self.current_project.as_deref() != Some(id) {
            self.save_draft();
            self.close_all_connections();
            self.reset_agents();
            self.history = History::load_from(self.history_path(id));
            self.load_tabs(id);
            self.manager.connection_selected = 0;
            self.manager.from_query = false;
        }
        self.current_project = Some(id.to_string());
        self.screen = Screen::Connections;
    }

    /// Where the app keeps its files, for tests that need them on disk.
    #[cfg(test)]
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data_dir = Some(dir);
    }

    fn history_path(&self, project_id: &str) -> Option<PathBuf> {
        let dir = self.data_dir.as_ref()?.join("history");
        Some(history::project_history_path(&dir, project_id))
    }

    fn draft_path(&self, project_id: &str) -> Option<PathBuf> {
        Some(self.data_dir.as_ref()?.join("drafts").join(format!("{project_id}.sql")))
    }

    /// Whether actions may reach outside the app: connect, run `ssh` or a
    /// shell command. Always, except in the random-keys test.
    #[cfg_attr(not(test), expect(clippy::unused_self, reason = "the random-keys test uses self"))]
    fn may_reach_outside(&self) -> bool {
        #[cfg(test)]
        if self.sandbox {
            return false;
        }
        true
    }

    /// A sender bound to the current generation, for use inside a task.
    fn sender(&self) -> impl Fn(DbEventKind) + Send + 'static {
        let tx = self.db_tx.clone();
        let generation = self.generation;
        move |kind| {
            let _ = tx.send(DbEvent { generation, run: None, kind });
        }
    }

    /// For the running query's outcome (see [`DbEvent::run`]).
    fn run_sender(&self) -> impl Fn(DbEventKind) + Send + 'static {
        let tx = self.db_tx.clone();
        let (generation, run) = (self.generation, Some(self.tab.run.id));
        move |kind| {
            let _ = tx.send(DbEvent { generation, run, kind });
        }
    }

    fn invalidate_tasks(&mut self) {
        self.tab.run.db = None;
        // A check on its way is for the connection before: its answer will
        // be dropped (another generation), so it's no longer awaited.
        self.keepalive.pinging = false;
        self.generation += 1;
        self.tab.run.id = self.next_run_id();
        self.tab.run.queued_plan = None;
        if let Some(task) = self.tab.run.task.take() {
            task.abort();
        }
    }

    /// Call after any change to the editor text: stops history browsing,
    /// clears the error marker and schedules a fresh syntax hint.
    fn edited(&mut self) {
        self.history.reset();
        self.tab.error_marker = None;
        self.tab.syntax_hint = None;
        self.lint_due = Some(Instant::now() + LINT_DELAY);
    }

    /// Queues `text` for the system clipboard and reports it in the status bar.
    fn copy_text(&mut self, text: String, status: &str) {
        // Over SSH the terminal carries the copy, and many drop a big one.
        let status = if crate::system::clipboard::over_ssh() && text.len() > crate::system::clipboard::OSC52_LIMIT {
            #[expect(clippy::cast_precision_loss, reason = "display only")]
            let kb = text.len() as f64 / 1000.0;
            format!("{status} ({kb:.0} kB: over SSH, a copy this big may not reach your clipboard; export it instead)")
        } else {
            status.to_string()
        };
        self.clipboard = Some(text);
        self.set_status(status, false);
    }

    /// Adds a line to the MESSAGES tab (keeps the last 200).
    fn log(&mut self, text: String, is_error: bool) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.notes.log.push(Message { time, text, is_error });
        if self.notes.log.len() > 200 {
            self.notes.log.remove(0);
            // The selected one stays selected (or goes with the oldest).
            self.notes.selected = self.notes.selected.and_then(|i| i.checked_sub(1));
        }
    }

    /// Whether keys are going into a text field (so `?` types a `?`).
    pub fn typing(&self) -> bool {
        match self.screen {
            Screen::Projects => self.manager.project_prompt.is_some(),
            Screen::Connections => false,
            Screen::Connect => true,
            Screen::Query => match self.top_overlay() {
                Some(overlay) => overlay.takes_text(),
                None => {
                    self.focus == Focus::Editor
                        || self.manager.project_prompt.is_some()
                        || self.find.is_some()
                        || self.cell_edit.is_some()
                        || self.tab.grid.filter_editing
                        || self.connection.schema.as_ref().is_some_and(|s| s.filter_editing)
                }
            },
        }
    }

    /// Quits, asking first if a transaction would be rolled back.
    fn request_quit(&mut self) {
        if !self.any_open_transaction() {
            self.should_quit = true;
            return;
        }
        self.popups.confirm = Some(Confirm {
            message: t!("app.quit_rollback").to_string(),
            details: vec![],
            danger: true,
            action: ConfirmAction::Quit,
            offer_transaction: false,
        });
    }

    /// A message for the user: in the editor's or the results' footer when
    /// it's about something done there, otherwise in the status bar.
    fn set_status(&mut self, status: String, is_error: bool) {
        // The completion list belongs to the editor. Not while the find bar
        // or a cell's prompt has the footer's place.
        let popup = self.help_visible || self.top_overlay().is_some_and(|o| o != Overlay::Completion);
        let footer = self.handling_input
            && self.screen == Screen::Query
            && !popup
            && match self.focus {
                Focus::Editor => self.find.is_none(),
                Focus::Results => self.cell_edit.is_none(),
                Focus::Schema => false,
            };
        if footer {
            // Gone with the next key: MESSAGES keeps it.
            if self.notes.log.last().is_none_or(|m| m.text != status) {
                self.log(status.clone(), is_error);
            }
            if self.focus == Focus::Editor {
                self.notes.editor_note = Some(status);
                self.notes.editor_note_is_error = is_error;
            } else {
                self.notes.results_note = Some((status, is_error));
            }
            return;
        }
        self.notes.status = status;
        self.notes.status_is_error = is_error;
        self.notes.status_in_results = false;
    }

    /// A run's progress or outcome, in the results pane's footer.
    fn set_result_status(&mut self, status: String, is_error: bool) {
        self.notes.status = status;
        self.notes.status_is_error = is_error;
        self.notes.status_in_results = true;
    }

    /// Runs `handle` for a key, click or paste: what it reports while the
    /// editor has the focus goes to the editor's own footer.
    fn on_input(&mut self, handle: impl FnOnce(&mut Self)) {
        // A paste can be handled inside a key (Ctrl+V).
        let outer = std::mem::replace(&mut self.handling_input, true);
        handle(self);
        self.handling_input = outer;
        self.recheck_lost_connection();
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.on_input(|app| app.paste(text));
    }

    fn paste(&mut self, text: &str) {
        match self.screen {
            Screen::Query => self.paste_on_query_screen(text),
            Screen::Projects => {
                if let Some(prompt) = &mut self.manager.project_prompt {
                    prompt.text.insert_str(text);
                }
            }
            Screen::Connect if !self.connection.form.connecting => {
                // A pasted connection URL fills in the whole form.
                if looks_like_url(text) {
                    match self.connection.form.apply_url(text.trim()) {
                        Ok(()) => {
                            self.connection.form.error = None;
                            self.connection.form.info = Some(t!("connections.from_url").to_string());
                        }
                        Err(e) => self.connection.form.error = Some(e),
                    }
                    return;
                }
                let focus = self.connection.form.focus;
                if let Some(field) = self.connection.form.field_mut(focus) {
                    input::paste_at_end(field, text);
                }
            }
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.panes.drag_out = None;
        self.on_input(|app| app.key(key));
    }

    fn key(&mut self, key: KeyEvent) {
        // Messages last until the next key; the MESSAGES tab keeps outcomes.
        if !self.tab.run.executing {
            self.notes.status.clear();
            self.notes.status_is_error = false;
        }
        self.notes.editor_note = None;
        self.notes.editor_note_is_error = false;
        self.notes.results_note = None;
        // A key hides the reopening card; a key typed before it showed counts.
        if let Some(splash) = self.splash.take()
            && splash.shown
        {
            return;
        }
        if self.help_visible {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) => {
                    self.help_visible = false;
                    self.help_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::F(1) {
            self.help_visible = true;
            return;
        }
        // `?` is every key, wherever it isn't a character being typed.
        if key.code == KeyCode::Char('?') && !self.typing() && !self.popup_open() {
            self.help_visible = true;
            return;
        }
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        // Ctrl+Q quits from anywhere (on the query screen Ctrl+C copies).
        let ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q');
        if let Some(confirm) = self.popups.confirm.take() {
            self.answer_confirm(confirm, key, ctrl_c || ctrl_q);
            // A sharing question waiting behind it comes next.
            return self.share_tick();
        }
        if ctrl_q {
            return self.request_quit();
        }
        if ctrl_c {
            // On the query screen Ctrl+C copies, and Ctrl+Q quits; elsewhere
            // it quits, asking first if a transaction is open.
            if matches!(self.screen, Screen::Query) {
                return self.copy_focused();
            }
            return self.request_quit();
        }

        match self.screen {
            Screen::Projects => self.handle_projects_key(key),
            Screen::Connections => self.handle_connections_key(key),
            Screen::Connect => self.handle_connect_key(key),
            Screen::Query => self.handle_query_key(key),
        }
    }
}

/// A run's time so far, as minutes and seconds (`3:07`).
pub(super) fn mm_ss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}
