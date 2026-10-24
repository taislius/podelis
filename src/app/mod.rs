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
    CheckOutcome, ColumnInfo, ConnectParams, ConnectionMode, Database, Engine, ErrorPos, Page, QueryOutcome,
    RoutineInfo, RowStream, SslMode, TableDetails, TableInfo, TxnState,
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
mod compare;
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
mod palette;
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
            SchemaSort::Name => t!("navigator.by_name"),
            SchemaSort::Rows => t!("navigator.by_rows"),
            SchemaSort::Size => t!("navigator.by_size"),
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
        // Each name lowercased once, to filter and rank it.
        let mut rank = |name: &str| match_rank(name, &needle, &mut buf);
        for section in [SchemaSection::Tables, SchemaSection::Views, SchemaSection::System] {
            let mut ranked: Vec<(u8, usize)> = (0..self.tables.len())
                .filter(|&t| section.holds(&self.tables[t].info))
                .filter_map(|t| Some((rank(&self.tables[t].info.name)?, t)))
                .collect();
            // Biggest first; unknown sizes last. The sort is stable, so
            // ties stay by name. By name while filtering, the best matches
            // first: `products` before `alternative_prod` for "produc".
            match self.sort {
                SchemaSort::Name => ranked.sort_by_key(|&(rank, _)| rank),
                SchemaSort::Rows => ranked.sort_by_key(|&(_, t)| std::cmp::Reverse(self.tables[t].info.rows)),
                SchemaSort::Size => ranked.sort_by_key(|&(_, t)| std::cmp::Reverse(self.tables[t].info.bytes)),
            }
            let matching: Vec<usize> = ranked.into_iter().map(|(_, t)| t).collect();
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
        let mut routines: Vec<(u8, usize)> =
            (0..self.routines.len()).filter_map(|r| Some((rank(&self.routines[r].name)?, r))).collect();
        routines.sort_by_key(|&(rank, _)| rank);
        if !routines.is_empty() {
            nodes.push(SchemaNode::Section(SchemaSection::Routines));
            nodes.extend(routines.into_iter().map(|(_, r)| SchemaNode::Routine(r)));
        }
        nodes
    }
}

/// How well `name` matches the filter `needle` (lowercase), best first: the
/// name itself, a name starting with it, a word in it starting with it
/// (after `_`, `.`, `-` or a space), anywhere else; `None` when it isn't in
/// it at all. Case is folded as the results' filter folds it, in `buf`. No
/// filter: all alike.
fn match_rank(name: &str, needle: &str, buf: &mut String) -> Option<u8> {
    if needle.is_empty() {
        return Some(0);
    }
    let name = results::lowered_for(name, needle, buf).filter(|n| n.contains(needle))?;
    Some(if name == needle {
        0
    } else if name.starts_with(needle) {
        1
    } else if name.match_indices(needle).any(|(i, _)| name[..i].ends_with(['_', '.', '-', ' '])) {
        // (A match right after a separator: `order_p` in `big_order_products` too.)
        2
    } else {
        3
    })
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

/// How a connection check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ping {
    /// The server said no, or the connection broke.
    Failed,
    /// Nothing came back in time: the session may be stuck on a dead link.
    NoAnswer,
}

pub struct DbEvent {
    generation: u64,
    /// A run's outcome, by its id: it outlives connection switches (another
    /// tab shown meanwhile), not a disconnect.
    run: Option<u64>,
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
    /// Another page of a streamed result; true when `M` asked for it (the
    /// first of its rows gets selected, so they're seen arriving).
    MoreRows(Page, bool),
    /// Rows written so far by "export all rows".
    ExportProgress(u64),
    /// "Export all rows" finished: the file, and how many rows (or why not).
    ExportDone(PathBuf, Result<u64, String>),
    MoreFailed(String),
    /// Why, where, the transaction after it, and whether it's a syntax
    /// error or unknown function (maybe a feature the server lacks).
    QueryFailed(String, Option<ErrorPos>, Option<TxnState>, bool),
    /// Why not, and whether the server couldn't be reached at all.
    CancelFailed(String, bool),
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
    ContainersFound(u64, Result<Vec<crate::system::containers::Container>, String>),
    /// The server's sessions, for the activity view.
    ActivityLoaded(Result<Vec<crate::db::Activity>, String>),
    /// A session's query was cancelled or the session ended.
    SessionStopped(Result<String, String>),
    /// A keep-alive check: latency and whether it had to reconnect.
    /// A connection check: its latency and whether it reconnected, or why
    /// not and whether it got no answer at all (`Ping::NoAnswer`).
    Pinged(Result<(Duration, bool), (String, Ping)>),
    /// A connection test's report (title, text).
    Diagnosed(String, String),
    /// The output of piping rows through a command.
    PipeDone(String, Result<String, String>),
    /// Characters of an answer being written, for ask `n`.
    AskProgress(u64, usize),
    /// Ask `n`'s key is slow to come: the keychain (or a password
    /// manager) is asking the user.
    AskWaitingKey(u64),
    /// Ask `n`'s answer: the SQL and what the model said, or why not.
    AskDone(u64, Box<Result<serde_json::Value, ask::AskFailure>>),
    /// A gateway call's answer (the memory window's).
    GatewayDone(memory_view::GatewayPurpose, Box<Result<serde_json::Value, ask::AskFailure>>),
    /// A definition to show (title, text).
    Definition(String, Result<String, String>),
    /// Rows opened in an app (what, the command), or why they couldn't be.
    Opened(String, String, Result<(), String>),
    /// The row a foreign key points to (`f`), looked up.
    Followed(Box<(follow::Followed, Result<QueryOutcome, String>)>),
    /// The background syntax check of `text`: the server's problem, else
    /// the parser's hint, else the first lint warning.
    SyntaxChecked {
        text: String,
        hint: Option<SyntaxHint>,
    },
    /// A read on production, planned: the tables it would read in full.
    Guarded {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
        scans: Vec<(String, f64)>,
    },
    /// A plan made on a connection of its own while a query ran: the
    /// statement, its line, and the plan (`None`: it couldn't be made there).
    PlanAside {
        tab: usize,
        stmt: String,
        line: usize,
        plan: Option<QueryOutcome>,
    },
    /// A schema snapshot saved: its file and how many tables, or why not.
    SnapshotSaved(Result<(PathBuf, usize), String>),
    /// This connection's schema and a saved snapshot of it, to compare.
    SnapshotCompared(Result<(Vec<crate::transfer::diff::TableShape>, crate::transfer::snapshots::Snapshot), String>),
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

/// File formats for exporting results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    /// `INSERT` statements.
    Sql,
    /// A table with `+--+` borders.
    Ascii,
}

/// What every row of a result can be exported as, page by page as they
/// arrive (an ASCII table can't: its widths need every row first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedFormat {
    Csv,
    Json,
    Sql,
}

impl From<ExportFormat> for pipes::OpenFormat {
    fn from(format: ExportFormat) -> Self {
        match format {
            ExportFormat::Csv => pipes::OpenFormat::Csv,
            ExportFormat::Json => pipes::OpenFormat::Json,
            ExportFormat::Sql => pipes::OpenFormat::Sql,
            ExportFormat::Ascii => pipes::OpenFormat::Ascii,
        }
    }
}

impl From<PagedFormat> for ExportFormat {
    fn from(format: PagedFormat) -> Self {
        match format {
            PagedFormat::Csv => ExportFormat::Csv,
            PagedFormat::Json => ExportFormat::Json,
            PagedFormat::Sql => ExportFormat::Sql,
        }
    }
}

impl Run {
    /// A new stream of rows, or none: an `M` waiting for the old one's
    /// page is forgotten with it.
    fn set_stream(&mut self, stream: Option<RowStream>) {
        self.stream = stream;
        self.more_wanted = false;
    }
}

/// The query running, one at a time across the tabs: whose it is, on
/// which connection, since when, and what it's for.
#[derive(Default)]
pub struct Run {
    /// A query (or check, or comparison) is running.
    pub executing: bool,
    /// Which run the events in flight are for.
    id: u64,
    /// When the running query (or check, or comparison) started.
    since: Option<Instant>,
    /// A write, a check or a comparison: every tab stays where it is until
    /// it ends. (A read leaves the others free, and runs on in its tab.)
    holds_tabs: bool,
    /// The connection the running query runs on: cancelled there, whichever
    /// tab (and connection) is shown meanwhile.
    db: Option<Database>,
    /// Purpose of the next run, and of the one in flight.
    purpose: RunPurpose,
    /// Timeout and review state of the running query.
    extra: RunExtra,
    /// Its cancel was asked for and hasn't come back yet.
    pub cancelling: bool,
    /// Cancelling the running query failed: Esc again stops waiting.
    pub cancel_failed: bool,
    /// Further rows of the current (streamed) result, fetched on demand.
    pub stream: Option<RowStream>,
    /// More rows of the stream are on their way.
    pub fetching_more: bool,
    /// `M` was pressed while they were: its rows are fetched once they've
    /// come (for this stream only; [`Run::set_stream`] forgets it).
    more_wanted: bool,
    /// What the statements of the running query do (for transaction tracking).
    infos: Vec<StatementInfo>,
    /// Parameter substitutions in the last run, for mapping error positions.
    filled: Option<Filled>,
    /// The SQL running: a failure says if it uses what the server is too
    /// old for.
    sql: String,
    /// The failure that came is a syntax error or an unknown function
    /// (`db::missing_feature`), for `on_query_failed`.
    missing_feature: bool,
    /// The running connect or query task, aborted on cancel/disconnect.
    task: Option<JoinHandle<()>>,
    /// The statement of the EXPLAIN on its way, and whether it's ANALYZE.
    /// And the editor line it starts on.
    plan_request: Option<(String, bool, usize)>,
    /// F7 while a query ran: the statement to plan (and its line) next.
    queued_plan: Option<(String, usize)>,
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
    /// It's a sandbox step: added to the sandbox when it ends well.
    sandbox_step: Option<sandbox::PendingStep>,
    /// Said before the next result's status (what an edit just did).
    status_prefix: Option<String>,
    /// When it started, to announce a long run's end.
    started: Option<Instant>,
    /// It ended a statement with `\G`: show the result one column per line.
    vertical: bool,
    /// It ran past `long_query`, and the status bar said how to cancel it.
    offered_cancel: bool,
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
    /// Back to before sandbox step `number`: it failed, or (`undo`) the
    /// user undid it.
    SandboxBack { number: usize, undo: bool },
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
    /// Checked: the first problem, if any, and whether it's a syntax error
    /// or unknown function (maybe a feature the server lacks).
    Done(Option<(SyntaxHint, bool)>),
}

/// Something to do once the user answers "y".
pub enum ConfirmAction {
    /// A read on production that would read a whole big table: run it as
    /// it is, without asking again.
    RunHeavy {
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
    },
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
    /// An agent's write waiting for the user: y approves it, anything else
    /// says no.
    AgentWrite(crate::agents::Request),
    /// The project doesn't share its schema with the model asked: `y`
    /// sends it this once, `a` always for the project.
    ShareSchema {
        question: ask::Question,
        project_id: Option<String>,
    },
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
    /// `y` copies the text itself (a message), saying this.
    pub copies_text: Option<&'static str>,
    /// A row a foreign key points to (`f`): `e` opens its lookup (the
    /// tab's name, the SQL) in a tab of its own.
    pub follow: Option<(String, String)>,
}

impl CellView {
    pub fn new(title: String, text: String) -> Self {
        CellView {
            title,
            text,
            scroll: 0,
            record: false,
            sql: None,
            max_scroll: u16::MAX,
            browse: false,
            copies_text: None,
            follow: None,
        }
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
    /// What's said to the user.
    pub notes: messages::Notes,
    /// The panes' layout.
    pub panes: panes::Panes,
    /// The projects and connections screen's state.
    pub manager: projects::Manager,
    /// The settings screen (Ctrl+K ,).
    pub settings_view: Option<settings_view::SettingsView>,
    /// The shown tab's connection; the other tabs' are parked (`Conns`).
    pub connection: session::Connection,
    /// What agents did and asked for (the agents view).
    pub agents: Agents,
    /// The sharing question, waiting while another confirmation is open.
    pending_share: Option<Confirm>,
    pub screen: Screen,
    pub store: ConnectionStore,
    /// Id of the open project.
    pub current_project: Option<String>,
    /// Config folder holding per-project history and editor drafts; `None`
    /// disables persistence (tests).
    data_dir: Option<PathBuf>,
    pub param_form: Option<ParamForm>,
    /// Asking to commit or roll back writes run in a transaction.
    pub review: Option<review::Review>,
    /// The sandbox's window (`Ctrl+K x` while one is open).
    pub sandbox_view: Option<sandbox::SandboxView>,
    /// Where a sandbox's script goes once it's rolled back.
    sandbox_then: Option<sandbox::Then>,
    /// The find/replace bar (Ctrl+F).
    pub find: Option<find::FindBar>,
    /// A pop-up menu of what can be done in the focused pane.
    pub menu: Option<menu::Menu>,
    /// The add-row form (`a`).
    pub row_form: Option<rows::RowForm>,
    /// The command the rows are piped through (`|`), being typed.
    pub pipe_prompt: Option<LineInput>,
    /// `Ctrl+K q`: the question being typed, and one being answered.
    pub ask_prompt: Option<LineInput>,
    pub asking: Option<ask::Asking>,
    /// `Ctrl+K k`: the project's memory.
    pub memory_view: Option<memory_view::MemoryView>,
    /// Numbers the asks, so a stopped one's answer is dropped.
    ask_count: u64,
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
    /// Handling a key, click or paste (see `on_input`).
    handling_input: bool,
    lint_due: Option<Instant>,
    pub settings: Settings,
    pub keymap: crate::config::keys::KeyMap,
    history: History,
    pub focus: Focus,
    /// Full-value popup for the selected cell.
    pub cell_view: Option<CellView>,
    pub help_visible: bool,
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
    /// The F3 library/search window.
    pub library_view: Option<LibraryView>,
    /// Save-as / rename prompt.
    pub tab_prompt: Option<TabPrompt>,
    /// Watch mode: the last query run again every few seconds.
    pub watch: Option<watch::Watch>,
    /// The run starting now is watch mode's (others stop it).
    watch_rerun: bool,
    /// The run starting now is the shown result's again (`M`, after a
    /// row change): it replaces that result instead of keeping a new one.
    same_result: bool,
    /// "Export all rows" in progress: what stops it, and the file it writes.
    export_task: Option<(tokio::sync::oneshot::Sender<()>, PathBuf)>,
    /// The last run id handed out ([`App::next_run_id`]).
    run_ids: u64,
    pub completion: Option<Completion>,
    /// Row limit for the next run only.
    next_row_limit: Option<usize>,
    /// A function's definition on its way is for the navigator's popup.
    routine_definition_pending: bool,
    /// Clickable areas from the last frame.
    pub hits: Hits,
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
    /// Picking the write mode: the highlighted one.
    pub tx_picker: Option<TxMode>,
    /// A question waiting for y/n.
    pub confirm: Option<Confirm>,
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

impl App {
    // Long only because it lists every field.
    #[allow(clippy::too_many_lines)]
    pub fn new(store: ConnectionStore, store_warning: Option<String>, data_dir: Option<PathBuf>) -> Self {
        let (db_tx, db_rx) = mpsc::unbounded_channel();
        let projects_selected =
            store.last_project.as_ref().and_then(|id| store.projects.iter().position(|p| &p.id == id)).unwrap_or(0);
        let projects_message = store_warning.map(|w| (w, true)).or_else(|| {
            store.migrated_project.as_ref().map(|_| (t!("projects.migrated", project = store::DEFAULT_PROJECT), false))
        });
        Self {
            screen: Screen::Projects,
            store,
            current_project: None,
            history: History::load_from(None),
            data_dir,
            menu: None,
            row_form: None,
            pipe_prompt: None,
            ask_prompt: None,
            asking: None,
            memory_view: None,
            ask_count: 0,
            last_pipe: String::new(),
            external: None,
            activity: None,
            palette: None,
            last_space: None,
            esc_then_tab: false,
            find: None,
            review: None,
            sandbox_view: None,
            sandbox_then: None,
            param_form: None,
            param_values: HashMap::new(),
            drafts: tabs::Drafts::default(),
            #[cfg(test)]
            sandbox: false,
            handling_input: false,
            lint_due: None,
            settings: Settings::default(),
            keymap: crate::config::keys::KeyMap::default(),
            focus: Focus::Editor,
            cell_view: None,
            help_visible: false,
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
            library_view: None,
            tab_prompt: None,
            watch: None,
            watch_rerun: false,
            same_result: false,
            export_task: None,
            run_ids: 0,
            completion: None,
            next_row_limit: None,
            hits: Hits::default(),
            last_activity: Instant::now(),
            connection_lost: false,
            heartbeat_secs: 30,
            pinging: false,
            pending_follow: false,
            routine_definition_pending: false,
            tx_picker: None,
            confirm: None,
            should_quit: false,
            clipboard: None,
            notification: None,
            terminal_focused: None,
            generation: 0,
            notes: messages::Notes::default(),
            panes: panes::Panes { editor_percent: 40, editor_height: 1, ..panes::Panes::default() },
            settings_view: None,
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
            pending_share: None,
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
        let Some(started) = self.tab.run.extra.started.take() else { return };
        let after = self.settings.notify_after;
        if after == 0 || started.elapsed() < Duration::from_secs(after) || self.terminal_focused == Some(true) {
            return;
        }
        self.notification = Some(what(started.elapsed()));
    }

    pub fn engine(&self) -> Engine {
        self.connection.database.as_ref().map_or(self.connection.form.engine, Database::engine)
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

    /// Tab `i`'s run: the shown tab's, or a parked one's.
    pub fn tab_run(&self, i: usize) -> Option<&Run> {
        if i == self.active_tab { Some(&self.tab.run) } else { self.parked.get(i)?.as_ref().map(|t| &t.run) }
    }

    /// Whether tab `i`'s query is running.
    pub fn tab_running(&self, i: usize) -> bool {
        self.tab_run(i).is_some_and(|r| r.executing)
    }

    /// The tabs whose queries are running, the shown one's too.
    pub fn running_tabs(&self) -> Vec<usize> {
        (0..self.tabs.len()).filter(|&i| self.tab_running(i)).collect()
    }

    /// The shown tab's query is running.
    pub(super) fn running_here(&self) -> bool {
        self.tab.run.executing
    }

    /// This tab's connection is running a query: its own, or another
    /// tab's on the same connection (a session runs one at a time).
    pub(super) fn conn_busy(&self) -> bool {
        self.tab.run.executing || self.busy_conn_tab().is_some()
    }

    /// A run that keeps every tab where it is: a write, a check, a comparison.
    pub(super) fn blocks_tabs(&self) -> bool {
        self.tab.run.executing && self.tab.run.holds_tabs
    }

    /// A fresh run id, unique across the tabs (events find their tab by it).
    pub(super) fn next_run_id(&mut self) -> u64 {
        self.run_ids += 1;
        self.run_ids
    }

    fn invalidate_tasks(&mut self) {
        self.tab.run.db = None;
        // A check on its way is for the connection before: its answer will
        // be dropped (another generation), so it's no longer awaited.
        self.pinging = false;
        self.generation += 1;
        self.tab.run.id = self.next_run_id();
        self.tab.run.queued_plan = None;
        if let Some(task) = self.tab.run.task.take() {
            task.abort();
        }
    }

    /// Stops every tab's query (the project's tabs are being replaced):
    /// their tasks end, and anything they still send finds no tab.
    pub(super) fn stop_all_runs(&mut self) {
        self.invalidate_tasks();
        for tab in self.parked.iter_mut().flatten() {
            if let Some(task) = tab.run.task.take() {
                task.abort();
            }
            tab.run.set_stream(None);
            tab.run.executing = false;
            tab.run.db = None;
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
        self.confirm = Some(Confirm {
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

    /// What runs, for the results pane's title: with whole seconds once
    /// it's been one, so a long run is seen to go on.
    pub fn running_label(&self) -> Option<String> {
        if !self.running_here() {
            return None;
        }
        let secs = self.tab.run.since.map_or(0, |since| since.elapsed().as_secs());
        Some(match secs {
            0 => t!("run.running_label").to_string(),
            1..60 => format!("{} {secs} s", t!("run.running_label")),
            _ => format!("{} {}", t!("run.running_label"), mm_ss(secs)),
        })
    }

    /// The tab the running query (or check) is for: its own, or for a run
    /// that holds every tab (a write, a check) the shown one.
    pub(super) fn run_tab(&self) -> Option<usize> {
        self.tab.run.executing.then_some(self.active_tab)
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

    #[cfg(test)]
    pub fn set_status_for_tests(&mut self, status: &str) {
        self.set_status(status.to_string(), false);
    }

    #[cfg(test)]
    pub fn show_results_for_tests(&mut self, outcome: QueryOutcome) {
        self.show_results(outcome);
    }

    /// A run of the editor's `range` starting, as `run_now` would: its
    /// own result, kept.
    #[cfg(test)]
    pub fn open_run_for_tests(&mut self, range: std::ops::Range<usize>) {
        let text = self.tab.editor.text();
        self.open_run(Some((&text[range.clone()], range.start)));
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
            t!("run.nothing").to_string()
        } else if !outcome.columns.is_empty() {
            if outcome.truncated() {
                t!("run.showing_first", shown = outcome.rows.len(), total = outcome.total_rows, time = elapsed)
            } else {
                tn!("run.rows_in", outcome.rows.len(), time = elapsed)
            }
        } else {
            tn!("run.affected_in", outcome.rows_affected.unwrap_or(0), time = elapsed)
        };
        if outcome.statements > 1 {
            let _ = write!(status, " · {}", tn!("run.statements", outcome.statements));
        }
        if sets > 1 {
            let _ = write!(status, " · {}", tn!("run.result_sets", sets));
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
        let from = if next.statement > 0 {
            format!(", {}", t!("run.from_statement", n = next.statement))
        } else {
            String::new()
        };
        let status = t!(
            "run.result_set",
            n = index + 1,
            of = self.tab.grid.result_sets.len(),
            from = from,
            rows = tn!("rows.rows", next.total_rows)
        );
        self.set_status(status, false);
        self.set_result_tab(ResultTab::Rows);
    }

    /// The next (or previous) result set, wrapping around.
    fn step_result_set(&mut self, forward: bool) {
        match self.tab.grid.step_set(forward) {
            Some(index) => self.show_result_set(index),
            None => {
                self.set_status(t!("run.one_result_set").into(), false);
            }
        }
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

    /// A key while a confirmation is open: what it answers. `quit_key`:
    /// Ctrl+C or Ctrl+Q, which confirm quitting.
    fn answer_confirm(&mut self, confirm: Confirm, key: KeyEvent, quit_key: bool) {
        // A second Ctrl+C or Ctrl+Q confirms quitting.
        if confirm.offer_transaction
            && matches!(key.code, KeyCode::Char('t' | 'T'))
            && let ConfirmAction::Run { sql, offset, filled, .. } = confirm.action
        {
            return self.run_in_transaction(&sql, offset, filled, RunPurpose::Query);
        }
        let yes =
            matches!(key.code, KeyCode::Char('y' | 'Y')) || (quit_key && matches!(confirm.action, ConfirmAction::Quit));
        if let ConfirmAction::AgentWrite(request) = &confirm.action {
            // Only y, or n / Esc, answer; and not in the first moment
            // (a key being typed as it appeared).
            let settled = self.agents.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            let answer = match key.code {
                KeyCode::Char('y' | 'Y') => Some(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
                _ => None,
            };
            match answer {
                Some(approve) if settled => {
                    let request = request.clone();
                    return self.answer_agent(&request, approve);
                }
                _ => {
                    self.confirm = Some(confirm);
                    return;
                }
            }
        }
        if let ConfirmAction::ShareSchema { question, project_id } = &confirm.action {
            // It came by itself, maybe while typing: only y, a, or n / Esc
            // answer it, and not in its first moment (as an agent's write).
            let settled = self.agents.asked.is_none_or(|at| at.elapsed() >= AGENT_SETTLE);
            let answer = match key.code {
                KeyCode::Char('y' | 'Y') => Some(Some(false)),
                KeyCode::Char('a' | 'A') => Some(Some(true)),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(None),
                _ => None,
            };
            match answer {
                Some(Some(always)) if settled => {
                    return self.answer_share(question.clone(), project_id.as_deref(), always);
                }
                Some(None) if settled => return self.set_status(t!("app.not_sent").into(), false),
                _ => {
                    self.confirm = Some(confirm);
                    return;
                }
            }
        }
        if !yes {
            self.tab.run.queued_plan = None;
            self.set_status("Cancelled".to_string(), false);
            return;
        }
        match confirm.action {
            ConfirmAction::Run { sql, offset, infos, purpose, filled } => {
                self.run_writes(sql, offset, infos, purpose, filled);
            }
            ConfirmAction::RunHeavy { sql, offset, infos, filled } => {
                self.run_writes(sql, offset, infos, RunPurpose::Query, filled);
            }
            ConfirmAction::Disconnect => self.disconnect(),
            ConfirmAction::Quit => self.should_quit = true,
            ConfirmAction::CloseTab => self.close_tab(),
            ConfirmAction::LeaveProject(target, panes) => self.go_to_project(target, panes),
            // Answered above.
            ConfirmAction::AgentWrite(_) | ConfirmAction::ShareSchema { .. } => {}
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
        // Ctrl+Q quits from anywhere (on the query screen Ctrl+C copies).
        let ctrl_q = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q');
        if let Some(confirm) = self.confirm.take() {
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

#[cfg(test)]
mod match_tests {
    #[test]
    fn the_best_matches_come_first() {
        let mut names = vec!["alternative_prod", "order_products", "products", "productsx", "prod", "reproduce"];
        let mut buf = String::new();
        names.sort_by_key(|n| super::match_rank(n, "prod", &mut buf));
        assert_eq!(names, ["prod", "products", "productsx", "alternative_prod", "order_products", "reproduce"]);
        assert_eq!(super::match_rank("cats", "prod", &mut buf), None, "not in it");
        // A filter with a separator in it still starts a word.
        assert_eq!(super::match_rank("big_order_products", "order_p", &mut buf), Some(2));
        assert_eq!(super::match_rank("xorder_px", "order_p", &mut buf), Some(3));
        // Folded as the filter folds it: a Lithuanian name ranks as itself.
        assert_eq!(super::match_rank("Žąsys", "žąsys", &mut buf), Some(0));
        assert_eq!(super::match_rank("ŽĄSYS_seni", "žąs", &mut buf), Some(1));
        // Greek's final sigma, as the filter's text lowercases it.
        assert_eq!(super::match_rank("ΟΔΟΣ", &"ΟΔΟΣ".to_lowercase(), &mut buf), Some(0));
    }
}
