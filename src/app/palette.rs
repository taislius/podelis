//! Ctrl+K: first a menu of one-letter actions and the focused pane's keys;
//! typing anything else searches every action by name (the palette).

use super::*;
use crate::keys::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Action(Action),
    RecordView,
    ExportCsv,
    ExportJson,
    ExportInserts,
    /// Every row, not just those loaded.
    ExportAll(ExportFormat),
    /// The marked rows (or those shown) to the clipboard.
    CopyRows(ExportFormat),
    /// What AI agents did through `podelis mcp`.
    AgentActivity,
    LoadMore,
    PipeRows,
    OpenRows,
    /// The rows in this format, in its app.
    OpenRowsAs(super::pipes::OpenFormat),
    RefreshSchema,
    CopyDiagnostics,
    Help,
    Quit,
    /// An open tab, by index.
    SwitchTab(usize),
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

impl Command {
    fn all() -> Vec<Command> {
        let mut out: Vec<Command> = Action::all().filter(|a| *a != Action::Palette).map(Command::Action).collect();
        out.extend([
            Command::RecordView,
            Command::ExportCsv,
            Command::ExportJson,
            Command::ExportInserts,
            Command::ExportAll(ExportFormat::Csv),
            Command::ExportAll(ExportFormat::Json),
            Command::ExportAll(ExportFormat::Sql),
            Command::CopyRows(ExportFormat::Csv),
            Command::CopyRows(ExportFormat::Json),
            Command::CopyRows(ExportFormat::Sql),
            Command::AgentActivity,
            Command::LoadMore,
            Command::PipeRows,
            Command::OpenRows,
            Command::RefreshSchema,
            Command::CopyDiagnostics,
            Command::Help,
            Command::Quit,
        ]);
        out
    }

    pub fn describe(self) -> &'static str {
        match self {
            Command::Action(a) => a.describe(),
            Command::RecordView => "Show the selected row vertically",
            Command::ExportCsv => "Export rows as CSV",
            Command::ExportJson => "Export rows as JSON",
            Command::ExportInserts => "Export rows as SQL INSERT statements",
            Command::ExportAll(ExportFormat::Csv) => "Export all rows as CSV (not just those loaded)",
            Command::ExportAll(ExportFormat::Json) => "Export all rows as JSON (not just those loaded)",
            Command::ExportAll(ExportFormat::Sql) => "Export all rows as SQL INSERT statements",
            Command::CopyRows(ExportFormat::Csv) => "Copy rows as CSV (the marked ones, or all shown)",
            Command::CopyRows(ExportFormat::Json) => "Copy rows as JSON (the marked ones, or all shown)",
            Command::CopyRows(ExportFormat::Sql) => "Copy rows as SQL INSERT statements",
            Command::AgentActivity => "Agent activity: what AI agents ran (podelis mcp)",
            Command::LoadMore => "Load more rows",
            Command::PipeRows => "Pipe rows through a command (grep, sort, jq…)",
            Command::OpenRows => "Open rows in an app: CSV, JSON, SQL… (or a viewer in this terminal)",
            Command::OpenRowsAs(_) => "Open rows in an app",
            Command::RefreshSchema => "Refresh the navigator",
            Command::CopyDiagnostics => "Copy diagnostics for a bug report",
            Command::Help => "Show all keys",
            Command::Quit => "Quit",
            Command::SwitchTab(_) => "Switch to tab",
            Command::CompareSchema(_) => "Compare the schema with another connection",
            Command::SnapshotSchema => "Save a schema snapshot, to compare with later",
            Command::Watch(_) => "Watch: run the query again every few seconds",
            Command::CompareSnapshot(_) => "Compare the schema with a saved snapshot",
            Command::CompareRows(_) => "Compare the query's rows with another connection",
        }
    }

    /// What the palette lists: the description, with the tab's name.
    pub fn text(self, app: &App) -> String {
        match self {
            Command::SwitchTab(i) => {
                format!("Switch to tab: {}", app.tabs.get(i).map_or("", |t| t.name.as_str()))
            }
            Command::Watch(0) => "Stop watching the query".to_string(),
            Command::Watch(s) => format!("Watch: run the query again every {s} s, marking what changed"),
            Command::CompareSnapshot(i) => match app.palette.as_ref().and_then(|p| p.snapshots.get(i)) {
                Some(s) => format!("Compare the schema with the snapshot: {} · {}", s.connection, s.taken),
                None => self.describe().to_string(),
            },
            Command::CompareSchema(Some(id)) | Command::CompareRows(Some(id)) => {
                let name = app.other_connections().into_iter().find(|(i, _)| *i == id).map(|(_, n)| n);
                let what = if matches!(self, Command::CompareSchema(_)) { "schema" } else { "query's rows" };
                format!("Compare the {what} with {}", name.unwrap_or_default())
            }
            _ => self.describe().to_string(),
        }
    }

    /// The key that does it directly, if any.
    pub fn keys(self, app: &App) -> String {
        match self {
            Command::Action(a) => app.keymap.label(a).to_string(),
            Command::RecordView => "x (results)".into(),
            Command::ExportCsv => "e (results)".into(),
            Command::ExportJson => "J (results)".into(),
            Command::ExportInserts => "i (results)".into(),
            Command::LoadMore => "M (results)".into(),
            Command::PipeRows => "| (results)".into(),
            Command::OpenRows => "o (results)".into(),
            Command::RefreshSchema => "r (navigator)".into(),
            Command::Help => "F1".into(),
            Command::Quit => "Ctrl+Q".into(),
            Command::SwitchTab(i) if i < 9 => format!("Ctrl+K {}", i + 1),
            Command::CopyDiagnostics
            | Command::ExportAll(_)
            | Command::CopyRows(_)
            | Command::OpenRowsAs(_)
            | Command::AgentActivity
            | Command::SwitchTab(_)
            | Command::CompareSchema(_)
            | Command::SnapshotSchema
            | Command::Watch(_)
            | Command::CompareSnapshot(_)
            | Command::CompareRows(_) => String::new(),
        }
    }
}

/// How long Ctrl+K waits for a letter before showing its menu, so typing
/// `Ctrl+K w` quickly doesn't flash it.
pub const MENU_DELAY: Duration = Duration::from_millis(500);

pub struct Palette {
    pub query: LineInput,
    pub selected: usize,
    /// Searching commands by name (false: the menu of letters).
    pub searching: bool,
    /// The menu of letters is shown (it waits for `MENU_DELAY` first).
    pub menu: bool,
    opened: Instant,
    /// The saved schema snapshots, read once when it opens (it's searched
    /// on every key).
    snapshots: Vec<crate::snapshots::Saved>,
}

impl App {
    /// Waits for a letter; `menu_now` shows the menu at once (for `?`).
    pub(super) fn open_palette(&mut self, menu_now: bool) {
        self.palette = Some(Palette {
            query: LineInput::default(),
            selected: 0,
            searching: false,
            menu: menu_now,
            opened: Instant::now(),
            snapshots: self.snapshots(),
        });
        if !menu_now {
            let key = self.keymap.short_label(Action::Palette);
            self.set_status(format!("{key}…"), false);
        }
    }

    /// When the waiting Ctrl+K should show its menu.
    pub fn palette_menu_due(&self) -> Option<Instant> {
        self.palette.as_ref().filter(|p| !p.searching && !p.menu).map(|p| p.opened + MENU_DELAY)
    }

    /// No letter came: show the menu.
    pub fn show_palette_menu(&mut self) {
        if let Some(palette) = &mut self.palette {
            palette.menu = true;
        }
        self.status.clear();
    }

    /// Commands whose description contains every word typed.
    pub fn palette_items(&self) -> Vec<Command> {
        let query = self.palette.as_ref().map(|p| p.query.text().to_lowercase()).unwrap_or_default();
        // The other open tabs, by name, after the commands.
        let tabs = (0..self.tabs.len()).filter(|i| *i != self.active_tab).map(Command::SwitchTab);
        // Comparing: with each other connection, or (with none open) a hint.
        let others: Vec<Option<u64>> = match self.other_connections().as_slice() {
            [] => vec![None],
            many => many.iter().map(|(id, _)| Some(*id)).collect(),
        };
        let compare = others.iter().flat_map(|id| [Command::CompareSchema(*id), Command::CompareRows(*id)]);
        // The newest saved schema snapshots, one entry each.
        let saved = self.palette.as_ref().map_or(0, |p| p.snapshots.len());
        let snapshots = (0..saved.min(20)).map(Command::CompareSnapshot);
        // While watching, a way to stop that isn't Esc (vim's in the editor).
        let watch = if self.watch.is_some() { vec![0] } else { watch::EVERY.to_vec() };
        Command::all()
            .into_iter()
            .chain(compare)
            .chain(std::iter::once(Command::SnapshotSchema))
            .chain(watch.into_iter().map(Command::Watch))
            .chain(snapshots)
            .chain(tabs)
            .filter(|c| {
                let text = c.text(self).to_lowercase();
                query.split_whitespace().all(|w| text.contains(w))
            })
            .collect()
    }

    pub(super) fn handle_palette_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.palette.as_ref().is_some_and(|p| !p.searching) {
            return self.handle_leader_key(key, ctrl);
        }
        let count = self.palette_items().len();
        let Some(palette) = &mut self.palette else { return };
        if let Some(i) = list::step(palette.selected, count, key, Nav::SEARCHED) {
            palette.selected = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.palette = None,
            // Back to the letters.
            KeyCode::Backspace if palette.query.is_empty() => palette.searching = false,
            KeyCode::Enter => {
                let selected = palette.selected;
                let chosen = self.palette_items().get(selected).copied();
                self.palette = None;
                if let Some(command) = chosen {
                    self.run_command(command);
                }
            }
            _ => {
                if palette.query.handle(key) == InputResult::Changed {
                    palette.selected = 0;
                }
            }
        }
    }

    /// A key in the menu of letters: run its action, or start searching.
    fn handle_leader_key(&mut self, key: KeyEvent, ctrl: bool) {
        let again = self.keymap.action(key) == Some(Action::Palette);
        let Some(palette) = &mut self.palette else { return };
        match key.code {
            KeyCode::Esc => self.palette = None,
            // Ctrl+K again, or arrows/Enter: the full list.
            _ if again => palette.searching = true,
            KeyCode::Up | KeyCode::Down | KeyCode::Enter => palette.searching = true,
            KeyCode::Char(c) if !ctrl => {
                if let Some(n) = c.to_digit(10).filter(|n| *n > 0) {
                    self.palette = None;
                    return self.switch_tab((n as usize - 1).min(self.tabs.len() - 1));
                }
                if c == '?' {
                    self.palette = None;
                    self.help_visible = true;
                    return;
                }
                if let Some(action) = self.keymap.leader(c) {
                    self.palette = None;
                    return self.run_action(action);
                }
                palette.searching = true;
                if c != '/' && c != ' ' {
                    palette.query.insert_str(&c.to_string());
                }
            }
            _ => {}
        }
    }

    /// What the palette says for `command`.
    #[cfg(test)]
    pub(super) fn palette_command_text(&self, command: Command) -> String {
        command.text(self)
    }

    pub(super) fn run_command(&mut self, command: Command) {
        match command {
            Command::Action(a) => self.run_action(a),
            Command::RecordView => self.open_record_view(),
            Command::ExportCsv => self.export(ExportFormat::Csv),
            Command::ExportJson => self.export(ExportFormat::Json),
            Command::ExportInserts => self.export(ExportFormat::Sql),
            Command::ExportAll(format) => self.export_all(format),
            Command::CopyRows(format) => self.copy_rows_as(format),
            Command::AgentActivity => self.open_agents(),
            Command::LoadMore => self.load_more(),
            Command::PipeRows => self.open_pipe_prompt(),
            Command::OpenRows => self.open_in_viewer(),
            Command::OpenRowsAs(format) => self.open_rows_as(format),
            Command::RefreshSchema => self.load_catalog(),
            Command::CopyDiagnostics => {
                let text = self.diagnostics();
                self.copy_text(text, "Copied diagnostics (no hosts, users or SQL)");
            }
            Command::Help => self.help_visible = true,
            Command::Quit => self.request_quit(),
            Command::SwitchTab(i) => self.switch_tab(i),
            Command::CompareSchema(id) => self.compare_schema(id),
            Command::SnapshotSchema => self.snapshot_schema(),
            Command::Watch(0) => self.stop_watch(""),
            Command::Watch(s) => self.start_watch(s),
            Command::CompareSnapshot(i) => self.compare_snapshot(i),
            Command::CompareRows(id) => self.compare_rows(id),
        }
    }
}
