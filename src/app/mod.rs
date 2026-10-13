use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthStr;

use crate::analyze::{self, HintSource, Kind, StatementInfo, SyntaxHint, TxnOp};
use crate::complete::{self, Candidate, TableRef};
use crate::db::{
    CheckOutcome, ColumnInfo, ConnectParams, ConnectionMode, Database, Engine, ErrorPos, Page, QueryOutcome,
    RoutineInfo, RowStream, SslMode, TableDetails, TableInfo, TxnState,
};
use crate::editor::{Pos, TextEditor};
use crate::history::{self, History};
use crate::params::Filled;
use crate::parse::{ParsedScript, parse};
use crate::settings::{EditorMode, Settings};
use crate::sql;
use crate::store::{self, ConnectionProfile, ConnectionStore, Project};
pub use find::FindBar;
pub use grid::ResultGrid;
pub use input::{InputResult, LineInput};
use list::Nav;
pub use overlay::Overlay;
pub use params::ParamForm;
pub use tabs::{LibraryItem, LibraryView, TabMeta, TabPrompt, TabPromptKind};
pub use tabs::{PlanSource, TabState};
use vim::Vim;
pub use vim::VimMode;

mod activity;
mod compare;
mod connections;
mod edit;
mod events;
mod find;
mod follow;
mod grid;
mod info;
mod input;
mod list;
mod menu;
mod mouse;
mod overlay;
mod palette;
mod params;
mod pipes;
mod projects;
mod query;
mod results;
mod review;
mod rows;
mod schema;
mod session;
mod tabs;
#[cfg(test)]
mod tests;
mod vim;

use connections::looks_like_url;
pub use connections::{ConnectField, ConnectForm, fields_for};
pub use results::{NULL_TEXT, display_prefix, format_elapsed, visible_columns};
use results::{column_widths, compute_view, one_line, pretty_value, widen_columns};
pub use review::TxMode;
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

pub struct SchemaTable {
    pub info: TableInfo,
    pub expanded: bool,
    pub columns: Vec<ColumnInfo>,
    /// Indexes, foreign keys and definition, loaded on first expand.
    pub details: Option<TableDetails>,
    pub details_loading: bool,
}

impl SchemaTable {
    fn new(info: TableInfo, columns: Vec<ColumnInfo>) -> Self {
        SchemaTable { info, expanded: false, columns, details: None, details_loading: false }
    }

    /// Where column `name` points as a foreign key (`orders.id`).
    pub fn foreign_key(&self, column: &str) -> Option<&str> {
        let details = self.details.as_ref()?;
        details.foreign_keys.iter().find(|f| f.column == column).map(|f| f.target.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaSection {
    Tables,
    Views,
    /// The database's own catalog tables and views, when shown.
    System,
    Routines,
}

impl SchemaSection {
    /// Whether `info` is listed in this section.
    pub fn holds(self, info: &TableInfo) -> bool {
        match self {
            SchemaSection::Tables => !info.system && !info.is_view,
            SchemaSection::Views => !info.system && info.is_view,
            SchemaSection::System => info.system,
            SchemaSection::Routines => false,
        }
    }
}

/// The order of the navigator's tables (`o` cycles it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchemaSort {
    #[default]
    Name,
    /// Most rows first (the server's estimate).
    Rows,
    /// Biggest on disk first, indexes included.
    Size,
}

impl SchemaSort {
    pub fn next(self) -> Self {
        match self {
            SchemaSort::Name => SchemaSort::Rows,
            SchemaSort::Rows => SchemaSort::Size,
            SchemaSort::Size => SchemaSort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SchemaSort::Name => "name",
            SchemaSort::Rows => "rows",
            SchemaSort::Size => "size",
        }
    }
}

/// One visible row of the schema tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaNode {
    /// A section header; not selectable.
    Section(SchemaSection),
    Table(usize),
    Column(usize, usize),
    Index(usize, usize),
    Routine(usize),
}

impl SchemaNode {
    fn table(self) -> Option<usize> {
        match self {
            SchemaNode::Table(t) | SchemaNode::Column(t, _) | SchemaNode::Index(t, _) => Some(t),
            SchemaNode::Section(_) | SchemaNode::Routine(_) => None,
        }
    }
}

/// The database catalog: shown in the schema browser and used for
/// autocomplete. Loaded on connect.
#[derive(Default)]
pub struct SchemaPanel {
    pub tables: Vec<SchemaTable>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    /// Only tables whose name contains this (case-insensitive) are shown.
    pub filter: LineInput,
    pub filter_editing: bool,
    /// Functions and procedures.
    pub routines: Vec<RoutineInfo>,
    /// A table whose definition opens once its details arrive.
    pub show_definition_of: Option<(String, String)>,
    pub sort: SchemaSort,
    /// The first row on screen, and how many fit (as last drawn).
    pub scroll: usize,
    pub page: usize,
}

impl SchemaPanel {
    /// Scrolls so the selected row shows, with an expanded table's
    /// columns and indexes as far as they fit (the table stays on screen),
    /// for `height` rows. Returns the first row to show.
    pub fn scroll_into_view(&mut self, nodes: &[SchemaNode], height: usize) -> usize {
        self.page = height.max(1);
        let selected = self.selected.min(nodes.len().saturating_sub(1));
        if let Some(SchemaNode::Table(t)) = nodes.get(selected) {
            let children = nodes[selected + 1..].iter().take_while(|n| n.table() == Some(*t)).count();
            let wanted = (selected + children + 1).saturating_sub(self.page).min(selected);
            self.scroll = self.scroll.max(wanted);
        }
        if selected < self.scroll {
            // A section's heading comes along with its first table.
            let heading = selected.checked_sub(1).filter(|&i| matches!(nodes[i], SchemaNode::Section(_)));
            self.scroll = heading.unwrap_or(selected);
        }
        if selected >= self.scroll + self.page {
            self.scroll = selected + 1 - self.page;
        }
        // No empty space below the last row.
        self.scroll = self.scroll.min(nodes.len().saturating_sub(self.page));
        self.scroll
    }

    /// The visible rows: a TABLES section, then VIEWS, then SYSTEM (if
    /// loaded), each with its (filtered) tables and the columns of expanded
    /// ones.
    pub fn nodes(&self) -> Vec<SchemaNode> {
        let needle = self.filter.text().to_lowercase();
        let mut nodes = Vec::new();
        let mut buf = String::new();
        let mut shown = |name: &str| needle.is_empty() || results::contains_ignoring_case(name, &needle, &mut buf);
        for section in [SchemaSection::Tables, SchemaSection::Views, SchemaSection::System] {
            let mut matching: Vec<usize> = (0..self.tables.len())
                .filter(|&t| section.holds(&self.tables[t].info) && shown(&self.tables[t].info.name))
                .collect();
            // Biggest first; unknown sizes last. The sort is stable, so
            // ties stay by name.
            match self.sort {
                SchemaSort::Name => {}
                SchemaSort::Rows => matching.sort_by_key(|&t| std::cmp::Reverse(self.tables[t].info.rows)),
                SchemaSort::Size => matching.sort_by_key(|&t| std::cmp::Reverse(self.tables[t].info.bytes)),
            }
            if matching.is_empty() {
                continue;
            }
            nodes.push(SchemaNode::Section(section));
            for t in matching {
                nodes.push(SchemaNode::Table(t));
                let table = &self.tables[t];
                if table.expanded {
                    nodes.extend((0..table.columns.len()).map(|c| SchemaNode::Column(t, c)));
                    let indexes = table.details.as_ref().map_or(0, |d| d.indexes.len());
                    nodes.extend((0..indexes).map(|i| SchemaNode::Index(t, i)));
                }
            }
        }
        let routines: Vec<usize> = (0..self.routines.len()).filter(|&r| shown(&self.routines[r].name)).collect();
        if !routines.is_empty() {
            nodes.push(SchemaNode::Section(SchemaSection::Routines));
            nodes.extend(routines.into_iter().map(SchemaNode::Routine));
        }
        nodes
    }
}

/// The database containers found by `c` in the connections, to pick one.
#[derive(Default)]
pub struct ContainerPicker {
    /// Which search this is: a result for an earlier, closed one is dropped.
    pub request: u64,
    pub loading: bool,
    pub found: Vec<crate::containers::Container>,
    pub selected: usize,
    pub error: Option<String>,
    /// About the selected container (why it can't be used), until the
    /// selection moves.
    pub note: Option<String>,
}

pub struct DbEvent {
    generation: u64,
    kind: DbEventKind,
}

enum DbEventKind {
    /// A saved connection's password, and why it's missing if the keychain
    /// couldn't be read.
    PasswordLoaded(ConnectionProfile, String, Option<String>),
    Connected(Database),
    ConnectFailed(String),
    /// A finished run, with a stream of further rows if it was streamed.
    QueryDone(QueryOutcome, Option<TxnState>, Option<RowStream>),
    /// Another page of a streamed result.
    MoreRows(Page),
    /// Rows written so far by "export all rows".
    ExportProgress(u64),
    /// "Export all rows" finished: the file, and how many rows (or why not).
    ExportDone(PathBuf, Result<u64, String>),
    MoreFailed(String),
    QueryFailed(String, Option<ErrorPos>, Option<TxnState>),
    CancelFailed(String),
    Catalog(Result<Vec<(TableInfo, Vec<ColumnInfo>)>, String>),
    Routines(Vec<RoutineInfo>),
    TableDetails {
        schema: String,
        table: String,
        result: Result<TableDetails, String>,
    },
    /// A connection reopened at startup.
    Restored {
        project_id: String,
        form: Box<ConnectForm>,
        result: Result<Database, String>,
    },
    /// The running database containers, or why they couldn't be listed.
    ContainersFound(u64, Result<Vec<crate::containers::Container>, String>),
    /// The server's sessions, for the activity view.
    ActivityLoaded(Result<Vec<crate::db::Activity>, String>),
    /// A session's query was cancelled or the session ended.
    SessionStopped(Result<String, String>),
    /// A keep-alive check: latency and whether it had to reconnect.
    Pinged(Result<(Duration, bool), String>),
    /// A connection test's report (title, text).
    Diagnosed(String, String),
    /// The output of piping rows through a command.
    PipeDone(String, Result<String, String>),
    /// A definition to show (title, text).
    Definition(String, Result<String, String>),
    /// The background syntax check of `text`: the server's problem, else
    /// the parser's hint, else the first lint warning.
    SyntaxChecked {
        text: String,
        hint: Option<SyntaxHint>,
    },
    /// Both sides' schemas, for comparing.
    SchemaCompared {
        other: compare::Other,
        result: Result<compare::Shapes, String>,
    },
    /// The same query's result on both sides.
    RowsCompared {
        other: compare::Other,
        result: Box<Result<(QueryOutcome, QueryOutcome), String>>,
    },
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

/// File formats for exporting results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    /// `INSERT` statements.
    Sql,
}

/// Where the UI last drew the clickable parts, for the mouse.
#[derive(Clone, Debug, Default)]
pub struct Hits {
    pub schema: Option<Rect>,
    /// The schema's filter line (a click starts typing in it).
    pub schema_filter: Rect,
    /// The schema tree's rows, and the index of the first one shown.
    pub schema_list: Rect,
    pub schema_offset: usize,
    /// The connection in the status bar (a click shows its info).
    pub connection: Rect,
    /// The project's name in the top bar (a click switches project).
    pub project: Rect,
    pub editor: Rect,
    pub editor_text: Rect,
    /// The write modes in the editor's footer.
    pub tx_mode: Rect,
    /// The line above a plan saying what it's for (a click goes there).
    pub plan_source: Rect,
    /// The syntax hint under the editor (a click goes to what it's about).
    pub syntax_hint: Rect,
    /// Editor tab strip: (row, then each tab's x range and index).
    pub tabs: Vec<(u16, u16, usize)>,
    pub tabs_y: u16,
    /// The `+` after the tabs.
    pub new_tab: Option<(u16, u16)>,
    /// The run buttons right of the tabs: x ranges and actions.
    pub run_buttons: Vec<(u16, u16, crate::keys::Action)>,
    pub results: Rect,
    /// The result table's body rows, and each visible column's x range.
    pub result_rows: Rect,
    pub result_cols: Vec<(u16, u16, usize)>,
    /// The open pop-up menu.
    pub menu: Rect,
    /// The projects and connections panes: each list's rows and the
    /// index of the first one shown, and the settings pane.
    pub manager_projects: (Rect, usize),
    pub manager_connections: (Rect, usize),
    pub manager_settings: Rect,
    /// The results pane's tabs: their x ranges, and their row.
    pub result_tabs: Vec<(u16, u16, ResultTab)>,
    pub result_tabs_y: u16,
}

/// Extra state of the running query.
#[derive(Default)]
struct RunExtra {
    /// When it hits the connection's timeout, and the timeout.
    deadline: Option<(Instant, Duration)>,
    /// It was cancelled by the timeout.
    timed_out: Option<Duration>,
    /// It was started by `run_in_transaction`.
    reviewing: bool,
    /// It is a dry run, rolled back when it ends.
    dry_run: Option<review::DryRun>,
    /// Said before the next result's status (what an edit just did).
    status_prefix: Option<String>,
    /// When it started, to announce a long run's end.
    started: Option<Instant>,
    /// It ended a statement with `\G`: show the result one column per line.
    vertical: bool,
}

/// What a run's result is for.
#[derive(Default)]
pub enum RunPurpose {
    /// Show the result set.
    #[default]
    Query,
    /// Show the query plan in the QUERY PLAN tab.
    Explain,
    /// An UPDATE saving one edited cell; on success the cell is updated in
    /// place instead of replacing the results.
    CellEdit { row: usize, col: usize, value: Option<String> },
    /// COMMIT or ROLLBACK of a reviewed transaction: keeps the results.
    TxControl(&'static str),
    /// An INSERT or DELETE from the result grid ("Added"/"Deleted"); the
    /// query runs again afterwards.
    RowChange(&'static str),
}

/// The inline "SET column = …" prompt for editing a result cell.
pub struct CellEdit {
    /// Index into `results.rows` and the column.
    pub row: usize,
    pub col: usize,
    pub column: String,
    /// Quoted, possibly schema-qualified table name.
    pub table: String,
    /// Primary key column names and this row's values, as literals.
    pub key: Vec<(String, String)>,
    /// The column's type, as the result names it.
    pub column_type: String,
    pub nullable: bool,
    pub text: LineInput,
}

/// Outcome of checking the editor text with the server.
pub enum ServerCheck {
    /// The session stayed busy; fall back to the local parser.
    Busy,
    /// Checked: the first problem, if any.
    Done(Option<SyntaxHint>),
}

/// Something to do once the user answers "y".
pub enum ConfirmAction {
    Run {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        purpose: RunPurpose,
        filled: Option<Filled>,
    },
    Disconnect,
    Quit,
    CloseTab,
    /// Leave the project, for another one (or the project list); `true`
    /// lands in its connections.
    LeaveProject(Option<String>, bool),
}

/// The autocomplete popup.
pub struct Completion {
    pub items: Vec<Candidate>,
    pub selected: usize,
    /// Chars before the cursor that the chosen item replaces.
    pub prefix_len: usize,
}

pub struct CellView {
    pub title: String,
    pub text: String,
    pub scroll: u16,
    /// Showing the selected row as `column  value` lines (`x`): ←/→ move
    /// to the previous/next row.
    pub record: bool,
    /// SQL that comes with the text (a schema comparison's), and the
    /// connection it's for: `y` copies it, `e` opens it in a tab there.
    pub sql: Option<(String, u64)>,
    /// How far it scrolls, as last drawn (the text wraps to the popup).
    pub max_scroll: u16,
    /// A definition opened from the navigator (`d`): ←/→ show the previous
    /// or next table's, view's or function's.
    pub browse: bool,
}

impl CellView {
    pub fn new(title: String, text: String) -> Self {
        CellView { title, text, scroll: 0, record: false, sql: None, max_scroll: u16::MAX, browse: false }
    }

    /// A navigator definition (`d`), browsable with ←/→.
    pub fn definition(title: String, text: String) -> Self {
        CellView { browse: true, ..CellView::new(title, text) }
    }
}

pub struct Confirm {
    pub message: String,
    pub details: Vec<String>,
    /// Style the dialog as dangerous (production, destructive statements).
    pub danger: bool,
    pub action: ConfirmAction,
    /// Offer `t`: run the writes in a transaction to review before committing.
    pub offer_transaction: bool,
}

pub struct App {
    pub screen: Screen,
    pub store: ConnectionStore,
    /// Id of the open project.
    pub current_project: Option<String>,
    /// The project last left for the project list; Esc there goes back to it.
    left_project: Option<String>,
    pub projects_selected: usize,
    /// Message on the projects screen, and whether it's an error.
    pub projects_message: Option<(String, bool)>,
    pub project_prompt: Option<Prompt>,
    /// The projects and connections panes were opened over the work (so
    /// leaving the project from them reopens it on its tabs next time).
    manager_from_query: bool,
    /// The active tab's connection form, set aside while the settings pane
    /// edits another connection's (connecting takes a new tab instead).
    form_stash: Option<ConnectForm>,
    /// The active tab was opened, or disconnected, to pick a connection:
    /// leaving the panes drops it if it's still empty and unconnected.
    drop_blank_tab: bool,
    pub confirm_delete_project: bool,
    /// While moving a connection: the selected index into `move_targets()`.
    pub moving_to: Option<usize>,
    /// Picking a running database container (`c` in the connections).
    /// (Boxed: `App` is part of every async test's future.)
    pub containers: Option<Box<ContainerPicker>>,
    /// The number of the last container search started.
    containers_request: u64,
    /// Config folder holding per-project history and editor drafts; `None`
    /// disables persistence (tests).
    data_dir: Option<PathBuf>,
    /// Lines visible in the editor, recorded by the UI for PageUp/PageDown.
    pub editor_height: usize,
    /// Parameter substitutions in the last run, for mapping error positions.
    last_run_filled: Option<Filled>,
    pub param_form: Option<ParamForm>,
    /// Asking to commit or roll back writes run in a transaction.
    pub review: Option<review::Review>,
    /// The find/replace bar (Ctrl+F).
    pub find: Option<find::FindBar>,
    /// A pop-up menu of what can be done in the focused pane.
    pub menu: Option<menu::Menu>,
    /// The add-row form (`a`).
    pub row_form: Option<rows::RowForm>,
    /// The command the rows are piped through (`|`), being typed.
    pub pipe_prompt: Option<LineInput>,
    last_pipe: String,
    /// A viewer to run with the terminal (`o`), picked up by the main loop.
    external: Option<pipes::External>,
    /// The activity monitor.
    pub activity: Option<activity::ActivityView>,
    /// The command palette (Ctrl+K).
    pub palette: Option<palette::Palette>,
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
    /// A note about typing (e.g. "No completions") in the editor's footer,
    /// next to where you type; gone with the next key.
    pub editor_note: Option<String>,
    lint_due: Option<Instant>,
    pub settings: Settings,
    pub keymap: crate::keys::KeyMap,
    pub connections_selected: usize,
    /// Message on the connections screen, and whether it's an error.
    pub connections_message: Option<(String, bool)>,
    pub confirm_delete: bool,
    pub loading_profile: bool,
    pub connect_form: ConnectForm,
    history: History,
    pub focus: Focus,
    pub database: Option<Database>,
    pub connection_summary: String,
    /// Full-value popup for the selected cell.
    pub cell_view: Option<CellView>,
    /// Outcomes and errors of recent runs, oldest first.
    pub messages: Vec<Message>,
    pub help_visible: bool,
    /// Reopening the session's connections (Esc or any key hides it).
    pub splash: Option<session::Splash>,
    /// How long connecting took, for the application bar.
    pub connect_latency: Option<Duration>,
    connect_started: Option<Instant>,
    /// Purpose of the next run, and of the one in flight.
    running_purpose: RunPurpose,
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
    pub library: crate::library::Library,
    /// The F3 library/search window.
    pub library_view: Option<LibraryView>,
    /// Save-as / rename prompt.
    pub tab_prompt: Option<TabPrompt>,
    /// Further rows of the current (streamed) result, fetched on demand.
    pub row_stream: Option<RowStream>,
    /// "Export all rows" in progress: what stops it, and the file it writes.
    export_task: Option<(tokio::sync::oneshot::Sender<()>, PathBuf)>,
    pub fetching_more: bool,
    pub completion: Option<Completion>,
    /// Row limit for the next run only.
    next_row_limit: Option<usize>,
    /// Whether the schema navigator is actually on screen (the UI hides it
    /// in narrow terminals even when toggled on).
    pub schema_shown: bool,
    /// The focused pane fills the screen (`z`, Ctrl+K z / F12).
    pub zoomed: bool,
    /// A function's definition on its way is for the navigator's popup.
    routine_definition_pending: bool,
    /// The statement of the EXPLAIN on its way, and whether it's ANALYZE.
    /// And the editor line it starts on.
    plan_request: Option<(String, bool, usize)>,
    /// Pane sizes moved with Ctrl+arrows: the navigator's width (`None`
    /// for a quarter of the screen) and the editor's share of the height,
    /// in percent.
    pub schema_width: Option<u16>,
    pub editor_percent: u16,
    /// Clickable areas from the last frame.
    pub hits: Hits,
    /// For double clicks and drags: the last click (time, column, row), and
    /// whether a drag in the editor is selecting.
    last_click: Option<(Instant, u16, u16)>,
    dragging: bool,
    /// When the connection was last used (for keep-alive checks), and
    /// whether a check is running.
    last_activity: Instant,
    pinging: bool,
    /// The last check of the connection failed (shown in the top bar).
    pub connection_lost: bool,
    /// The heartbeat interval to go back to when it's switched on again.
    heartbeat_secs: u64,
    /// `f` is waiting for the result table's foreign keys to load.
    pending_follow: bool,
    pub schema: Option<SchemaPanel>,
    pub schema_visible: bool,
    pub status: String,
    pub status_is_error: bool,
    /// The status is about a run (its outcome or progress): the results
    /// pane shows it, not the status bar.
    pub status_in_results: bool,
    pub executing: bool,
    /// Timeout and review state of the running query.
    run_extra: RunExtra,
    pub cancelling: bool,
    /// Mode of the open connection.
    pub active_mode: ConnectionMode,
    /// What happens to writes on this connection (Ctrl+K w).
    pub tx_mode: TxMode,
    /// Picking the write mode: the highlighted one.
    pub tx_picker: Option<TxMode>,
    pub txn: TxnState,
    /// A question waiting for y/n.
    pub confirm: Option<Confirm>,
    /// What the statements of the running query do (for transaction tracking).
    last_run_infos: Vec<StatementInfo>,
    pub should_quit: bool,
    clipboard: Option<String>,
    /// A long query ended: announced by the main loop (bell, notification).
    notification: Option<String>,
    /// Whether the terminal window has focus, once the terminal says.
    terminal_focused: Option<bool>,
    /// Bumped whenever in-flight work should be ignored (disconnect, cancelled
    /// connect). Every task's event carries the value it was started under.
    generation: u64,
    /// The running connect or query task, aborted on cancel/disconnect.
    task: Option<JoinHandle<()>>,
    db_tx: mpsc::UnboundedSender<DbEvent>,
    db_rx: mpsc::UnboundedReceiver<DbEvent>,
}

impl App {
    // Long only because it lists every field.
    #[allow(clippy::too_many_lines)]
    pub fn new(store: ConnectionStore, store_warning: Option<String>, data_dir: Option<PathBuf>) -> Self {
        let (db_tx, db_rx) = mpsc::unbounded_channel();
        let projects_selected =
            store.last_project.as_ref().and_then(|id| store.projects.iter().position(|p| &p.id == id)).unwrap_or(0);
        let projects_message = store_warning.map(|w| (w, true)).or_else(|| {
            store.migrated_project.as_ref().map(|_| {
                (
                    format!(
                        "Your saved connections were moved into the '{}' project. \
                         Use m in a project to move connections elsewhere.",
                        store::DEFAULT_PROJECT
                    ),
                    false,
                )
            })
        });
        Self {
            screen: Screen::Projects,
            store,
            current_project: None,
            left_project: None,
            projects_selected,
            projects_message,
            project_prompt: None,
            manager_from_query: false,
            form_stash: None,
            drop_blank_tab: false,
            confirm_delete_project: false,
            moving_to: None,
            containers: None,
            containers_request: 0,
            history: History::load_from(None),
            data_dir,
            editor_height: 1,
            last_run_filled: None,
            menu: None,
            row_form: None,
            pipe_prompt: None,
            last_pipe: String::new(),
            external: None,
            activity: None,
            palette: None,
            last_space: None,
            esc_then_tab: false,
            find: None,
            review: None,
            param_form: None,
            param_values: HashMap::new(),
            drafts: tabs::Drafts::default(),
            #[cfg(test)]
            sandbox: false,
            editor_note: None,
            lint_due: None,
            settings: Settings::default(),
            keymap: crate::keys::KeyMap::default(),
            connections_selected: 0,
            connections_message: None,
            confirm_delete: false,
            loading_profile: false,
            connect_form: ConnectForm::new(),
            focus: Focus::Editor,
            database: None,
            connection_summary: String::new(),
            cell_view: None,
            messages: Vec::new(),
            help_visible: false,
            splash: None,
            connect_latency: None,
            connect_started: None,
            running_purpose: RunPurpose::Query,
            cell_edit: None,
            vim: Vim::default(),
            conns: session::Conns::default(),
            tabs: vec![TabMeta { name: "Query 1".into(), saved_id: None, conn: None }],
            tab: TabState::default(),
            parked: vec![None],
            active_tab: 0,
            library: crate::library::Library::load_from(None),
            library_view: None,
            tab_prompt: None,
            row_stream: None,
            export_task: None,
            fetching_more: false,
            completion: None,
            next_row_limit: None,
            hits: Hits::default(),
            last_click: None,
            last_activity: Instant::now(),
            connection_lost: false,
            heartbeat_secs: 30,
            pinging: false,
            pending_follow: false,
            dragging: false,
            zoomed: false,
            routine_definition_pending: false,
            plan_request: None,
            schema_width: None,
            editor_percent: 40,
            schema_shown: false,
            schema: None,
            schema_visible: false,
            status: String::new(),
            status_is_error: false,
            status_in_results: false,
            run_extra: RunExtra::default(),
            executing: false,
            cancelling: false,
            active_mode: ConnectionMode::Normal,
            tx_mode: TxMode::AutoCommit,
            tx_picker: None,
            txn: TxnState::Idle,
            confirm: None,
            last_run_infos: Vec::new(),
            should_quit: false,
            clipboard: None,
            notification: None,
            terminal_focused: None,
            generation: 0,
            task: None,
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
    /// the terminal is known to have focus (then you saw it end).
    fn notify_if_long(&mut self, what: impl FnOnce(Duration) -> String) {
        let Some(started) = self.run_extra.started.take() else { return };
        let after = self.settings.notify_after;
        if after == 0 || started.elapsed() < Duration::from_secs(after) || self.terminal_focused == Some(true) {
            return;
        }
        self.notification = Some(what(started.elapsed()));
    }

    pub fn engine(&self) -> Engine {
        self.database.as_ref().map_or(self.connect_form.engine, Database::engine)
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
            .ok_or_else(|| format!("No project named '{name}'"))?;
        self.open_project(&id);
        Ok(())
    }

    fn open_project(&mut self, id: &str) {
        if let Err(e) = self.store.set_last_project(id) {
            self.connections_message = Some((format!("Couldn't save last project: {e:#}"), true));
        } else {
            self.connections_message = None;
        }
        if self.current_project.as_deref() != Some(id) {
            self.save_draft();
            self.close_all_connections();
            self.history = History::load_from(self.history_path(id));
            self.load_tabs(id);
            self.connections_selected = 0;
            self.manager_from_query = false;
        }
        self.current_project = Some(id.to_string());
        self.screen = Screen::Connections;
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
    #[cfg_attr(not(test), allow(clippy::unused_self))]
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
            let _ = tx.send(DbEvent { generation, kind });
        }
    }

    fn invalidate_tasks(&mut self) {
        self.generation += 1;
        if let Some(task) = self.task.take() {
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
        let status = if crate::clipboard::over_ssh() && text.len() > crate::clipboard::OSC52_LIMIT {
            #[allow(clippy::cast_precision_loss)] // display only
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
        self.messages.push(Message { time, text, is_error });
        if self.messages.len() > 200 {
            self.messages.remove(0);
        }
    }

    /// Whether keys are going into a text field (so `?` types a `?`).
    pub fn typing(&self) -> bool {
        match self.screen {
            Screen::Projects => self.project_prompt.is_some(),
            Screen::Connections => false,
            Screen::Connect => true,
            Screen::Query => match self.top_overlay() {
                Some(overlay) => overlay.takes_text(),
                None => {
                    self.focus == Focus::Editor
                        || self.project_prompt.is_some()
                        || self.find.is_some()
                        || self.cell_edit.is_some()
                        || self.tab.grid.filter_editing
                        || self.schema.as_ref().is_some_and(|s| s.filter_editing)
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
        self.confirm = Some(Confirm {
            message: "A transaction is open. Quit and roll it back?".to_string(),
            details: vec![],
            danger: true,
            action: ConfirmAction::Quit,
            offer_transaction: false,
        });
    }

    fn set_status(&mut self, status: String, is_error: bool) {
        self.status = status;
        self.status_is_error = is_error;
        self.status_in_results = false;
    }

    #[cfg(test)]
    pub fn set_status_for_tests(&mut self, status: &str) {
        self.set_status(status.to_string(), false);
    }

    #[cfg(test)]
    pub fn show_results_for_tests(&mut self, outcome: QueryOutcome) {
        self.show_results(outcome);
    }

    #[cfg(test)]
    pub fn show_plan_for_tests(&mut self, outcome: QueryOutcome) {
        self.show_plan(outcome);
    }

    fn show_results(&mut self, outcome: QueryOutcome) {
        self.close_cell_popups();
        let sets = self.tab.grid.show(outcome);
        let Some(outcome) = &self.tab.grid.results else { return };

        let elapsed = format_elapsed(outcome.elapsed);
        let mut status = if outcome.statements == 0 {
            "Nothing to run".to_string()
        } else if !outcome.columns.is_empty() {
            if outcome.truncated() {
                format!("Showing first {} of {} rows in {elapsed}", outcome.rows.len(), outcome.total_rows)
            } else {
                format!("{} row(s) in {elapsed}", outcome.rows.len())
            }
        } else {
            format!("{} row(s) affected in {elapsed}", outcome.rows_affected.unwrap_or(0))
        };
        if outcome.statements > 1 {
            let _ = write!(status, " · {} statements", outcome.statements);
        }
        if sets > 1 {
            let _ = write!(status, " · {sets} result sets (< > to switch)");
        }
        self.set_status(status, false);
    }

    /// The popups about one cell of the old result.
    fn close_cell_popups(&mut self) {
        self.cell_view = None;
        self.cell_edit = None;
    }

    /// Shows result set `index` of a script's results.
    fn show_result_set(&mut self, index: usize) {
        if !self.tab.grid.select_set(index) {
            return;
        }
        self.close_cell_popups();
        let Some(next) = &self.tab.grid.results else { return };
        let from = if next.statement > 0 { format!(", from statement {}", next.statement) } else { String::new() };
        let status =
            format!("Result set {}/{}{from}: {} row(s)", index + 1, self.tab.grid.result_sets.len(), next.total_rows);
        self.set_status(status, false);
        self.set_result_tab(ResultTab::Rows);
    }

    /// The next (or previous) result set, wrapping around.
    fn step_result_set(&mut self, forward: bool) {
        match self.tab.grid.step_set(forward) {
            Some(index) => self.show_result_set(index),
            None => {
                self.set_status(
                    "Only one result set: a script shows one per statement that returns rows".into(),
                    false,
                );
            }
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        match self.screen {
            Screen::Query => self.paste_on_query_screen(text),
            Screen::Projects => {
                if let Some(prompt) = &mut self.project_prompt {
                    prompt.text.insert_str(text);
                }
            }
            Screen::Connect if !self.connect_form.connecting => {
                // A pasted connection URL fills in the whole form.
                if looks_like_url(text) {
                    match self.connect_form.apply_url(text.trim()) {
                        Ok(()) => {
                            self.connect_form.error = None;
                            self.connect_form.info = Some("Filled in from the URL".to_string());
                        }
                        Err(e) => self.connect_form.error = Some(e),
                    }
                    return;
                }
                let focus = self.connect_form.focus;
                if let Some(field) = self.connect_form.field_mut(focus) {
                    input::paste_at_end(field, text);
                }
            }
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Messages last until the next key; the MESSAGES tab keeps outcomes.
        if !self.executing {
            self.status.clear();
            self.status_is_error = false;
        }
        self.editor_note = None;
        // A key hides the reopening card; a key typed before it showed counts.
        if let Some(splash) = self.splash.take()
            && splash.shown
        {
            return;
        }
        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1)) {
                self.help_visible = false;
            }
            return;
        }
        if key.code == KeyCode::F(1) {
            self.help_visible = true;
            return;
        }
        // `?` is Ctrl+K on the query screen (where Ctrl+K's menu shows the
        // pane's keys too), and the keys elsewhere.
        if key.code == KeyCode::Char('?') && !self.typing() && !self.popup_open() {
            if matches!(self.screen, Screen::Query) {
                self.open_palette(true);
            } else {
                self.help_visible = true;
            }
            return;
        }
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        // Ctrl+Q quits too, from anywhere (Ctrl+C copies a selection).
        let ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q');
        if let Some(confirm) = self.confirm.take() {
            // A second Ctrl+C or Ctrl+Q confirms quitting.
            if confirm.offer_transaction
                && matches!(key.code, KeyCode::Char('t' | 'T'))
                && let ConfirmAction::Run { sql, offset, filled, .. } = confirm.action
            {
                return self.run_in_transaction(&sql, offset, filled, RunPurpose::Query);
            }
            let yes = matches!(key.code, KeyCode::Char('y' | 'Y'))
                || ((ctrl_c || ctrl_q) && matches!(confirm.action, ConfirmAction::Quit));
            if !yes {
                self.set_status("Cancelled".to_string(), false);
                return;
            }
            match confirm.action {
                ConfirmAction::Run { sql, offset, infos, purpose, filled } => {
                    self.run_writes(sql, offset, infos, purpose, filled);
                }
                ConfirmAction::Disconnect => self.disconnect(),
                ConfirmAction::Quit => self.should_quit = true,
                ConfirmAction::CloseTab => self.close_tab(),
                ConfirmAction::LeaveProject(target, panes) => self.go_to_project(target, panes),
            }
            return;
        }
        if ctrl_q {
            return self.request_quit();
        }
        if ctrl_c {
            // With text selected in the editor, Ctrl+C copies instead of
            // quitting; quitting asks first if a transaction is open.
            if matches!(self.screen, Screen::Query)
                && self.focus == Focus::Editor
                && let Some(text) = self.tab.editor.selected_text()
            {
                self.copy_text(text, "Copied selection");
                return;
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
