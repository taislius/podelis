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
    Connections(Vec<(u64, String)>),
}

pub struct Palette {
    pub query: LineInput,
    pub selected: usize,
    pub mode: PaletteMode,
    opened: Instant,
    /// The saved schema snapshots, read once when it opens (it's searched
    /// on every key).
    snapshots: Vec<crate::transfer::snapshots::Saved>,
}

impl Palette {
    fn new(mode: PaletteMode, selected: usize, snapshots: Vec<crate::transfer::snapshots::Saved>) -> Self {
        Palette { query: LineInput::default(), selected, mode, opened: Instant::now(), snapshots }
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

    /// A tab as the tab list shows it: its name, ! if its saved query has
    /// changed, its connection and how that is (a query running and for
    /// how long, a transaction open, lost), and which is shown.
    pub fn tab_entry(&self, i: usize) -> String {
        let Some(tab) = self.tabs.get(i) else { return String::new() };
        let dirty = if tab.saved_id.is_some() && self.tab_dirty(i) { " !" } else { "" };
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
        // Its own query (each tab has one; several may run at once).
        if let Some(run) = self.tab_run(i).filter(|r| r.executing) {
            if run.cancelling {
                return Some(t!("tab_list.cancelling").to_string());
            }
            let secs = run.since.map_or(0, |s| s.elapsed().as_secs());
            return Some(t!("tab_list.running", time = mm_ss(secs)));
        }
        let Some(id) = tab.conn else { return Some(t!("tab_list.no_connection").to_string()) };
        if self.conns.active == Some(id) && self.keepalive.connection_lost {
            return Some(t!("tab_list.lost").to_string());
        }
        (self.conn_txn(id) == Some(TxnState::Open)).then(|| t!("tab_list.in_transaction").to_string())
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

    /// Ctrl+K j: the open connections, to go to or close; the shown tab's
    /// selected.
    pub(super) fn open_connection_list(&mut self) {
        let mut ids: Vec<u64> = self.conns.active.into_iter().collect();
        ids.extend(self.parked_connections().into_iter().map(|(id, ..)| id));
        let conn_texts: Vec<(u64, String)> = ids
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

    /// An open connection as its list shows it: its name, engine and
    /// address, its tabs, and how it is (a query running, a transaction
    /// open, lost).
    pub fn connection_entry(&self, id: u64) -> String {
        let name = self.conn_name(id).unwrap_or_default();
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].conn == Some(id)).collect();
        let names: Vec<&str> = tabs.iter().map(|&i| self.tabs[i].name.as_str()).collect();
        let names = if names.is_empty() { t!("conn_list.no_tabs").to_string() } else { names.join(", ") };
        let running = tabs.iter().find_map(|&i| self.tab_run(i).filter(|r| r.executing).map(|r| (i, r)));
        let status = if let Some((_, run)) = running {
            Some(t!("tab_list.running", time = mm_ss(run.since.map_or(0, |s| s.elapsed().as_secs()))))
        } else if self.conns.active == Some(id) && self.keepalive.connection_lost {
            Some(t!("tab_list.lost").to_string())
        } else {
            (self.conn_txn(id) == Some(TxnState::Open)).then(|| t!("tab_list.in_transaction").to_string())
        };
        let status = status.map(|s| format!("  · {s}")).unwrap_or_default();
        let here = if self.conns.active == Some(id) { format!("  ({})", t!("menu.this_one")) } else { String::new() };
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
        let others: Vec<Option<u64>> = match self.other_connections().as_slice() {
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
            KeyCode::Backspace if palette.query.is_empty() && !palette.tabs_only() && !palette.conns_only() => {
                palette.mode = PaletteMode::Leader { menu: true, group: None };
            }
            KeyCode::Enter => {
                let selected = palette.selected;
                let chosen = self.palette_items().get(selected).copied();
                // The shown tab's own: nothing to go to. The list stays, and
                // says what can be done with it.
                if let Some(Command::GoToConnection(id)) = chosen
                    && self.conns.active == Some(id)
                {
                    let name = self.conn_name(id).unwrap_or_default();
                    return self.set_status(t!("conn_list.here", name = name), false);
                }
                self.popups.palette = None;
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
