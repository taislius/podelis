//! The library (F3): saved queries and the history, searched as you type;
//! `Enter` opens one in a tab, `Tab` inserts it at the cursor.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::app::mouse::{DOUBLE_CLICK, inside};
use crate::app::*;
use crate::config::library;

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

impl App {
    pub(in crate::app) fn open_library(&mut self) {
        self.popups.library_view = Some(LibraryView {
            query: LineInput::default(),
            selected: 0,
            confirm_delete: false,
            insert: false,
            offset: 0,
        });
    }

    /// Rows of the library window for its search text: matching saved
    /// queries, then matching history (newest first, without duplicates).
    pub fn library_items(&self) -> Vec<LibraryItem> {
        let query = self.popups.library_view.as_ref().map_or("", |v| v.query.text());
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

    pub(in crate::app) fn handle_library_key(&mut self, key: KeyEvent) {
        let items = self.library_items();
        let Some(view) = &mut self.popups.library_view else { return };
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
            KeyCode::Esc => self.popups.library_view = None,
            KeyCode::Char('d') if ctrl => {
                if matches!(items.get(view.selected), Some(LibraryItem::Saved(_))) {
                    view.confirm_delete = true;
                }
            }
            KeyCode::F(4) => {
                if let Some(LibraryItem::Saved(id)) = items.get(view.selected) {
                    let text = self.library.get(id).map(library::SavedQuery::path).unwrap_or_default();
                    self.popups.tab_prompt =
                        Some(TabPrompt { kind: TabPromptKind::RenameSaved(id.clone()), text: LineInput::new(text) });
                }
            }
            KeyCode::Enter => {
                let Some(item) = items.get(view.selected).cloned() else { return };
                let insert = view.insert;
                self.popups.library_view = None;
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
    pub(in crate::app) fn insert_library_item(&mut self, item: &LibraryItem) {
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
    pub(in crate::app) fn open_library_item(&mut self, item: &LibraryItem) {
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

    pub(in crate::app) fn library_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        let (list, first) = self.hits.library_list;
        // (Counted only for what moves or picks: not on every mouse move.)
        let moves = matches!(ev.kind, MouseEventKind::ScrollDown | MouseEventKind::ScrollUp | MouseEventKind::Down(_));
        let count = if moves { self.library_items().len() } else { 0 };
        let Some(view) = &mut self.popups.library_view else { return };
        // A delete asked about is for the row it was asked on.
        if matches!(ev.kind, MouseEventKind::ScrollDown | MouseEventKind::ScrollUp) {
            view.confirm_delete = false;
        }
        match ev.kind {
            MouseEventKind::ScrollDown => view.selected = (view.selected + 1).min(count.saturating_sub(1)),
            MouseEventKind::ScrollUp => view.selected = view.selected.saturating_sub(1),
            MouseEventKind::Down(MouseButton::Left) if inside(list, x, y) => {
                let row = first + usize::from(y - list.y);
                if row >= count {
                    return;
                }
                let double =
                    self.panes.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y)
                        && view.selected == row;
                view.selected = row;
                view.confirm_delete = false;
                self.panes.last_click = Some((Instant::now(), x, y));
                if double {
                    self.handle_library_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            _ => {}
        }
    }
}
