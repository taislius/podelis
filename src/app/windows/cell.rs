//! A text in a window of its own: a cell's full value, a definition, a
//! report; scrolled, copied, and (a row as `column  value` lines) moved
//! to the previous or next row.

use crate::app::key_table::{Move, Table, lookup, movement};
use crate::app::*;

/// What a key does in a text window (moving is `MOVE_KEYS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKey {
    Close,
    /// A row's window (`x`) closes with `x` too.
    CloseRow,
    Copy,
    /// The text's SQL, or the rows it follows to, in a tab.
    Open,
}

pub const CELL_KEYS: Table<CellKey> = &[
    (KeyCode::Esc, CellKey::Close),
    (KeyCode::Enter, CellKey::Close),
    (KeyCode::Char('q'), CellKey::Close),
    (KeyCode::Char('v'), CellKey::Close),
    (KeyCode::Char('x'), CellKey::CloseRow),
    (KeyCode::Char('y'), CellKey::Copy),
    (KeyCode::Char('e'), CellKey::Open),
];

pub struct CellView {
    pub title: String,
    pub text: String,
    pub scroll: u16,
    /// Showing the selected row as `column  value` lines (`x`): ←/→ move
    /// to the previous/next row.
    pub record: bool,
    /// SQL that comes with the text (a schema comparison's), and the
    /// connection it's for: `y` copies it, `e` opens it in a tab there.
    pub sql: Option<(String, ConnId)>,
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

impl App {
    pub(in crate::app) fn handle_cell_view_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.cell_view else { return };
        let page = self.panes.editor_height.max(5) as u16;
        if let Some(step) = movement(key) {
            // A row (`x`): past its end ↓ goes on to the next row, and ↑
            // above its start to the previous one.
            let (down, up) = (step == Move::Down, step == Move::Up);
            if view.record && ((down && view.scroll >= view.max_scroll) || (up && view.scroll == 0)) {
                return self.step_record(up);
            }
            match step {
                Move::Down => view.scroll = view.scroll.saturating_add(1),
                Move::Up => view.scroll = view.scroll.saturating_sub(1),
                Move::PageDown => view.scroll = view.scroll.saturating_add(page),
                Move::PageUp => view.scroll = view.scroll.saturating_sub(page),
                Move::Home => view.scroll = 0,
                Move::Left | Move::Right if view.browse => self.step_definition(step == Move::Left),
                Move::Left | Move::Right if view.record => self.step_record(step == Move::Left),
                Move::Left | Move::Right | Move::End => {}
            }
            return;
        }
        let Some(command) = lookup(CELL_KEYS, key) else { return };
        match command {
            CellKey::Close => self.popups.cell_view = None,
            CellKey::CloseRow if view.record => self.popups.cell_view = None,
            CellKey::Copy if view.sql.is_some() => {
                let text = view.sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
                self.copy_text(text, t!("copy.sql"));
            }
            CellKey::Open if view.follow.is_some() => {
                let Some((tab, sql)) = view.follow.take() else { return };
                self.open_followed(tab, &sql);
            }
            CellKey::Copy if view.follow.is_some() => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.row"));
            }
            CellKey::Open if view.sql.is_some() => {
                let Some((sql, conn)) = view.sql.take() else { return };
                self.popups.cell_view = None;
                self.open_tab_on(conn, &sql);
            }
            CellKey::Open | CellKey::CloseRow => {}
            CellKey::Copy if view.browse => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.definition"));
            }
            CellKey::Copy if view.record => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.row"));
            }
            CellKey::Copy if view.copies_text.is_some() => {
                let (text, said) = (view.text.clone(), view.copies_text.unwrap_or_default());
                self.copy_text(text, said);
            }
            CellKey::Copy => self.copy_cell(),
        }
    }
}
