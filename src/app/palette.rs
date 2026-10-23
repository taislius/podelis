//! Ctrl+K: first a menu of one-letter actions and the focused pane's keys;
//! typing anything else searches every action by name (the palette).

use super::*;
use crate::config::keys::Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Action(Action),
    RecordView,
    ExportCsv,
    ExportJson,
    ExportInserts,
    ExportAscii,
    /// Every row, not just those loaded.
    ExportAll(PagedFormat),
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
            Command::ExportAscii,
            Command::ExportAll(PagedFormat::Csv),
            Command::ExportAll(PagedFormat::Json),
            Command::ExportAll(PagedFormat::Sql),
            Command::CopyRows(ExportFormat::Csv),
            Command::CopyRows(ExportFormat::Json),
            Command::CopyRows(ExportFormat::Sql),
            Command::CopyRows(ExportFormat::Ascii),
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
            Command::RecordView => t!("palette.record"),
            Command::ExportCsv => t!("palette.export_csv"),
            Command::ExportJson => t!("palette.export_json"),
            Command::ExportInserts => t!("palette.export_sql"),
            Command::ExportAll(PagedFormat::Csv) => t!("menu.export_all_csv_full"),
            Command::ExportAll(PagedFormat::Json) => t!("palette.export_all_json"),
            Command::ExportAll(PagedFormat::Sql) => t!("palette.export_all_sql"),
            Command::CopyRows(ExportFormat::Csv) => t!("palette.copy_csv"),
            Command::CopyRows(ExportFormat::Json) => t!("palette.copy_json"),
            Command::CopyRows(ExportFormat::Sql) => t!("palette.copy_sql"),
            Command::CopyRows(ExportFormat::Ascii) => t!("menu.copy_ascii"),
            Command::ExportAscii => t!("menu.export_ascii"),
            Command::AgentActivity => t!("palette.agents"),
            Command::LoadMore => t!("menu.load_more"),
            Command::PipeRows => t!("palette.pipe"),
            Command::OpenRows => t!("palette.open_rows"),
            Command::OpenRowsAs(_) => t!("palette.open_rows_as"),
            Command::RefreshSchema => t!("palette.refresh"),
            Command::CopyDiagnostics => t!("palette.diagnostics"),
            Command::Help => t!("palette.help"),
            Command::Quit => t!("palette.quit"),
            Command::SwitchTab(_) => t!("palette.switch_tab"),
            Command::CompareSchema(_) => t!("palette.compare_schema"),
            Command::SnapshotSchema => t!("palette.snapshot"),
            Command::Watch(_) => t!("palette.watch"),
            Command::CompareSnapshot(_) => t!("palette.compare_snapshot"),
            Command::CompareRows(_) => t!("palette.compare_rows"),
        }
    }

    /// What the palette lists: the description, with the tab's name.
    pub fn text(self, app: &App) -> String {
        match self {
            Command::SwitchTab(i) if app.palette.as_ref().is_some_and(|p| p.tabs_only) => app.tab_entry(i),
            Command::SwitchTab(i) => {
                format!("{}: {}", t!("palette.switch_tab"), app.tabs.get(i).map_or("", |t| t.name.as_str()))
            }
            Command::Watch(0) => t!("palette.stop_watch").to_string(),
            Command::Watch(s) => t!("palette.watch_every", s = s),
            Command::CompareSnapshot(i) => match app.palette.as_ref().and_then(|p| p.snapshots.get(i)) {
                Some(s) => t!("palette.compare_with_snapshot", connection = s.connection, taken = s.taken),
                None => self.describe().to_string(),
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
            _ => self.describe().to_string(),
        }
    }

    /// The key that does it directly, if any.
    pub fn keys(self, app: &App) -> String {
        match self {
            Command::Action(a) => app.keymap.label(a).to_string(),
            Command::RecordView => format!("x ({})", t!("palette.in_results")),
            Command::ExportCsv => format!("e ({})", t!("palette.in_results")),
            Command::ExportJson => format!("J ({})", t!("palette.in_results")),
            Command::ExportInserts => format!("i ({})", t!("palette.in_results")),
            Command::LoadMore => format!("M ({})", t!("palette.in_results")),
            Command::PipeRows => format!("| ({})", t!("palette.in_results")),
            Command::OpenRows => format!("o ({})", t!("palette.in_results")),
            Command::RefreshSchema => format!("r ({})", t!("palette.in_navigator")),
            Command::Help => "F1".into(),
            Command::Quit => "Ctrl+Q".into(),
            Command::SwitchTab(i) if i < 9 => format!("Ctrl+K {}", i + 1),
            Command::CopyDiagnostics
            | Command::ExportAll(_)
            | Command::ExportAscii
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
    snapshots: Vec<crate::transfer::snapshots::Saved>,
    /// Only the tabs, all of them (Ctrl+K g): found by name, connection or
    /// the SQL in them.
    pub tabs_only: bool,
    /// Each tab's text to search, lowercased, read once when the list
    /// opens (it's searched on every key and drawn every frame).
    tab_texts: Vec<String>,
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
            tabs_only: false,
            tab_texts: Vec::new(),
        });
        if !menu_now {
            let key = self.keymap.short_label(Action::Palette);
            // With no menu coming by itself, how to get it.
            let status = if self.settings.leader_menu { format!("{key}…") } else { t!("palette.waiting", key = key) };
            self.set_status(status, false);
        }
    }

    /// A tab as the tab list shows it: its name, ● if its saved query has
    /// changed, its connection and how that is (a query running and for
    /// how long, a transaction open, lost), and which is shown.
    pub fn tab_entry(&self, i: usize) -> String {
        let Some(tab) = self.tabs.get(i) else { return String::new() };
        let dirty = if tab.saved_id.is_some() && self.tab_dirty(i) { " ●" } else { "" };
        let on = tab.conn.and_then(|id| self.conn_name(id)).map(|n| format!("  · {n}")).unwrap_or_default();
        let status = self.tab_status(i).map(|s| format!("  · {s}")).unwrap_or_default();
        let here = if i == self.active_tab { format!("  ({})", t!("menu.this_one")) } else { String::new() };
        format!("{}{dirty}{on}{status}{here}", tab.name)
    }

    /// How tab `i`'s connection is, when it's worth saying: its query
    /// running (or being cancelled), a transaction left open, a lost
    /// connection, or none.
    fn tab_status(&self, i: usize) -> Option<String> {
        let tab = self.tabs.get(i)?;
        // A write or a check holds every tab: it's the shown tab's.
        let runs_here = self.run_tab() == Some(i);
        if runs_here {
            if self.run.cancelling {
                return Some(t!("tab_list.cancelling").to_string());
            }
            let secs = self.run.since.map_or(0, |s| s.elapsed().as_secs());
            return Some(t!("tab_list.running", time = mm_ss(secs)));
        }
        let Some(id) = tab.conn else { return Some(t!("tab_list.no_connection").to_string()) };
        if self.conns.active == Some(id) && self.connection_lost {
            return Some(t!("tab_list.lost").to_string());
        }
        (self.conn_txn(id) == Some(TxnState::Open)).then(|| t!("tab_list.in_transaction").to_string())
    }

    /// Ctrl+K g: every tab in a list to search and pick from, the shown
    /// one selected (the tab row only has room for so many).
    pub(super) fn open_tab_list(&mut self) {
        self.palette = Some(Palette {
            query: LineInput::default(),
            selected: self.active_tab,
            searching: true,
            menu: false,
            opened: Instant::now(),
            snapshots: Vec::new(),
            tabs_only: true,
            tab_texts: (0..self.tabs.len())
                // Its name, connection and SQL: not its status, which changes.
                .map(|i| {
                    let tab = &self.tabs[i];
                    let on = tab.conn.and_then(|id| self.conn_name(id)).unwrap_or_default();
                    format!("{}\n{on}\n{}", tab.name, self.tab_text(i)).to_lowercase()
                })
                .collect(),
        });
    }

    /// When the waiting Ctrl+K should show its menu.
    pub fn palette_menu_due(&self) -> Option<Instant> {
        let palette = self.palette.as_ref().filter(|_| self.settings.leader_menu);
        palette.filter(|p| !p.searching && !p.menu).map(|p| p.opened + MENU_DELAY)
    }

    /// No letter came: show the menu.
    pub fn show_palette_menu(&mut self) {
        if let Some(palette) = &mut self.palette {
            palette.menu = true;
        }
        self.notes.status.clear();
    }

    /// Commands whose description contains every word typed.
    pub fn palette_items(&self) -> Vec<Command> {
        let query = self.palette.as_ref().map(|p| p.query.text().to_lowercase()).unwrap_or_default();
        if let Some(palette) = self.palette.as_ref().filter(|p| p.tabs_only) {
            // A tab by its name, its connection, or the SQL in it.
            return (palette.tab_texts.iter().enumerate())
                .filter(|(_, text)| query.split_whitespace().all(|w| text.contains(w)))
                .map(|(i, _)| Command::SwitchTab(i))
                .collect();
        }
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
            // (Not from the tab list: it has no letters to go back to.)
            KeyCode::Backspace if palette.query.is_empty() && !palette.tabs_only => palette.searching = false,
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
            // Ctrl+Shift+K (where the terminal tells it apart): the menu.
            _ if again && key.modifiers.contains(KeyModifiers::SHIFT) => palette.menu = true,
            // Ctrl+K again, or arrows/Enter: the full list.
            _ if again => palette.searching = true,
            KeyCode::Up | KeyCode::Down | KeyCode::Enter => palette.searching = true,
            KeyCode::Char(c) if !ctrl => {
                if let Some(n) = c.to_digit(10).filter(|n| *n > 0) {
                    self.palette = None;
                    return self.switch_tab((n as usize - 1).min(self.tabs.len() - 1));
                }
                // `?`: with no menu coming by itself, the menu; then (or
                // with it coming) every key.
                if c == '?' && !palette.menu && !self.settings.leader_menu {
                    palette.menu = true;
                    self.notes.status.clear();
                    return;
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
            Command::ExportAscii => self.export(ExportFormat::Ascii),
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
                self.copy_text(text, t!("palette.copied_diagnostics"));
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
