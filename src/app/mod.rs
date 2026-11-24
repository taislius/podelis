use crate::config::store::ProjectId;
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
use crate::language::statements::{Kind, StatementInfo, TxnOp};
use crate::sql;
use crate::sql::analyze::{self, HintSource, SyntaxHint};
use crate::sql::complete::{self, Candidate, TableRef};
use crate::sql::params::Filled;
use crate::sql::parse::{ParsedScript, parse};
use crate::text_editor::{Pos, TextEditor};
pub use hits::Hits;
pub use input::{InputResult, LineInput};
use list::Nav;
pub use query_screen::editor::find::FindBar;
pub use query_screen::editor::vim::VimMode;
pub use query_screen::overlay::Overlay;
pub use query_screen::params::ParamForm;
pub use query_screen::results::grid::ResultGrid;
pub use query_screen::sandbox::Sandbox;
pub use query_screen::tabs::{PlanSource, TabState};
pub use query_screen::tabs::{TabMeta, TabPrompt, TabPromptKind};
pub use windows::agents::{AgentQuery, Agents, short_dir, short_time};
pub use windows::library::{LibraryItem, LibraryView};

pub mod commands;
mod confirm;
mod conn_state;
mod events;
mod hits;
mod input;
pub(crate) mod key_table;
mod list;
mod manager;
pub(crate) use manager::keys as manager_keys;
mod mouse;
pub(crate) mod query_screen;
mod session;
#[cfg(test)]
mod tests;
mod tick;
pub(in crate::app) use tick::SessionSeen;
pub mod windows;

pub use query_screen::results::export_form;

use crate::config::draft::{ConnectionDraft, looks_like_url};
pub use confirm::{Confirm, ConfirmAction};
use events::{Background, ConnEvent, DbEventKind, RunEvent};
pub use events::{DbEvent, Ping};
pub use manager::connections::{ConnectField, ConnectForm, SAVE_KEY, TEST_KEY, ctrl_words, fields_for};
pub use query_screen::navigator::details::{SchemaNode, SchemaPanel, SchemaSection, SchemaSort, SchemaTable};
pub use query_screen::results::view::CellEdit;
pub use query_screen::results::view::{NULL_TEXT, display_prefix, format_elapsed, visible_columns};
use query_screen::results::view::{column_widths, compute_view, one_line, pretty_value, widen_columns};
pub use query_screen::review::TxMode;
pub use query_screen::{Dest, Format, Run, RunPurpose};
pub use session::ConnState;
#[cfg(test)]
pub use session::Splash;
pub use windows::cell::CellView;

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

/// Identity of a query run, unique across the app's open tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunId(u64);

impl RunId {
    pub(in crate::app) fn next(&mut self) -> Self {
        self.0 += 1;
        *self
    }
}

/// Identity of an open tab, stable when tabs before it are closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabId(u64);

impl TabId {
    pub(in crate::app) fn next(&mut self) -> Self {
        self.0 += 1;
        *self
    }
}

/// Identity of an open connection (its slot in `Conns`): what a tab on it
/// keeps. Never reused while the app runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(u64);

impl ConnId {
    pub(in crate::app) fn next(&mut self) -> Self {
        self.0 += 1;
        *self
    }

    #[cfg(test)]
    pub(in crate::app) const fn from_test_id(id: u64) -> Self {
        Self(id)
    }
}

/// Identity of the active connection state, incremented to invalidate its
/// in-flight background work when the connection changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct ConnectionGeneration(u64);

impl ConnectionGeneration {
    pub(in crate::app) fn bump(&mut self) {
        self.0 += 1;
    }
}

/// Identity of a replaceable database-container search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct ContainerRequestId(u64);

impl ContainerRequestId {
    pub(in crate::app) fn next(&mut self) -> Self {
        self.0 += 1;
        *self
    }

    #[cfg(test)]
    pub(in crate::app) fn from_test_id(id: u64) -> Self {
        Self(id)
    }
}

/// The database containers found by `c` in the connections, to pick one.
#[derive(Default)]
pub struct ContainerPicker {
    /// Which search this is: a result for an earlier, closed one is dropped.
    pub(in crate::app) request: ContainerRequestId,
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
    RenameProject(ProjectId),
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
    /// Where a failure was in the editor (its tab, and the range then):
    /// `g` in MESSAGES goes there.
    pub at: Option<(TabId, Pos, Pos)>,
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
    pub popups: query_screen::overlay::Popups,
    /// `Ctrl+K q`'s question being answered, and what goes with it.
    pub ask: query_screen::ask::AskState,
    /// What's said to the user.
    pub notes: query_screen::results::messages::Notes,
    /// The panes' layout.
    pub panes: query_screen::panes::Panes,
    /// The projects and connections screen's state.
    pub manager: manager::projects::Manager,
    /// The project's active and parked connections; tabs refer to slot IDs.
    pub connection: session::ProjectConnections,
    /// What agents did and asked for (the agents view).
    pub agents: Agents,
    pub screen: Screen,
    pub store: ConnectionStore,
    /// Id of the open project.
    pub current_project: Option<ProjectId>,
    /// Config folder holding per-project history and editor drafts; `None`
    /// disables persistence (tests).
    data_dir: Option<PathBuf>,
    /// A sandbox's way in and out: its first step waiting, where its
    /// script goes.
    sandbox_flow: query_screen::sandbox::SandboxFlow,
    /// The editor's own state beside the tab's text: find, vim, the
    /// checks' timer.
    pub editing: query_screen::editor::Editing,
    last_pipe: String,
    /// Values typed for each parameter name, offered again next time.
    param_values: HashMap<String, String>,
    /// Tabs-file writes, so a late background one can't undo a newer one.
    drafts: query_screen::tabs::Drafts,
    /// Tests that press random keys: nothing that reaches outside the app
    /// (connecting, `ssh`, shell commands).
    #[cfg(test)]
    pub(crate) sandbox: bool,
    /// Handling a key, click or paste (see `on_input`).
    handling_input: bool,
    pub settings: Settings,
    /// The heartbeat last passed to the open connections (`keepalive`).
    applied_heartbeat: u64,
    /// The heartbeat interval to restore when heartbeat is toggled back on.
    heartbeat_secs: u64,
    pub keymap: crate::config::keys::KeyMap,
    history: History,
    pub focus: Focus,
    /// Every key's window (F1, `?`).
    pub help: Help,
    /// Reopening the session's connections (Esc or any key hides it).
    pub splash: Option<session::Splash>,
    connect_started: Option<Instant>,
    /// The latest background swap of a tab's connection (`Swap`), and how
    /// many were asked for.
    latest_swap: Option<events::Swap>,
    swaps: u64,
    /// What the status bar said as a swap began ("Opening …"), to take
    /// back if its answer's dropped.
    swap_said: Option<(events::Swap, String, Instant)>,
    /// The open cell-edit prompt.
    pub cell_edit: Option<CellEdit>,
    /// Open tabs and their project connection slot references.
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
    pub watch: Option<query_screen::watch::Watch>,
    /// Exports of every row: the one running, its size being worked out,
    /// the form's last choice.
    exports: query_screen::results::export::Exports,
    /// The last run id handed out ([`App::next_run_id`]).
    run_ids: RunId,
    /// The last tab id handed out. IDs are not reused while the app lives.
    tab_ids: TabId,
    /// While a hidden tab's event is applied (`apply_in_tab`): that tab,
    /// whose run ending is announced even with the terminal in front.
    hidden_tab: Option<usize>,
    /// A function's definition on its way is for the navigator's popup.
    routine_definition_pending: bool,
    /// Clickable areas from the last frame.
    pub hits: Hits,
    /// `f` is waiting for the result table's foreign keys to load.
    pending_follow: bool,
    /// The last table definition `d` asked the server's DDL of: an older
    /// request, waiting its moment, sees it isn't wanted and asks nothing.
    ddl_wanted: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub should_quit: bool,
    /// What the main loop does for the app, outside the screen.
    for_terminal: ForTerminal,
    /// Whether the terminal window has focus, once the terminal says.
    terminal_focused: Option<bool>,
    /// Bumped whenever in-flight work should be ignored (disconnect, cancelled
    /// connect). Every task's event carries the value it was started under.
    generation: ConnectionGeneration,
    db_tx: mpsc::UnboundedSender<DbEvent>,
    db_rx: mpsc::UnboundedReceiver<DbEvent>,
}

/// What the main loop picks up from the app and does outside the screen.
#[derive(Default)]
struct ForTerminal {
    /// Text for the system clipboard (OSC 52, or the system's tool).
    clipboard: Option<String>,
    /// A long query ended: announced (bell, notification).
    notification: Option<String>,
    /// A viewer to run with the terminal (`o`).
    external: Option<query_screen::pipes::External>,
}

/// Every key's window (F1, `?`): whether it's open, how far it's scrolled.
#[derive(Default)]
pub struct Help {
    pub visible: bool,
    pub scroll: u16,
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
            popups: query_screen::overlay::Popups::default(),
            ask: query_screen::ask::AskState::default(),
            screen: Screen::Projects,
            store,
            current_project: None,
            history: History::load_from(None),
            data_dir,
            last_pipe: String::new(),
            editing: query_screen::editor::Editing::default(),
            sandbox_flow: query_screen::sandbox::SandboxFlow::default(),
            param_values: HashMap::new(),
            drafts: query_screen::tabs::Drafts::default(),
            #[cfg(test)]
            sandbox: false,
            handling_input: false,
            settings: Settings::default(),
            heartbeat_secs: 30,
            applied_heartbeat: 0,
            keymap: crate::config::keys::KeyMap::default(),
            focus: Focus::Editor,
            help: Help::default(),
            splash: None,
            connect_started: None,
            latest_swap: None,
            swaps: 0,
            swap_said: None,
            cell_edit: None,
            tabs: vec![TabMeta {
                id: TabId(1),
                name: query_screen::tabs::default_tab_name(1),
                saved_id: None,
                conn: None,
            }],
            tab: TabState::default(),
            parked: vec![None],
            active_tab: 0,
            tab_ids: TabId(1),
            library: crate::config::library::Library::load_from(None).0,
            watch: None,
            exports: query_screen::results::export::Exports::default(),
            run_ids: RunId::default(),
            hidden_tab: None,
            hits: Hits::default(),
            pending_follow: false,
            ddl_wanted: std::sync::Arc::default(),
            routine_definition_pending: false,
            should_quit: false,
            for_terminal: ForTerminal::default(),
            terminal_focused: None,
            generation: ConnectionGeneration::default(),
            notes: query_screen::results::messages::Notes::default(),
            panes: query_screen::panes::Panes {
                editor_percent: 40,
                editor_height: 1,
                ..query_screen::panes::Panes::default()
            },
            manager: manager::projects::Manager {
                project_selected: projects_selected,
                project_message: projects_message,
                ..manager::projects::Manager::default()
            },
            connection: session::ProjectConnections {
                active: session::Connection {
                    form: ConnectForm::new(),
                    mode: ConnectionMode::Normal,
                    tx_mode: TxMode::AutoCommit,
                    txn: TxnState::Idle,
                    ..session::Connection::default()
                },
                slots: session::Conns::default(),
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
        self.for_terminal.clipboard.take()
    }

    /// Something the main loop must act on before more input: a program to
    /// hand the terminal to, text to copy, a notification, or quitting.
    pub fn has_pending_output(&self) -> bool {
        self.for_terminal.external.is_some()
            || self.for_terminal.clipboard.is_some()
            || self.for_terminal.notification.is_some()
            || self.should_quit
    }

    /// A long query's end, for the main loop to announce.
    pub fn take_notification(&mut self) -> Option<String> {
        self.for_terminal.notification.take()
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
        self.for_terminal.notification = Some(match name {
            Some(name) => format!("{name}: {said}"),
            None => said,
        });
    }

    /// The engine the tab's SQL is for: its connection's. With none open
    /// (one that didn't reopen, say), the form's if it's a connection's,
    /// else the one the tab had last time, else the project's if all its
    /// connections are on one: not the blank form's default, which would
    /// read MySQL's `SET @id = 1` as wrong.
    pub fn engine(&self) -> Engine {
        if let Some(db) = &self.connection.database {
            return db.engine();
        }
        let form = &self.connection.form;
        if !form.name.is_empty() {
            return form.engine;
        }
        let profiles = self.profiles();
        let last = self.connection.slots.tab_connections.get(self.active_tab).and_then(Option::as_ref);
        if let Some(profile) = last.and_then(|name| profiles.iter().find(|p| &p.name == name)) {
            return profile.engine;
        }
        match profiles.split_first() {
            Some((first, rest)) if rest.iter().all(|p| p.engine == first.engine) => first.engine,
            _ => form.engine,
        }
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
        self.current_project.as_ref().and_then(|id| self.store.project(id))
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

    fn open_project(&mut self, id: &ProjectId) {
        if let Err(e) = self.store.set_last_project(id) {
            self.manager.connection_message = Some((format!("Couldn't save last project: {e:#}"), true));
        } else {
            self.manager.connection_message = None;
        }
        if self.current_project.as_ref() != Some(id) {
            self.save_draft();
            self.close_all_connections();
            self.reset_agents();
            self.history = History::load_from(self.history_path(id));
            self.load_tabs(id);
            self.manager.connection_selected = 0;
            self.manager.from_query = false;
        }
        self.current_project = Some(id.clone());
        self.screen = Screen::Connections;
    }

    /// Where the app keeps its files, for tests that need them on disk.
    #[cfg(test)]
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data_dir = Some(dir);
    }

    fn history_path(&self, project_id: &ProjectId) -> Option<PathBuf> {
        let dir = self.data_dir.as_ref()?.join("history");
        Some(history::project_history_path(&dir, project_id))
    }

    fn draft_path(&self, project_id: &ProjectId) -> Option<PathBuf> {
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
            let _ = tx.send(DbEvent::connection(generation, kind));
        }
    }

    /// For what's learnt about connection `id`, applied to it wherever it
    /// is then.
    fn conn_sender(&self, id: ConnId) -> impl Fn(ConnEvent) + Send + 'static {
        let tx = self.db_tx.clone();
        move |kind| {
            let _ = tx.send(DbEvent::conn(id, kind));
        }
    }

    /// For the running query's outcome, applied to its tab by run id.
    fn run_sender(&self) -> impl Fn(RunEvent) + Send + 'static {
        let tx = self.db_tx.clone();
        let run = self.tab.run.id;
        move |kind| {
            let _ = tx.send(DbEvent::run(run, kind));
        }
    }

    /// A new background swap for the shown tab: the latest, so an earlier
    /// one's answer is dropped.
    fn new_swap(&mut self) -> events::Swap {
        self.swaps += 1;
        let swap = events::Swap { tab: self.tabs[self.active_tab].id, attempt: self.swaps };
        self.latest_swap = Some(swap);
        // Its latency counts from now (not from one asked for before).
        let now = Instant::now();
        self.connect_started = Some(now);
        self.swap_said = Some((swap, self.notes.status.clone(), now));
        swap
    }

    /// A swap's answer dropped (another asked for since, or none wanted):
    /// its "Opening …" goes, unless something else is said by now.
    fn swap_dropped(&mut self, swap: events::Swap) {
        let Some((_, text, started)) = self.swap_said.take_if(|(said, ..)| *said == swap) else { return };
        if text == self.notes.status {
            self.set_status(String::new(), false);
        }
        // Its time, not another connect's begun since.
        if self.connect_started == Some(started) {
            self.connect_started = None;
        }
    }

    fn invalidate_tasks(&mut self) {
        // A connection on its way for the tab isn't wanted now either.
        self.latest_swap = None;
        self.tab.run.db = None;
        // (A check on its way is its connection's: its answer still finds
        // it, by id, so it's still awaited.)
        self.generation.bump();
        self.tab.run.id = self.next_run_id();
        self.tab.run.queued_plan = None;
        if let Some(task) = self.tab.run.task.take() {
            task.abort();
        }
    }

    /// What follows a change to the text: the history's place starts over,
    /// the error and hint marks go, the checks run once typing pauses.
    /// Keys, the mouse and pastes do it themselves when the text changed
    /// (`on_input`); a change from elsewhere (an answer arriving) calls it.
    fn edited(&mut self) {
        self.history.reset();
        self.tab.error_marker = None;
        self.tab.syntax_hint = None;
        self.editing.lint_due = Some(Instant::now() + LINT_DELAY);
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
        self.for_terminal.clipboard = Some(text);
        self.set_status(status, false);
    }

    /// Adds a line to the MESSAGES tab (keeps the last 200).
    fn log(&mut self, text: String, is_error: bool) {
        // Clearing the status (an empty one) isn't a message: else a line
        // with a time and nothing after it.
        if text.trim().is_empty() {
            return;
        }
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.notes.log.push(Message { time, text, is_error, at: None });
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
                // The export form's name, on its line.
                Some(Overlay::ExportForm) => {
                    self.popups.export_form.as_ref().is_some_and(|f| f.line == export_form::ExportLine::Name)
                }
                Some(overlay) => overlay.takes_text(),
                None => {
                    self.focus == Focus::Editor
                        || self.manager.project_prompt.is_some()
                        || self.editing.find.is_some()
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
        let popup = self.help.visible || self.top_overlay().is_some_and(|o| o != Overlay::Completion);
        let footer = self.handling_input
            && self.screen == Screen::Query
            && !popup
            && match self.focus {
                Focus::Editor => self.editing.find.is_none(),
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
        let before = (self.tabs[self.active_tab].id, self.tab.editor.version());
        handle(self);
        self.handling_input = outer;
        // Whatever changed the text, what goes with a change follows: no
        // edit by a key, the mouse or a paste can forget it.
        let same_tab = self.tabs[self.active_tab].id == before.0;
        if !outer && same_tab && self.tab.editor.version() != before.1 {
            if self.editing.recalled == Some(self.tab.editor.version()) {
                // From the history (↑/↓ there): its place in it is kept.
                self.editing.lint_due = Some(Instant::now() + LINT_DELAY);
            } else {
                self.edited();
            }
        }
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
                self.connection.form.paste_into_field(text);
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
        if self.help.visible {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) => {
                    self.help.visible = false;
                    self.help.scroll = 0;
                }
                _ => match key_table::movement(key) {
                    Some(key_table::Move::Down) => self.help.scroll = self.help.scroll.saturating_add(1),
                    Some(key_table::Move::Up) => self.help.scroll = self.help.scroll.saturating_sub(1),
                    Some(key_table::Move::PageDown) => self.help.scroll = self.help.scroll.saturating_add(10),
                    Some(key_table::Move::PageUp) => self.help.scroll = self.help.scroll.saturating_sub(10),
                    _ => {}
                },
            }
            return;
        }
        if key.code == KeyCode::F(1) {
            self.help.visible = true;
            return;
        }
        // `?` is every key, wherever it isn't a character being typed.
        if key.code == KeyCode::Char('?') && !self.typing() && !self.popup_open() {
            self.help.visible = true;
            return;
        }
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        // Ctrl+Q quits from anywhere (on the query screen Ctrl+C copies).
        let ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q');
        if let Some(confirm) = self.popups.confirm.take() {
            self.answer_confirm(confirm, key, ctrl_c || ctrl_q);
            // An export's question, or a sharing one, waiting behind it comes next.
            self.offer_export_question();
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

        if key.modifiers == KeyModifiers::CONTROL
            && matches!(self.screen, Screen::Projects | Screen::Connections | Screen::Connect)
            && self.resize_manager(key.code)
        {
            return;
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
