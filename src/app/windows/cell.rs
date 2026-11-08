//! A text in a window of its own: a cell's full value, a definition, a
//! report; scrolled, copied, and (a row as `column  value` lines) moved
//! to the previous or next row.

use crate::app::*;

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

impl App {
    pub(in crate::app) fn handle_cell_view_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.popups.cell_view else { return };
        let page = self.panes.editor_height.max(5) as u16;
        // A row (`x`): past its end ↓ goes on to the next row, and ↑ above
        // its start to the previous one.
        let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
        let up = matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
        if view.record && ((down && view.scroll >= view.max_scroll) || (up && view.scroll == 0)) {
            return self.step_record(up);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'v') => self.popups.cell_view = None,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(page),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(page),
            KeyCode::Home => view.scroll = 0,
            KeyCode::Char('y') if view.sql.is_some() => {
                let text = view.sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
                self.copy_text(text, t!("copy.sql"));
            }
            KeyCode::Char('e') if view.follow.is_some() => {
                let Some((tab, sql)) = view.follow.take() else { return };
                self.open_followed(tab, &sql);
            }
            KeyCode::Char('y') if view.follow.is_some() => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.row"));
            }
            KeyCode::Char('e') if view.sql.is_some() => {
                let Some((sql, conn)) = view.sql.take() else { return };
                self.popups.cell_view = None;
                self.open_tab_on(conn, &sql);
            }
            KeyCode::Char('y') if view.browse => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.definition"));
            }
            KeyCode::Char('y') if view.record => {
                let text = view.text.clone();
                self.copy_text(text, t!("copy.row"));
            }
            KeyCode::Char('y') if view.copies_text.is_some() => {
                let (text, said) = (view.text.clone(), view.copies_text.unwrap_or_default());
                self.copy_text(text, said);
            }
            KeyCode::Char('y') => self.copy_cell(),
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') if view.browse => {
                self.step_definition(matches!(key.code, KeyCode::Left | KeyCode::Char('h')));
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') if view.record => {
                self.step_record(matches!(key.code, KeyCode::Left | KeyCode::Char('h')));
            }
            KeyCode::Char('x') if view.record => self.popups.cell_view = None,
            _ => {}
        }
    }
}
