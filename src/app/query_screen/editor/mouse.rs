//! The editor's mouse: a click places the cursor, a drag selects (and,
//! held past the edge, scrolls on), the wheel moves the view; the tabs row
//! (a click switches, a middle click closes) and the footer's syntax hint
//! and write modes.

use super::super::super::mouse::{WHEEL_LINES, inside};
use crate::app::*;

/// How often a drag held past the editor's edge scrolls it on.
const DRAG_REPEAT: Duration = Duration::from_millis(60);

impl App {
    /// A drag selecting in the editor, at (`x`, `y`). Past the top or the
    /// bottom of the text it scrolls that way, faster the further out, and
    /// the selection goes on; held there, it keeps scrolling
    /// ([`Self::drag_scroll`]), as editors do.
    pub(in crate::app) fn drag_editor(&mut self, x: u16, y: u16) {
        let area = self.hits.editor_text;
        let height = usize::from(area.height);
        let before = self.tab.editor.scroll_row;
        let out = if y < area.y {
            self.tab.editor.scroll_view(-isize::from(i16::try_from(area.y - y).unwrap_or(i16::MAX)), height);
            true
        } else if y >= area.bottom() && area.height > 0 {
            self.tab.editor.scroll_view(isize::from(i16::try_from(y + 1 - area.bottom()).unwrap_or(i16::MAX)), height);
            true
        } else {
            false
        };
        // Until there's no further to go.
        let moved = self.tab.editor.scroll_row != before;
        self.panes.drag_out = (out && moved).then(|| (x, y, Instant::now() + DRAG_REPEAT));
        let y = y.clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
        let pos = self.editor_pos_at(x, y);
        let anchor = self.tab.editor.anchor().unwrap_or_else(|| self.tab.editor.cursor());
        self.tab.editor.set_cursor(pos);
        self.tab.editor.set_anchor((anchor != pos).then_some(anchor));
    }

    /// When a drag held past the editor's edge scrolls on.
    pub fn drag_scroll_due(&self) -> Option<Instant> {
        self.panes.drag_out.filter(|_| self.panes.dragging).map(|(_, _, due)| due)
    }

    /// The pointer kept still past the editor's edge: scroll on a step,
    /// while the editor is what's there (a dialog over it, or another
    /// screen, stops it).
    pub fn drag_scroll(&mut self) {
        let editor_there = self.screen == Screen::Query && !self.popup_open();
        match self.panes.drag_out.filter(|_| self.panes.dragging && editor_there) {
            Some((x, y, _)) => self.on_input(|app| app.drag_editor(x, y)),
            None => self.panes.drag_out = None,
        }
    }

    /// A middle click on a tab closes it (asking first if it has unsaved
    /// changes), as in browsers.
    pub(in crate::app) fn middle_click_tab(&mut self, x: u16) {
        let Some(&(_, _, tab)) = self.hits.tabs.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x)) else { return };
        let (was, count) = (self.active_tab, self.tabs.len());
        self.switch_tab(tab);
        if self.active_tab == tab {
            self.request_close_tab();
        }
        // Not closed, and not asking about it (a query running): back to
        // the tab that was shown, as browsers do.
        if self.tabs.len() == count && self.popups.confirm.is_none() && was != tab {
            self.switch_tab(was);
        } else if self.tabs.len() < count && was != tab {
            self.switch_tab(if was > tab { was - 1 } else { was });
        }
    }

    /// A tab, the `+`, or a run button. True if `x` was one.
    fn click_tab_row(&mut self, hits: &Hits, x: u16) -> bool {
        let at = |x0: u16, x1: u16| (x0..x1).contains(&x);
        if let Some(&(_, _, tab)) = hits.tabs.iter().find(|(x0, x1, _)| at(*x0, *x1)) {
            self.switch_tab(tab);
        } else if hits.tab_more.iter().any(|&(x0, x1)| at(x0, x1)) {
            self.open_tab_list();
        } else if hits.new_tab.is_some_and(|(x0, x1)| at(x0, x1)) {
            self.new_tab(None, "", None);
        } else if let Some(&(_, _, action)) = hits.run_buttons.iter().find(|(x0, x1, _)| at(*x0, *x1)) {
            self.focus = Focus::Editor;
            self.run_action(action);
        } else {
            return false;
        }
        true
    }

    /// A syntax hint: to the spot in the editor it's about. True if it was.
    pub(in crate::app) fn click_syntax_hint(&mut self, hits: &Hits, x: u16, y: u16) -> bool {
        if !inside(hits.syntax_hint, x, y) {
            return false;
        }
        let Some(offset) = self.tab.syntax_hint.as_ref().and_then(|h| h.offset) else { return false };
        let pos = self.tab.editor.pos_of_offset(offset);
        self.tab.editor.clear_selection();
        self.tab.editor.set_cursor(pos);
        self.focus = Focus::Editor;
        true
    }

    /// The footer's write modes: their help opens the picker, the mode
    /// itself goes to the next. True if it was one.
    pub(in crate::app) fn click_tx_footer(&mut self, hits: &Hits, x: u16, y: u16) -> bool {
        if inside(hits.tx_help, x, y) {
            self.open_tx_picker();
        } else if inside(hits.tx_mode, x, y) {
            self.cycle_tx_mode();
        } else {
            return false;
        }
        true
    }

    /// The tabs row, the text (the cursor placed, a drag begun) or the rest
    /// of the pane. True if it was the editor's.
    pub(in crate::app) fn click_editor(&mut self, hits: &Hits, x: u16, y: u16) -> bool {
        if y == hits.tabs_y && inside(hits.editor, x, y) && self.click_tab_row(hits, x) {
            // The tabs are the editor's, even while one can't be switched to.
            self.focus = Focus::Editor;
            return true;
        }
        if inside(hits.editor_text, x, y) {
            self.focus = Focus::Editor;
            self.popups.completion = None;
            let pos = self.editor_pos_at(x, y);
            self.tab.editor.clear_selection();
            self.tab.editor.set_cursor(pos);
            self.tab.editor.set_anchor(Some(pos));
            self.panes.dragging = true;
            return true;
        }
        if inside(hits.editor, x, y) {
            self.focus = Focus::Editor;
            return true;
        }
        false
    }

    /// The view moves, not the cursor: a selection stays whole.
    pub(in crate::app) fn wheel_editor(&mut self, down: bool) {
        let rows = if down { WHEEL_LINES as isize } else { -(WHEEL_LINES as isize) };
        self.tab.editor.scroll_view(rows, self.panes.editor_height);
    }

    /// The editor position under screen cell (x, y), clamped to the text.
    pub(in crate::app) fn editor_pos_at(&self, x: u16, y: u16) -> Pos {
        let area = self.hits.editor_text;
        let row = (self.tab.editor.scroll_row + y.saturating_sub(area.y) as usize).min(self.tab.editor.lines.len() - 1);
        let display = self.tab.editor.scroll_col + x.saturating_sub(area.x) as usize;
        (row, self.tab.editor.col_at_display(row, display))
    }
}
