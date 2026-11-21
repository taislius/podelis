//! Ctrl+K: first a menu of one-letter actions and the focused pane's keys;
//! `/` or Enter searches every action by name (the palette), as does typing
//! what isn't a letter in use.

use crate::app::commands::Command;
use crate::app::*;
use crate::config::keys::{Action, LeaderStep};

/// How long Ctrl+K waits for a letter before showing its menu, so typing
/// `Ctrl+K w` quickly doesn't flash it.
pub const MENU_DELAY: Duration = Duration::from_millis(500);

/// What Ctrl+K is doing.
pub enum PaletteMode {
    /// Waiting for a letter; whether its menu is shown yet (it waits for
    /// `MENU_DELAY` first), and the group whose letter was pressed, if any.
    Leader { menu: bool, group: Option<char> },
    /// Searching every command by name.
    Commands,
    /// Only the tabs, all of them (Ctrl+K g), found by name, connection or
    /// the SQL in them: each tab's text to search, lowercased, read once
    /// when the list opens (it's searched on every key and drawn every
    /// frame).
    Tabs(Vec<String>),
    /// Only the open connections (Ctrl+K j), with their text to search
    /// (name, address, its tabs' names), lowercased.
    Connections(Vec<(ConnId, String)>),
    /// The server's databases (`D` in the navigator), by name.
    Databases(Vec<String>),
    /// The projects, to switch to (the top bar's project ▼).
    Projects,
    /// The project's saved connections, for this tab (its connection ▼).
    Saved,
}

pub struct Palette {
    pub query: LineInput,
    pub selected: usize,
    pub mode: PaletteMode,
    opened: Instant,
    /// Where it hangs from, as a drop-down (under what was clicked); `None`
    /// in the middle of the screen.
    pub anchor: Option<(u16, u16)>,
    /// The saved schema snapshots, read once when it opens (it's searched
    /// on every key).
    snapshots: Vec<crate::transfer::snapshots::Saved>,
}

impl Palette {
    fn new(mode: PaletteMode, selected: usize, snapshots: Vec<crate::transfer::snapshots::Saved>) -> Self {
        Palette { query: LineInput::default(), selected, mode, opened: Instant::now(), anchor: None, snapshots }
    }

    /// A list to search (false: the menu of letters).
    pub fn searching(&self) -> bool {
        !matches!(self.mode, PaletteMode::Leader { .. })
    }

    /// The menu of letters is shown.
    pub fn menu_shown(&self) -> bool {
        matches!(self.mode, PaletteMode::Leader { menu: true, .. })
    }

    /// The group whose letter was pressed.
    pub fn group(&self) -> Option<char> {
        match self.mode {
            PaletteMode::Leader { group, .. } => group,
            _ => None,
        }
    }

    pub fn tabs_only(&self) -> bool {
        matches!(self.mode, PaletteMode::Tabs(_))
    }

    /// A list of its own to pick from, `selected` picked: no letters to go
    /// back to.
    pub(in crate::app) fn listing(mode: PaletteMode, selected: usize) -> Self {
        Palette::new(mode, selected, Vec::new())
    }

    /// A list of its own (no letters to go back to).
    pub fn own_list(&self) -> bool {
        matches!(
            self.mode,
            PaletteMode::Tabs(_)
                | PaletteMode::Connections(_)
                | PaletteMode::Databases(_)
                | PaletteMode::Projects
                | PaletteMode::Saved
        )
    }

    /// Database `i` of the list.
    pub(in crate::app) fn database(&self, i: usize) -> Option<&String> {
        if let PaletteMode::Databases(names) = &self.mode { names.get(i) } else { None }
    }

    pub fn conns_only(&self) -> bool {
        matches!(self.mode, PaletteMode::Connections(_))
    }

    pub(in crate::app) fn snapshot(&self, i: usize) -> Option<&crate::transfer::snapshots::Saved> {
        self.snapshots.get(i)
    }
}

impl App {
    /// Waits for a letter; `menu_now` shows the menu at once (for `?`).
    pub(in crate::app) fn open_palette(&mut self, menu_now: bool) {
        self.popups.palette =
            Some(Palette::new(PaletteMode::Leader { menu: menu_now, group: None }, 0, self.snapshots()));
        if !menu_now {
            let key = self.keymap.short_label(Action::Palette);
            // With no menu coming by itself, how to get it.
            let status = if self.settings.leader_menu { format!("{key}…") } else { t!("palette.waiting", key = key) };
            self.set_status(status, false);
        }
    }

    /// The list of every command, searching for `query`.
    pub(in crate::app) fn open_command_list(&mut self, query: &str) {
        let mut palette = Palette::new(PaletteMode::Commands, 0, self.snapshots());
        palette.query.insert_str(query);
        self.popups.palette = Some(palette);
    }

    /// A tab as the tab list shows it: its name, connection and how that is
    /// (a query running and for
    /// how long, a transaction open, lost), and which is shown.
    pub fn tab_entry(&self, i: usize) -> String {
        let Some(tab) = self.tabs.get(i) else { return String::new() };
        let on = tab.conn.and_then(|id| self.conn_name(id)).map(|n| format!("  · {n}")).unwrap_or_default();
        let mut statuses = Vec::new();
        if tab.saved_id.is_some() && self.tab_dirty(i) {
            statuses.push(t!("tab_list.edited").to_string());
        }
        statuses.extend(self.tab_status(i));
        let status = if statuses.is_empty() { String::new() } else { format!("  · {}", statuses.join(" · ")) };
        let here = if i == self.active_tab { format!("  ({})", t!("menu.this_one")) } else { String::new() };
        format!("{}{on}{status}{here}", tab.name)
    }

    /// How tab `i`'s connection is, when it's worth saying: its query
    /// running (or being cancelled), a transaction left open, a lost
    /// connection, or none.
    fn tab_status(&self, i: usize) -> Option<String> {
        let tab = self.tabs.get(i)?;
        // Its own query (each tab has one; several may run at once).
        if let Some(run) = self.tab_run(i).filter(|r| r.executing) {
            if run.cancelling {
                return Some(t!("tab_list.cancelling").to_string());
            }
            let secs = run.since.map_or(0, |s| s.elapsed().as_secs());
            return Some(t!("tab_list.running", time = mm_ss(secs)));
        }
        let Some(id) = tab.conn else { return Some(t!("tab_list.no_connection").to_string()) };
        self.conn_state_said(id)
            .or_else(|| (self.conn_txn(id) == Some(TxnState::Open)).then(|| t!("tab_list.in_transaction").to_string()))
    }

    /// Ctrl+K g: every tab in a list to search and pick from, the shown
    /// one selected (the tab row only has room for so many).
    pub(super) fn open_tab_list(&mut self) {
        let texts = (0..self.tabs.len())
            // Its name, connection and SQL: not its status, which changes.
            .map(|i| {
                let tab = &self.tabs[i];
                let on = tab.conn.and_then(|id| self.conn_name(id)).unwrap_or_default();
                format!("{}\n{on}\n{}", tab.name, self.tab_text(i)).to_lowercase()
            })
            .collect();
        self.popups.palette = Some(Palette::new(PaletteMode::Tabs(texts), self.active_tab, Vec::new()));
    }

    /// The top bar's project ▼: the projects to switch to, the open one
    /// picked, and the whole projects window after them.
    pub fn open_project_menu(&mut self, anchor: (u16, u16)) {
        let here = self.store.projects.iter().position(|p| Some(&p.id) == self.current_project.as_ref());
        let mut palette = Palette::listing(PaletteMode::Projects, here.unwrap_or(0));
        palette.anchor = Some(anchor);
        self.popups.palette = Some(palette);
    }

    /// The top bar's connection ▼: the project's connections, for this tab
    /// (its own picked), then its databases and the connections window.
    pub fn open_connection_menu(&mut self, anchor: (u16, u16)) {
        let here = self
            .connection
            .slots
            .active
            .and_then(|_| self.profiles().iter().position(|p| p.name == self.connection.form.name));
        let mut palette = Palette::listing(PaletteMode::Saved, here.unwrap_or(0));
        palette.anchor = Some(anchor);
        self.popups.palette = Some(palette);
    }

    /// A project as its drop-down shows it.
    pub fn project_entry(&self, i: usize) -> String {
        let Some(project) = self.store.projects.get(i) else { return String::new() };
        if Some(&project.id) == self.current_project.as_ref() {
            format!("{}  · {}", project.name, t!("palette.this_project"))
        } else {
            project.name.clone()
        }
    }

    /// A saved connection as its drop-down shows it: whether it's this
    /// tab's, or open in other tabs.
    pub fn saved_entry(&self, i: usize) -> String {
        let Some(profile) = self.profiles().get(i) else { return String::new() };
        let slots: Vec<ConnId> = self
            .connection
            .all()
            .filter(|(_, s)| s.form.name == profile.name && s.database.is_some())
            .map(|(id, _)| id)
            .collect();
        let here = self.tabs[self.active_tab].conn.is_some_and(|c| slots.contains(&c));
        let tabs = self.tabs.iter().filter(|t| t.conn.is_some_and(|c| slots.contains(&c))).count();
        let said = if here {
            format!("  · {}", t!("palette.in_this_tab"))
        } else if tabs > 0 {
            format!("  · {}", t!("palette.in_tabs", n = tabs))
        } else {
            String::new()
        };
        format!("{}  · {}{said}", profile.name, profile.engine.label())
    }

    /// Ctrl+K j: the open connections, to go to or close; the shown tab's
    /// selected.
    pub(super) fn open_connection_list(&mut self) {
        let mut ids: Vec<ConnId> = self.connection.slots.active.into_iter().collect();
        ids.extend(self.parked_connections().into_iter().map(|(id, ..)| id));
        let conn_texts: Vec<(ConnId, String)> = ids
            .iter()
            .map(|&id| {
                let tabs: Vec<&str> =
                    self.tabs.iter().filter(|t| t.conn == Some(id)).map(|t| t.name.as_str()).collect();
                (
                    id,
                    format!(
                        "{}\n{}\n{}",
                        self.conn_name(id).unwrap_or_default(),
                        self.conn_address(id),
                        tabs.join("\n")
                    )
                    .to_lowercase(),
                )
            })
            .collect();
        if conn_texts.is_empty() {
            return self.set_status(t!("tab_list.no_connection").to_string(), false);
        }
        self.popups.palette = Some(Palette::new(PaletteMode::Connections(conn_texts), 0, Vec::new()));
    }

    /// Where connection `id` stands, in words, when it isn't simply
    /// working (shown or parked: a hidden tab's too).
    fn conn_state_said(&self, id: ConnId) -> Option<String> {
        Some(match self.connection.state_of(id)? {
            ConnState::Lost { .. } => t!("tab_list.lost").to_string(),
            ConnState::Opening { .. } => t!("bar.reconnecting").to_string(),
            ConnState::Failed { .. } => t!("tab_list.not_reconnected").to_string(),
            ConnState::Degraded { .. } => t!("bar.no_tables").to_string(),
            ConnState::Ready { .. } => return None,
        })
    }

    /// An open connection as its list shows it: its name, engine and
    /// address, its tabs, and how it is (a query running, a transaction
    /// open, lost).
    pub fn connection_entry(&self, id: ConnId) -> String {
        let name = self.conn_name(id).unwrap_or_default();
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].conn == Some(id)).collect();
        let names: Vec<&str> = tabs.iter().map(|&i| self.tabs[i].name.as_str()).collect();
        let names = if names.is_empty() { t!("conn_list.no_tabs").to_string() } else { names.join(", ") };
        let running = tabs.iter().find_map(|&i| self.tab_run(i).filter(|r| r.executing).map(|r| (i, r)));
        let status = if let Some((_, run)) = running {
            Some(t!("tab_list.running", time = mm_ss(run.since.map_or(0, |s| s.elapsed().as_secs()))))
        } else if let Some(said) = self.conn_state_said(id) {
            Some(said)
        } else {
            (self.conn_txn(id) == Some(TxnState::Open)).then(|| t!("tab_list.in_transaction").to_string())
        };
        let status = status.map(|s| format!("  · {s}")).unwrap_or_default();
        let here = if self.connection.slots.active == Some(id) {
            format!("  ({})", t!("menu.this_one"))
        } else {
            String::new()
        };
        format!("{name}  · {}  · {names}{status}{here}", self.conn_address(id))
    }

    /// When the waiting Ctrl+K should show its menu.
    pub fn palette_menu_due(&self) -> Option<Instant> {
        let palette = self.popups.palette.as_ref().filter(|_| self.settings.leader_menu);
        palette.filter(|p| matches!(p.mode, PaletteMode::Leader { menu: false, .. })).map(|p| p.opened + MENU_DELAY)
    }

    /// No letter came: show the menu.
    pub fn show_palette_menu(&mut self) {
        if let Some(palette) = &mut self.popups.palette {
            palette.mode = PaletteMode::Leader { menu: true, group: palette.group() };
        }
        self.notes.status.clear();
    }

    /// Commands whose description contains every word typed.
    pub fn palette_items(&self) -> Vec<Command> {
        let query = self.popups.palette.as_ref().map(|p| p.query.text().to_lowercase()).unwrap_or_default();
        if let Some(PaletteMode::Connections(texts)) = self.popups.palette.as_ref().map(|p| &p.mode) {
            return texts
                .iter()
                .filter(|(id, text)| {
                    self.conn_name(*id).is_some() && query.split_whitespace().all(|w| text.contains(w))
                })
                .map(|(id, _)| Command::GoToConnection(*id))
                .collect();
        }
        let matches = |text: &str| query.split_whitespace().all(|w| text.to_lowercase().contains(w));
        if let Some(PaletteMode::Projects) = self.popups.palette.as_ref().map(|p| &p.mode) {
            return (self.store.projects.iter().enumerate())
                .filter(|(_, p)| matches(&p.name))
                .map(|(i, _)| Command::SwitchProject(i))
                .chain(std::iter::once(Command::Action(Action::Projects)).filter(|_| query.is_empty()))
                .collect();
        }
        if let Some(PaletteMode::Saved) = self.popups.palette.as_ref().map(|p| &p.mode) {
            let more = [Command::PickDatabase, Command::Action(Action::OpenConnection)];
            return (self.profiles().iter().enumerate())
                .filter(|(_, p)| matches(&p.name))
                .map(|(i, _)| Command::OpenSaved(i))
                .chain(more.into_iter().filter(|_| query.is_empty()))
                .collect();
        }
        if let Some(PaletteMode::Databases(names)) = self.popups.palette.as_ref().map(|p| &p.mode) {
            return (names.iter().enumerate())
                .filter(|(_, name)| query.split_whitespace().all(|w| name.to_lowercase().contains(w)))
                .map(|(i, _)| Command::OpenDatabase(i))
                .collect();
        }
        if let Some(PaletteMode::Tabs(texts)) = self.popups.palette.as_ref().map(|p| &p.mode) {
            // A tab by its name, its connection, or the SQL in it.
            return (texts.iter().enumerate())
                .filter(|(_, text)| query.split_whitespace().all(|w| text.contains(w)))
                .map(|(i, _)| Command::SwitchTab(i))
                .collect();
        }
        // The other open tabs, by name, after the commands.
        let tabs = (0..self.tabs.len()).filter(|i| *i != self.active_tab).map(Command::SwitchTab);
        // Comparing: with each other connection, or (with none open) a hint.
        let others: Vec<Option<ConnId>> = match self.other_connections().as_slice() {
            [] => vec![None],
            many => many.iter().map(|(id, _)| Some(*id)).collect(),
        };
        let compare = others.iter().flat_map(|id| [Command::CompareSchema(*id), Command::CompareRows(*id)]);
        // The newest saved schema snapshots, one entry each.
        let saved = self.popups.palette.as_ref().map_or(0, |p| p.snapshots.len());
        let snapshots = (0..saved.min(20)).map(Command::CompareSnapshot);
        // While watching, a way to stop that isn't Esc (vim's in the editor).
        let watch = if self.watch.is_some() { vec![0] } else { query_screen::watch::EVERY.to_vec() };
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
        if self.popups.palette.as_ref().is_some_and(|p| !p.searching()) {
            return self.handle_leader_key(key, ctrl);
        }
        let count = self.palette_items().len();
        let Some(palette) = &mut self.popups.palette else { return };
        if let Some(i) = list::step(palette.selected, count, key, Nav::SEARCHED) {
            palette.selected = i;
            return;
        }
        // Ctrl+X in the connections: close them all.
        if palette.conns_only() && ctrl && key.code == KeyCode::Char('x') {
            return self.request_close_all_connections();
        }
        // Ctrl+D in the connections: close the one selected.
        if palette.conns_only() && ctrl && key.code == KeyCode::Char('d') {
            let selected = palette.selected;
            if let Some(Command::GoToConnection(id)) = self.palette_items().get(selected).copied() {
                self.request_close_connection(id);
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.popups.palette = None,
            // Back to the letters.
            // (Not from the tab list: it has no letters to go back to.)
            KeyCode::Backspace if palette.query.is_empty() && !palette.own_list() => {
                palette.mode = PaletteMode::Leader { menu: true, group: None };
            }
            KeyCode::Enter => {
                let selected = palette.selected;
                let chosen = self.palette_items().get(selected).copied();
                // The shown tab's own: nothing to go to. The list stays, and
                // says what can be done with it.
                if let Some(Command::GoToConnection(id)) = chosen
                    && self.connection.slots.active == Some(id)
                {
                    let name = self.conn_name(id).unwrap_or_default();
                    return self.set_status(t!("conn_list.here", name = name), false);
                }
                // A database: its list goes once it's opened (it needs the list).
                if !matches!(chosen, Some(Command::OpenDatabase(_))) {
                    self.popups.palette = None;
                }
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
        let Some(palette) = &mut self.popups.palette else { return };
        let group = palette.group();
        match key.code {
            KeyCode::Esc => self.popups.palette = None,
            // Ctrl+Shift+K (where the terminal tells it apart): the menu.
            _ if again && key.modifiers.contains(KeyModifiers::SHIFT) => {
                palette.mode = PaletteMode::Leader { menu: true, group };
            }
            // Ctrl+K again, or arrows/Enter: the full list.
            _ if again => palette.mode = PaletteMode::Commands,
            KeyCode::Up | KeyCode::Down | KeyCode::Enter => palette.mode = PaletteMode::Commands,
            // Out of a group: the letters again.
            KeyCode::Backspace if group.is_some() => palette.mode = PaletteMode::Leader { menu: true, group: None },
            KeyCode::Char(c) if !ctrl => match group {
                Some(group) => self.handle_group_letter(group, c),
                None => self.handle_top_letter(c),
            },
            _ => {}
        }
    }

    /// A letter straight after Ctrl+K.
    fn handle_top_letter(&mut self, c: char) {
        let Some(palette) = &mut self.popups.palette else { return };
        // 1…9 a tab by its place, 0 the last one (past nine, too).
        if let Some(n) = c.to_digit(10) {
            self.popups.palette = None;
            let last = self.tabs.len() - 1;
            return self.switch_tab(if n == 0 { last } else { (n as usize - 1).min(last) });
        }
        // `?`: with no menu coming by itself, the menu; then (or with it
        // coming) every key.
        if c == '?' && !palette.menu_shown() && !self.settings.leader_menu {
            palette.mode = PaletteMode::Leader { menu: true, group: None };
            self.notes.status.clear();
            return;
        }
        if c == '?' {
            self.popups.palette = None;
            self.help.visible = true;
            return;
        }
        match self.keymap.leader_step(&[c]) {
            LeaderStep::Run(action) => {
                self.popups.palette = None;
                self.run_action(action);
            }
            LeaderStep::Group => {
                // Its menu after the same pause, or at once with the letters' up.
                palette.mode = PaletteMode::Leader { menu: palette.menu_shown(), group: Some(c) };
                palette.opened = Instant::now();
                if !palette.menu_shown() {
                    let key = self.keymap.short_label(Action::Palette);
                    self.set_status(format!("{key} {c}…"), false);
                }
            }
            LeaderStep::Nothing => {
                palette.mode = PaletteMode::Commands;
                if c != '/' && c != ' ' {
                    palette.query.insert_str(&c.to_string());
                }
            }
        }
    }

    /// A letter after a group's: runs what's in it. `m` and `?` mean what
    /// they mean everywhere; another letter shows what's in the group, never
    /// a command somewhere else.
    fn handle_group_letter(&mut self, group: char, c: char) {
        let Some(palette) = &mut self.popups.palette else { return };
        if let LeaderStep::Run(action) = self.keymap.leader_step(&[group, c]) {
            self.popups.palette = None;
            return self.run_action(action);
        }
        match c {
            'm' => {
                self.popups.palette = None;
                self.open_menu(None);
            }
            '?' if palette.menu_shown() => {
                self.popups.palette = None;
                self.help.visible = true;
            }
            _ => {
                palette.mode = PaletteMode::Leader { menu: true, group: Some(group) };
                self.notes.status.clear();
            }
        }
    }

    /// What the palette says for `command`.
    #[cfg(test)]
    pub(in crate::app) fn palette_command_text(&self, command: Command) -> String {
        command.text(self)
    }
}
