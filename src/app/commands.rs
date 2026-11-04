//! Everything that can be done from a key, a menu or the palette: one list
//! of commands, one answer to whether each applies now (`can`), one way to
//! run it (`run_command`), and the panes' keys as data.
//!
//! [`Action`] stays in `config::keys`: those are the commands `config.toml`
//! can bind. A command here is one of them, or a pane's, or a pick from a
//! list.

use super::*;
use crate::config::keys::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Action(Action),
    // A result's rows.
    FullValue,
    RecordView,
    Vertical,
    Sort,
    FilterRows,
    Chart,
    Follow,
    CopyCell,
    CopyRow,
    /// A menu of the formats to copy the rows in.
    CopyMenu,
    /// A menu of the formats to write the rows to a file in.
    ExportMenu,
    /// A menu of the formats to open the rows in.
    OpenRows,
    PipeRows,
    LoadMore,
    /// A menu of a script's other result sets and the results kept.
    OtherResults,
    NextSet,
    PrevSet,
    EarlierResult,
    LaterResult,
    Pin,
    EditCell,
    AddRow,
    MarkRow,
    DeleteRows,
    /// The rows, in a format, to somewhere.
    Rows {
        to: Dest,
        format: Format,
    },
    // The results pane's tabs and what's in them.
    ShowRows,
    ShowMessages,
    ShowPlan,
    WholeMessage,
    CopyMessage,
    CopyMessages,
    CopyPlanLines,
    CopyWholePlan,
    // The navigator's.
    Expand,
    Definition,
    Related,
    SelectFrom,
    CopyName,
    FilterNames,
    OrderNames,
    SystemTables,
    RefreshSchema,
    // The editor's menu.
    /// The list of every command, at once (Ctrl+K waits for a letter first).
    AllCommands,
    ClipCopy,
    ClipCut,
    ClipPaste,
    // The app's, with no key of their own.
    /// What AI agents did through `podelis mcp`.
    AgentActivity,
    CopyDiagnostics,
    Help,
    Quit,
    /// An open tab, by index.
    SwitchTab(usize),
    /// An open connection (`Ctrl+K j`), by its id: goes to a tab on it.
    GoToConnection(u64),
    /// Compare with another tab's connection (or the only other one).
    CompareSchema(Option<u64>),
    /// Run the last query again every this many seconds.
    Watch(u64),
    /// Save this connection's schema to a file, to compare with later.
    SnapshotSchema,
    /// Compare the schema now with a saved snapshot (by its place in the
    /// list, newest first).
    CompareSnapshot(usize),
    CompareRows(Option<u64>),
}

/// Whether a command applies now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Can {
    Yes,
    /// It doesn't, and why: dimmed in a menu, said when it's chosen.
    /// `refused`: asked of the data or the connection and turned down, said
    /// as an error; otherwise there's just nothing to do it to.
    No {
        why: String,
        refused: bool,
    },
}

impl Can {
    fn not_now(why: &str) -> Can {
        Can::No { why: why.to_string(), refused: false }
    }

    fn refused(why: &str) -> Can {
        Can::No { why: why.to_string(), refused: true }
    }
}

/// What the focused pane shows, which decides its keys and its menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Here {
    Rows,
    /// A plan's table: rows to look at, not to change.
    PlanTable,
    Messages,
    PlanText,
    /// The results pane, with nothing to act on.
    Empty,
    Navigator,
    Editor,
}

/// Keys and what they do, in a pane. The first key of a command is the one
/// its menu item shows.
type Keys = &'static [(KeyCode, Command)];

/// In the results pane, whatever its tab shows.
const RESULTS_KEYS: Keys = &[
    (KeyCode::Char('1'), Command::ShowRows),
    (KeyCode::Char('2'), Command::ShowMessages),
    (KeyCode::Char('3'), Command::ShowPlan),
    (KeyCode::Char('m'), Command::Action(Action::Menu)),
    (KeyCode::Char(','), Command::EarlierResult),
    (KeyCode::Char('.'), Command::LaterResult),
    (KeyCode::Char('p'), Command::Pin),
];

const fn shown(format: Format) -> Command {
    Command::Rows { to: Dest::ExportShown, format }
}

/// Over a grid of rows.
const GRID_KEYS: Keys = &[
    (KeyCode::Enter, Command::FullValue),
    (KeyCode::Char('v'), Command::FullValue),
    (KeyCode::Char('x'), Command::RecordView),
    (KeyCode::Char('X'), Command::Vertical),
    (KeyCode::Char('z'), Command::Action(Action::Zoom)),
    (KeyCode::Char('s'), Command::Sort),
    (KeyCode::Char('/'), Command::FilterRows),
    (KeyCode::Char('b'), Command::Chart),
    (KeyCode::Char('f'), Command::Follow),
    (KeyCode::Char('y'), Command::CopyCell),
    (KeyCode::Char('Y'), Command::CopyRow),
    (KeyCode::Char('C'), Command::CopyMenu),
    (KeyCode::Char('E'), Command::ExportMenu),
    (KeyCode::Char('e'), shown(Format::Csv)),
    (KeyCode::Char('J'), shown(Format::Json)),
    (KeyCode::Char('i'), shown(Format::Sql)),
    (KeyCode::Char('o'), Command::OpenRows),
    (KeyCode::Char('|'), Command::PipeRows),
    (KeyCode::Char('M'), Command::LoadMore),
    (KeyCode::Char('>'), Command::NextSet),
    (KeyCode::Char('<'), Command::PrevSet),
    (KeyCode::Char('c'), Command::EditCell),
    (KeyCode::Char('a'), Command::AddRow),
    (KeyCode::Char(' '), Command::MarkRow),
    (KeyCode::Char('d'), Command::DeleteRows),
];

const MESSAGES_KEYS: Keys = &[
    (KeyCode::Enter, Command::WholeMessage),
    (KeyCode::Char('v'), Command::WholeMessage),
    (KeyCode::Char('y'), Command::CopyMessage),
    (KeyCode::Char('Y'), Command::CopyMessages),
];

const PLAN_KEYS: Keys = &[(KeyCode::Char('y'), Command::CopyPlanLines), (KeyCode::Char('Y'), Command::CopyWholePlan)];

const NAVIGATOR_KEYS: Keys = &[
    (KeyCode::Enter, Command::Expand),
    (KeyCode::Char('d'), Command::Definition),
    (KeyCode::Char('f'), Command::Related),
    (KeyCode::Char('s'), Command::SelectFrom),
    (KeyCode::Char('y'), Command::CopyName),
    (KeyCode::Char('/'), Command::FilterNames),
    (KeyCode::Char('o'), Command::OrderNames),
    (KeyCode::Char('.'), Command::SystemTables),
    (KeyCode::Char('r'), Command::RefreshSchema),
    (KeyCode::Char('z'), Command::Action(Action::Zoom)),
    (KeyCode::Char('m'), Command::Action(Action::Menu)),
    (KeyCode::Char(' '), Command::Action(Action::Palette)),
];

/// The tables a pane's keys are in, its own first.
fn keys_of(here: Here) -> &'static [Keys] {
    match here {
        Here::Rows | Here::PlanTable => &[GRID_KEYS, RESULTS_KEYS],
        Here::Messages => &[MESSAGES_KEYS, RESULTS_KEYS],
        Here::PlanText => &[PLAN_KEYS, RESULTS_KEYS],
        Here::Empty => &[RESULTS_KEYS],
        Here::Navigator => &[NAVIGATOR_KEYS],
        Here::Editor => &[],
    }
}

/// A pane's letters are plain ones: Ctrl+D or Alt+E isn't `d` or `e`. (Shift
/// is how `E` is typed.)
pub(super) fn is_plain(key: KeyEvent) -> bool {
    (key.modifiers - KeyModifiers::SHIFT).is_empty()
}

fn key_label(code: KeyCode) -> String {
    match code {
        KeyCode::Enter => "↵".to_string(),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// The pane's keys for help, a command's keys together: (keys, command).
pub fn pane_key_list(here: Here) -> Vec<(String, Command)> {
    let mut list: Vec<(String, Command)> = Vec::new();
    // Those of the results' every tab are listed once, with the rows'.
    let tables = match here {
        Here::Messages | Here::PlanText => &keys_of(here)[..1],
        _ => keys_of(here),
    };
    for &(code, command) in tables.iter().flat_map(|keys| keys.iter()) {
        match list.iter_mut().find(|(_, c)| *c == command) {
            Some((keys, _)) => {
                keys.push_str(" · ");
                keys.push_str(&key_label(code));
            }
            None => list.push((key_label(code), command)),
        }
    }
    list
}

/// A group of what `Action`s do: how Ctrl+K's menu and the keys are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Transaction,
    Run,
    Editor,
    Ai,
    Connection,
    Tabs,
    View,
    App,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Transaction => t!("group.transaction"),
            Group::Run => t!("group.run"),
            Group::Editor => t!("group.editor"),
            Group::Ai => t!("group.ai"),
            Group::Connection => t!("group.connection"),
            Group::Tabs => t!("group.tabs"),
            Group::View => t!("group.view"),
            Group::App => t!("group.app"),
        }
    }
}

/// Every action, in its group and place.
pub const ACTION_GROUPS: &[(Group, &[Action])] = &[
    (Group::Transaction, &[Action::TxMode, Action::Commit, Action::Rollback]),
    (Group::Run, &[Action::Run, Action::RunStatement, Action::Explain, Action::ExplainAnalyze]),
    (
        Group::Editor,
        &[
            Action::Format,
            Action::Comment,
            Action::SelectStatement,
            Action::ZoomStatement,
            Action::NextWarning,
            Action::InsertSnippet,
            Action::SaveStatement,
            Action::Complete,
            Action::Find,
            Action::Undo,
            Action::Redo,
            Action::HistoryPrev,
            Action::HistoryNext,
        ],
    ),
    (Group::Ai, &[Action::Ask, Action::AiExplain, Action::Memory]),
    (
        Group::Connection,
        &[
            Action::OpenConnection,
            Action::Disconnect,
            Action::ConnectionList,
            Action::Projects,
            Action::Activity,
            Action::ConnectionInfo,
            Action::Heartbeat,
        ],
    ),
    (
        Group::Tabs,
        &[
            Action::TabList,
            Action::RenameTab,
            Action::Library,
            Action::NewTab,
            Action::CloseTab,
            Action::SaveTab,
            Action::PrevTab,
            Action::NextTab,
        ],
    ),
    (Group::View, &[Action::ToggleSchema, Action::Zoom]),
    (Group::App, &[Action::Menu, Action::Settings, Action::Palette]),
];

impl Command {
    /// What the palette searches, after the actions: everything without a
    /// list to pick from. The editor's clipboard has its usual keys.
    pub(super) fn all() -> Vec<Command> {
        let mut out: Vec<Command> = Action::all().filter(|a| *a != Action::Palette).map(Command::Action).collect();
        out.extend([
            Command::RecordView,
            Command::FullValue,
            Command::Vertical,
            Command::Sort,
            Command::FilterRows,
            Command::Chart,
            Command::Follow,
            Command::CopyCell,
            Command::CopyRow,
            Command::CopyMenu,
            Command::ExportMenu,
            Command::OtherResults,
        ]);
        for to in [Dest::Copy, Dest::ExportShown, Dest::ExportAll, Dest::Open] {
            out.extend(Format::for_dest(to).map(|format| Command::Rows { to, format }));
        }
        out.extend([
            Command::OpenRows,
            Command::PipeRows,
            Command::LoadMore,
            Command::NextSet,
            Command::PrevSet,
            Command::EarlierResult,
            Command::LaterResult,
            Command::Pin,
            Command::EditCell,
            Command::AddRow,
            Command::MarkRow,
            Command::DeleteRows,
            Command::ShowRows,
            Command::ShowMessages,
            Command::ShowPlan,
            Command::WholeMessage,
            Command::CopyMessage,
            Command::CopyMessages,
            Command::CopyPlanLines,
            Command::CopyWholePlan,
            Command::Expand,
            Command::Definition,
            Command::Related,
            Command::SelectFrom,
            Command::CopyName,
            Command::FilterNames,
            Command::OrderNames,
            Command::SystemTables,
            Command::RefreshSchema,
            Command::AgentActivity,
            Command::CopyDiagnostics,
            Command::Help,
            Command::Quit,
        ]);
        out
    }

    /// One wording, in every menu and the palette (an action's own longer
    /// description is what the palette searches: [`Action::describe`]).
    pub fn label(self, app: &App) -> String {
        let said: &'static str = match self {
            Command::Action(a) => return Self::action_label(a, app).to_string(),
            Command::Rows { to, format } => {
                let format = format.phrase();
                return match to {
                    Dest::Copy => t!("menu.rows_copy", format = format),
                    Dest::ExportShown => t!("menu.rows_export", format = format),
                    Dest::ExportAll => t!("menu.rows_export_all", format = format),
                    Dest::Open => t!("menu.rows_open", format = format),
                };
            }
            Command::FullValue => t!("menu.full_value"),
            Command::RecordView => t!("menu.row_vertical"),
            Command::Vertical => t!("menu.vertical"),
            Command::Sort => t!("menu.sort"),
            Command::FilterRows => t!("menu.filter"),
            Command::Chart => t!("menu.chart"),
            Command::Follow => t!("menu.follow"),
            Command::CopyCell => t!("menu.copy_cell"),
            Command::CopyRow => t!("menu.copy_row"),
            Command::CopyMenu => t!("menu.copy_rows_as"),
            Command::ExportMenu => t!("menu.export_rows"),
            Command::OpenRows => t!("menu.open_rows"),
            Command::PipeRows => t!("menu.pipe"),
            Command::LoadMore => t!("menu.load_more"),
            Command::OtherResults => t!("menu.other_results"),
            Command::NextSet => t!("menu.next_set"),
            Command::PrevSet => t!("menu.prev_set"),
            Command::EarlierResult => t!("menu.earlier_result"),
            Command::LaterResult => t!("menu.later_result"),
            Command::Pin if app.tab.shown_run().is_some_and(|r| r.pinned) => t!("menu.unpin"),
            Command::Pin => t!("menu.pin"),
            Command::EditCell => t!("menu.edit_cell"),
            Command::AddRow => t!("menu.add_row"),
            Command::MarkRow => t!("menu.mark_row"),
            Command::DeleteRows => t!("menu.delete_rows"),
            Command::ShowRows => t!("menu.show_rows"),
            Command::ShowMessages => t!("menu.show_messages"),
            Command::ShowPlan => t!("menu.show_plan"),
            Command::WholeMessage => t!("menu.whole_message"),
            Command::CopyMessage => t!("menu.copy_message"),
            Command::CopyMessages => t!("menu.copy_messages"),
            Command::CopyPlanLines => t!("menu.copy_plan_lines"),
            Command::CopyWholePlan => t!("menu.copy_whole_plan"),
            Command::Expand => t!("menu.expand"),
            Command::Definition => t!("menu.definition"),
            Command::Related => t!("menu.related"),
            Command::SelectFrom => t!("menu.select_from"),
            Command::CopyName => t!("menu.copy_name"),
            Command::FilterNames => t!("menu.filter_name"),
            Command::OrderNames => t!("menu.order"),
            Command::SystemTables => t!("menu.system"),
            Command::RefreshSchema => t!("menu.refresh"),
            Command::AllCommands => t!("menu.all_commands"),
            Command::ClipCopy => t!("menu.copy"),
            Command::ClipCut => t!("menu.cut"),
            Command::ClipPaste => t!("menu.paste"),
            Command::AgentActivity => t!("palette.agents"),
            Command::CopyDiagnostics => t!("palette.diagnostics"),
            Command::Help => t!("palette.help"),
            Command::Quit => t!("palette.quit"),
            Command::SwitchTab(_) => t!("palette.switch_tab"),
            Command::GoToConnection(_) => t!("menu.connection_list"),
            Command::CompareSchema(_) => t!("palette.compare_schema"),
            Command::SnapshotSchema => t!("palette.snapshot"),
            Command::Watch(_) => t!("palette.watch"),
            Command::CompareSnapshot(_) => t!("palette.compare_snapshot"),
            Command::CompareRows(_) => t!("palette.compare_rows"),
        };
        said.to_string()
    }

    /// An action's words in a menu: short, where its description is long.
    fn action_label(action: Action, app: &App) -> &'static str {
        match action {
            Action::Run => t!("menu.run"),
            Action::SelectStatement => t!("menu.select_statement"),
            Action::ZoomStatement if app.panes.statement_zoom => t!("menu.unzoom_statement"),
            Action::ZoomStatement => t!("menu.zoom_statement"),
            Action::Explain => t!("menu.explain"),
            Action::ExplainAnalyze => t!("menu.explain_analyze"),
            Action::AiExplain => t!("menu.ai_explain"),
            Action::TxMode => t!("menu.tx_mode"),
            Action::InsertSnippet => t!("menu.snippet"),
            Action::SaveStatement => t!("menu.save_statement"),
            Action::SaveTab => t!("menu.save"),
            Action::Library => t!("menu.library"),
            Action::CloseTab => t!("menu.close_tab"),
            Action::Zoom => t!("menu.zoom"),
            other => other.describe(),
        }
    }

    /// What the palette lists: the description, with the tab's name.
    pub fn text(self, app: &App) -> String {
        match self {
            Command::Action(a) => a.describe().to_string(),
            // Longer than in a menu: the palette has no pane around it, and
            // these words are what's searched for.
            Command::PipeRows => t!("palette.pipe").to_string(),
            Command::OpenRows => t!("palette.open_rows").to_string(),
            Command::RefreshSchema => t!("palette.refresh").to_string(),
            Command::Definition => t!("palette.definition").to_string(),
            Command::Related => t!("palette.related").to_string(),
            Command::CopyName => t!("palette.copy_name").to_string(),
            Command::FilterNames => t!("palette.filter_names").to_string(),
            Command::OrderNames => t!("palette.order_names").to_string(),
            Command::SystemTables => t!("palette.system_tables").to_string(),
            Command::Rows { to: Dest::ExportAll, .. } => {
                format!("{} {}", self.label(app), t!("palette.not_just_loaded"))
            }
            Command::SwitchTab(i) if app.popups.palette.as_ref().is_some_and(palette::Palette::tabs_only) => {
                app.tab_entry(i)
            }
            Command::SwitchTab(i) => {
                format!("{}: {}", t!("palette.switch_tab"), app.tabs.get(i).map_or("", |t| t.name.as_str()))
            }
            Command::GoToConnection(id) => app.connection_entry(id),
            Command::Watch(0) => t!("palette.stop_watch").to_string(),
            Command::Watch(s) => t!("palette.watch_every", s = s),
            Command::CompareSnapshot(i) => match app.popups.palette.as_ref().and_then(|p| p.snapshot(i)) {
                Some(s) => t!("palette.compare_with_snapshot", connection = s.connection, taken = s.taken),
                None => self.label(app),
            },
            Command::CompareSchema(Some(id)) | Command::CompareRows(Some(id)) => {
                let name = app.other_connections().into_iter().find(|(i, _)| *i == id).map(|(_, n)| n);
                let name = name.unwrap_or_default();
                if matches!(self, Command::CompareSchema(_)) {
                    t!("palette.compare_schema_with", name = name)
                } else {
                    t!("palette.compare_rows_with", name = name)
                }
            }
            _ => self.label(app),
        }
    }

    /// The pane's key for it in `here`, its first.
    pub fn key_code_here(self, here: Here) -> Option<KeyCode> {
        keys_of(here).iter().flat_map(|keys| keys.iter()).find(|(_, c)| *c == self).map(|(code, _)| *code)
    }

    /// The key beside it in a menu in `here`: one, the first.
    pub fn key_here(self, app: &App, here: Here) -> String {
        let leader = || app.keymap.first_label(Action::Palette).to_string();
        match self {
            Command::ClipCopy => return "Ctrl+C".to_string(),
            Command::ClipCut => return "Ctrl+X".to_string(),
            Command::ClipPaste => return "Ctrl+V".to_string(),
            Command::AllCommands => return format!("{} /", leader()),
            _ => {}
        }
        if let Some(code) = self.key_code_here(here) {
            return key_label(code);
        }
        match self {
            Command::Action(a) => app.keymap.first_label(a).to_string(),
            _ => String::new(),
        }
    }

    /// The key that does it directly, if any, and in which pane: for the
    /// palette, which is opened from anywhere.
    pub fn keys(self, app: &App) -> String {
        match self {
            Command::Action(a) => return app.keymap.label(a).to_string(),
            Command::Help => return "F1 · ?".into(),
            Command::Quit => return "Ctrl+Q".into(),
            Command::SwitchTab(i) if i < 9 => return format!("Ctrl+K {}", i + 1),
            // `C` opens the copy menu; `C` again copies this.
            Command::Rows { to: Dest::Copy, format: Format::Markdown } => {
                return format!("C C ({})", t!("palette.in_results"));
            }
            _ => {}
        }
        let panes = [(Here::Rows, t!("palette.in_results")), (Here::Messages, t!("palette.in_results"))];
        let panes = panes
            .into_iter()
            .chain([(Here::PlanText, t!("palette.in_results")), (Here::Navigator, t!("palette.in_navigator"))]);
        for (here, pane) in panes {
            let key = self.key_here(app, here);
            if !key.is_empty() {
                return format!("{key} ({pane})");
            }
        }
        String::new()
    }
}

#[cfg(test)]
impl Command {
    pub fn all_for_tests() -> Vec<Command> {
        Command::all()
    }
}

#[cfg(test)]
impl App {
    pub fn run_command_for_tests(&mut self, command: Command) {
        self.run_command(command);
    }

    pub fn set_result_tab_for_tests(&mut self, tab: ResultTab) {
        self.tab.result_tab = tab;
    }
}

impl App {
    /// What the focused pane shows.
    pub fn here(&self) -> Here {
        match self.focus {
            Focus::Results => {
                let has_columns = self.tab.grid.results.as_ref().is_some_and(|r| !r.columns.is_empty());
                if self.grid_shown() && has_columns {
                    if self.tab.plan_swapped { Here::PlanTable } else { Here::Rows }
                } else if self.tab.result_tab == ResultTab::Messages && !self.notes.log.is_empty() {
                    Here::Messages
                } else if self.tab.result_tab == ResultTab::Plan && !self.grid_shown() && self.tab.plan.is_some() {
                    Here::PlanText
                } else {
                    Here::Empty
                }
            }
            Focus::Schema if self.connection.schema.is_some() => Here::Navigator,
            Focus::Schema | Focus::Editor => Here::Editor,
        }
    }

    /// The command `code` runs in the focused pane.
    pub(super) fn pane_command(&self, key: KeyEvent) -> Option<Command> {
        if !is_plain(key) {
            return None;
        }
        keys_of(self.here()).iter().flat_map(|keys| keys.iter()).find(|(k, _)| *k == key.code).map(|(_, c)| *c)
    }

    /// Whether `command` applies now, and why not.
    pub fn can(&self, command: Command) -> Can {
        self.can_with(command, &std::cell::OnceCell::new())
    }

    /// `can`, with what a whole menu's items share: whether the result can
    /// be edited, which parses its statement, asked once.
    pub(super) fn can_with(&self, command: Command, editable: &std::cell::OnceCell<Result<(), String>>) -> Can {
        use Command as C;
        let editable = || editable.get_or_init(|| self.edit_table().map(drop));
        let results = self.tab.grid.results.as_ref().filter(|r| !r.columns.is_empty());
        let no_rows = self.tab.grid.view.is_empty();
        let plan = self.tab.plan_swapped;
        let runs = self.tab.runs.len();
        let at = self.tab.run_at;
        let sets = self.tab.grid.result_sets.len();
        let no = |cannot: bool, why: &str| if cannot { Can::not_now(why) } else { Can::Yes };
        let not_for_plan = || Can::refused(t!("results.not_for_plan"));
        match command {
            C::EditCell | C::AddRow | C::DeleteRows | C::Follow | C::LoadMore | C::NextSet | C::PrevSet if plan => {
                not_for_plan()
            }
            C::EarlierResult | C::LaterResult | C::Pin | C::OtherResults if plan => not_for_plan(),
            C::Rows { to, format }
                if plan && (to == Dest::ExportAll || matches!(format, Format::Sql | Format::Parquet)) =>
            {
                not_for_plan()
            }
            // (Which column it is, the edit itself says when it's chosen.)
            C::EditCell => match editable() {
                Ok(()) => no(no_rows, t!("commands.no_rows")),
                Err(why) => Can::refused(&t!("edit.cant", why = why)),
            },
            C::AddRow => match editable() {
                Ok(()) => Can::Yes,
                Err(why) => Can::refused(&t!("rows.cant_add", why = why)),
            },
            // Marked rows count even with a filter hiding them.
            C::DeleteRows => match editable() {
                // (With rows on show one is selected when it runs: `run_command`.)
                Ok(()) => no(self.tab.grid.marked.is_empty() && no_rows, t!("rows.none_selected")),
                Err(why) => Can::refused(&t!("rows.cant_delete", why = why)),
            },
            C::FullValue | C::RecordView | C::CopyCell | C::CopyRow | C::MarkRow | C::Follow => {
                no(results.is_none() || no_rows, t!("commands.no_rows"))
            }
            C::PipeRows => no(no_rows, t!("commands.no_rows")),
            // These take the marked rows, shown or not, else those shown.
            C::OpenRows | C::CopyMenu | C::Rows { to: Dest::Copy | Dest::Open, .. } => {
                no(self.tab.grid.marked.is_empty() && no_rows, t!("commands.no_rows"))
            }
            C::Sort | C::FilterRows | C::Chart | C::Vertical | C::ExportMenu | C::LoadMore => {
                no(results.is_none(), t!("commands.no_rows"))
            }
            C::Rows { to: Dest::ExportShown | Dest::ExportAll, .. } => no(results.is_none(), t!("commands.no_rows")),
            C::NextSet | C::PrevSet => no(sets < 2, t!("run.one_result_set")),
            C::EarlierResult | C::LaterResult if runs < 2 => Can::not_now(t!("runs.none_kept")),
            C::EarlierResult => no(at == 0, t!("runs.oldest")),
            C::LaterResult => no(at + 1 >= runs, t!("runs.newest")),
            C::Pin => no(self.tab.runs.get(at).is_none(), t!("runs.nothing_to_pin")),
            C::OtherResults => no(sets < 2 && runs == 0, t!("runs.none_kept")),
            C::WholeMessage | C::CopyMessage | C::CopyMessages => {
                no(self.notes.log.is_empty(), t!("commands.no_messages"))
            }
            C::CopyPlanLines | C::CopyWholePlan => no(self.tab.plan.is_none(), t!("commands.no_plan")),
            // Loading it makes the panel: a connection is all it needs.
            C::RefreshSchema => no(self.connection.database.is_none(), t!("info.not_connected")),
            // Its keys would go to a navigator that isn't drawn.
            C::FilterNames | C::Expand if self.hits.navigator_too_narrow => {
                Can::not_now(t!("commands.navigator_too_narrow"))
            }
            C::SelectFrom | C::Related | C::Definition if self.connection.schema.is_some() => {
                let node = self.selected_node();
                let is_routine = matches!(node, Some(SchemaNode::Routine(_)));
                if command == C::Definition && is_routine {
                    Can::Yes
                } else if node.and_then(SchemaNode::table).is_none() {
                    Can::not_now(t!("commands.no_table"))
                } else {
                    no(command == C::SelectFrom && self.engine().dialect().is_none(), t!("commands.no_sql"))
                }
            }
            C::Expand
            | C::Definition
            | C::Related
            | C::SelectFrom
            | C::CopyName
            | C::FilterNames
            | C::OrderNames
            | C::SystemTables => no(self.connection.schema.is_none(), t!("commands.no_navigator")),
            C::CompareSchema(None) if self.other_connections().is_empty() => Can::refused(t!("compare.need_other")),
            // As Ctrl+C: the selection, else the line the cursor is on.
            C::ClipCopy => {
                no(self.tab.editor.selection().is_none() && self.editor_line().is_none(), t!("copy.nothing"))
            }
            C::ClipCut => no(self.tab.editor.selection().is_none(), t!("commands.no_selection")),
            C::Action(Action::Commit | Action::Rollback) if self.connection.txn == TxnState::Idle => {
                Can::refused(t!("review.none_open"))
            }
            _ => Can::Yes,
        }
    }

    /// A command from the palette may be for a pane that isn't on show, or
    /// that doesn't have the keys: there, for what's done there, so what's
    /// typed next goes to it. (A result's command over a plan's table is
    /// refused by `can`, rather than taken away from the plan being read.)
    fn go_to_pane(&mut self, command: Command) {
        use Command as C;
        match command {
            // Not the rows' tab: it's for what's kept, seen from any of them.
            C::OtherResults | C::ShowRows | C::ShowMessages | C::ShowPlan => self.focus = Focus::Results,
            C::FilterRows
            | C::Sort
            | C::MarkRow
            | C::Vertical
            | C::Follow
            | C::FullValue
            | C::RecordView
            | C::CopyMenu
            | C::ExportMenu
            | C::OpenRows => {
                self.focus = Focus::Results;
                if !self.grid_shown() {
                    self.set_result_tab(ResultTab::Rows);
                }
            }
            C::Chart | C::CopyCell | C::CopyRow | C::LoadMore | C::NextSet | C::PrevSet => {
                if !self.grid_shown() {
                    self.set_result_tab(ResultTab::Rows);
                }
            }
            C::FilterNames | C::Expand => {
                self.panes.schema_visible = true;
                self.focus = Focus::Schema;
            }
            C::Definition
            | C::Related
            | C::SelectFrom
            | C::CopyName
            | C::OrderNames
            | C::SystemTables
            | C::RefreshSchema => {
                self.panes.schema_visible = true;
            }
            _ => {}
        }
    }

    /// The rows on show, and with `focus` the keys there.
    fn go_to_rows(&mut self, focus: bool) {
        if focus {
            self.focus = Focus::Results;
        }
        if !self.grid_shown() {
            self.set_result_tab(ResultTab::Rows);
        }
    }

    /// What every key, menu item and palette entry ends in.
    pub(super) fn run_command(&mut self, command: Command) {
        use Command as C;
        // A row to act on, from the start.
        if self.tab.grid.table_state.selected().is_none() && !self.tab.grid.view.is_empty() {
            self.tab.grid.table_state.select(Some(0));
        }
        // These say why themselves, from the one parse of the statement.
        let says_itself = matches!(command, C::EditCell | C::AddRow | C::DeleteRows) && !self.tab.plan_swapped;
        if !says_itself && let Can::No { why, refused } = self.can(command) {
            return self.set_status(why, refused);
        }
        self.go_to_pane(command);
        match command {
            C::Action(a) => self.run_action(a),
            C::FullValue => self.open_cell_view(),
            C::RecordView => self.open_record_view(),
            C::Vertical => self.toggle_vertical(),
            C::Sort => self.cycle_sort(),
            C::FilterRows => self.tab.grid.filter_editing = true,
            C::Chart => self.chart_column(),
            C::Follow => self.follow_foreign_key(),
            C::CopyCell => self.copy_cell(),
            C::CopyRow => self.copy_row(),
            C::CopyMenu => self.open_rows_menu(Dest::Copy),
            C::ExportMenu => self.open_export_menu(None),
            C::OpenRows => self.open_in_viewer(),
            C::PipeRows => self.open_pipe_prompt(),
            C::LoadMore => self.load_more(),
            C::OtherResults => self.open_other_results_menu(),
            C::NextSet => self.step_result_set(true),
            C::PrevSet => self.step_result_set(false),
            C::EarlierResult => self.step_run(false),
            C::LaterResult => self.step_run(true),
            C::Pin => self.toggle_pin_run(),
            // These say why not themselves; they go to the rows only if they
            // go ahead.
            C::EditCell => {
                self.start_cell_edit();
                if self.cell_edit.is_some() {
                    self.go_to_rows(true);
                }
            }
            C::AddRow => {
                self.start_row_insert();
                if self.popups.row_form.is_some() {
                    self.go_to_rows(false);
                }
            }
            C::MarkRow => self.toggle_mark(),
            C::DeleteRows => {
                self.delete_rows();
                if self.popups.confirm.is_some() {
                    self.go_to_rows(false);
                }
            }
            C::Rows { to: Dest::Copy, format } => self.copy_rows_as(format),
            C::Rows { to: Dest::ExportShown, format } => self.export(format),
            C::Rows { to: Dest::ExportAll, format } => self.export_all(format),
            C::Rows { to: Dest::Open, format } => self.open_rows_as(format),
            C::ShowRows => self.set_result_tab(ResultTab::Rows),
            C::ShowMessages => self.set_result_tab(ResultTab::Messages),
            C::ShowPlan => self.set_result_tab(ResultTab::Plan),
            C::WholeMessage => self.open_message(),
            C::CopyMessage => self.copy_message(),
            C::CopyMessages => self.copy_messages(),
            C::CopyPlanLines => self.copy_plan(false),
            C::CopyWholePlan => self.copy_plan(true),
            C::Expand => self.expand_selected(),
            C::Definition => self.show_selected_definition(),
            C::Related => self.show_selected_related(),
            C::SelectFrom => self.select_from_selected(),
            C::CopyName => self.copy_schema_name(),
            C::FilterNames => {
                if let Some(panel) = &mut self.connection.schema {
                    panel.filter_editing = true;
                }
            }
            C::OrderNames => self.cycle_schema_sort(),
            C::SystemTables => self.toggle_system_tables(),
            C::RefreshSchema => self.load_catalog(),
            C::AllCommands => self.open_command_list(""),
            C::ClipCopy => self.copy_focused(),
            C::ClipCut => self.cut_selection(),
            C::ClipPaste => self.paste_clipboard(),
            C::AgentActivity => self.open_agents(),
            C::CopyDiagnostics => {
                let text = self.diagnostics();
                self.copy_text(text, t!("palette.copied_diagnostics"));
            }
            C::Help => self.help_visible = true,
            C::Quit => self.request_quit(),
            C::SwitchTab(i) => self.switch_tab(i),
            C::GoToConnection(id) => self.go_to_connection(id),
            // Several to compare with: they're picked from the palette's list.
            C::CompareSchema(None) if self.other_connections().len() > 1 => {
                self.open_command_list(t!("palette.compare_schema_with", name = "").trim());
            }
            C::CompareSchema(id) => self.compare_schema(id),
            C::SnapshotSchema => self.snapshot_schema(),
            C::Watch(0) => self.stop_watch(""),
            C::Watch(s) => self.start_watch(s),
            C::CompareSnapshot(i) => self.compare_snapshot(i),
            C::CompareRows(id) => self.compare_rows(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_means_one_thing_in_a_pane() {
        for keys in [RESULTS_KEYS, GRID_KEYS, MESSAGES_KEYS, PLAN_KEYS, NAVIGATOR_KEYS] {
            for (i, (code, _)) in keys.iter().enumerate() {
                assert!(!keys[i + 1..].iter().any(|(other, _)| other == code), "{code:?} twice");
            }
        }
        for keys in [GRID_KEYS, MESSAGES_KEYS, PLAN_KEYS] {
            for (code, _) in keys {
                assert!(!RESULTS_KEYS.iter().any(|(other, _)| other == code), "{code:?} in both");
            }
        }
    }

    #[test]
    fn every_action_is_in_one_group() {
        for action in Action::all() {
            let places = ACTION_GROUPS.iter().flat_map(|(_, actions)| actions.iter()).filter(|a| **a == action).count();
            assert_eq!(places, 1, "{action:?}");
        }
    }

    #[test]
    fn parquet_is_never_copied_and_every_row_goes_page_by_page() {
        assert!(!Format::Parquet.supports(Dest::Copy));
        assert!(Format::Tsv.supports(Dest::Copy));
        let all: Vec<Format> = Format::for_dest(Dest::ExportAll).collect();
        assert_eq!(all, [Format::Csv, Format::Json, Format::Sql, Format::Parquet]);
    }
}
