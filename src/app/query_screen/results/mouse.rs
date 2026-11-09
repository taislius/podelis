//! The results pane's mouse: its top rows (the result tabs, `export ▾` /
//! `menu ▾`, the kept results and the statement above them), a row and a
//! column picked (a double click opens the cell), a message, and the wheel
//! through the rows (sideways through the columns).

use super::super::super::mouse::{WHEEL_LINES, inside};
use crate::app::*;

impl App {
    /// The results pane's top rows: its tabs, `export ▾` / `menu ▾`, above
    /// a result the kept results' numbers and its statement, and the line
    /// above a plan. True if `x`, `y` was one.
    pub(in crate::app) fn click_results_top(&mut self, hits: &Hits, x: u16, y: u16) -> bool {
        let find = |spots: &[(u16, u16, usize)]| spots.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x)).copied();
        if !inside(hits.results, x, y) {
            return false;
        }
        if y == hits.result_tabs_y
            && let Some(&(x0, _, export)) = hits.result_buttons.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x))
        {
            self.focus = Focus::Results;
            let at = Some((x0, y + 1));
            if export { self.run_command(commands::Command::ExportMenu) } else { self.open_menu(at) }
        } else if y == hits.result_tabs_y
            && let Some(&(_, _, tab)) = hits.result_tabs.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x))
        {
            self.focus = Focus::Results;
            // Clicking the shown RESULT tab again steps through a script's result sets.
            if tab == ResultTab::Rows && self.tab.result_tab == ResultTab::Rows && self.tab.grid.result_sets.len() > 1 {
                self.step_result_set(true);
            } else {
                self.set_result_tab(tab);
            }
        } else if hits.run_tabs_y == Some(y)
            && let Some((_, _, i)) = find(&hits.run_tabs)
        {
            self.focus = Focus::Results;
            self.show_run(i);
        } else if inside(hits.run_source, x, y) {
            self.go_to_run_statement();
        } else if inside(hits.plan_source, x, y) {
            // The line above a plan: to its statement in the editor.
            self.go_to_plan_statement();
        } else {
            return false;
        }
        true
    }

    /// A message, a row and column of the grid, or the rest of the pane.
    /// True if it was the results'.
    pub(in crate::app) fn click_results(&mut self, hits: &Hits, x: u16, y: u16, double: bool) -> bool {
        if inside(hits.messages, x, y) && self.tab.result_tab == ResultTab::Messages {
            self.focus = Focus::Results;
            self.click_message(y, double);
            return true;
        }
        if inside(hits.result_rows, x, y) && self.grid_shown() {
            self.focus = Focus::Results;
            // As in the navigator: a click on a row ends typing the filter.
            self.tab.grid.filter_editing = false;
            let row = self.tab.grid.row_offset + (y - hits.result_rows.y) as usize;
            if row < self.tab.grid.view.len() {
                self.tab.grid.table_state.select(Some(row));
                if let Some(&(_, _, col)) = hits.result_cols.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x)) {
                    self.tab.grid.selected_col = col;
                }
                self.maybe_fetch_more();
                if double {
                    self.open_cell_view();
                }
            }
            return true;
        }
        if inside(hits.results, x, y) {
            self.focus = Focus::Results;
            // A failure with its place: a click on it goes there.
            if inside(hits.result_body, x, y)
                && self.tab.run_error.is_some()
                && !self.grid_shown()
                && self.tab.result_tab == ResultTab::Rows
                && let Some((start, end)) = self.tab.error_marker
            {
                self.show_error_place(start, end);
            }
            return true;
        }
        false
    }

    /// A sideways wheel over the results: the next column, or the plan
    /// and messages scrolled across.
    pub(in crate::app) fn wheel_across(&mut self, x: u16, y: u16, dir: i16) {
        if !inside(self.hits.results, x, y) {
            return;
        }
        if !self.grid_shown() {
            return self.scroll_tab_across(dir);
        }
        let cols = self.tab.grid.results.as_ref().map_or(0, |r| r.columns.len());
        let col = self.tab.grid.selected_col;
        self.tab.grid.selected_col =
            if dir > 0 { (col + 1).min(cols.saturating_sub(1)) } else { col.saturating_sub(1) };
    }

    /// The wheel over the results: the selected row (more loaded as it
    /// nears the end), or the plan and messages scrolled.
    pub(in crate::app) fn wheel_results(&mut self, down: bool) {
        if !self.grid_shown() {
            let delta = if down { WHEEL_LINES as i16 } else { -(WHEEL_LINES as i16) };
            let delta = if self.tab.result_tab == ResultTab::Messages { -delta } else { delta };
            self.tab.tab_scroll = self.tab.tab_scroll.saturating_add_signed(delta);
            return;
        }
        let len = self.tab.grid.view.len();
        if len == 0 {
            return;
        }
        let at = self.tab.grid.table_state.selected().unwrap_or(0);
        let next = if down { (at + WHEEL_LINES).min(len - 1) } else { at.saturating_sub(WHEEL_LINES) };
        self.tab.grid.table_state.select(Some(next));
        self.maybe_fetch_more();
    }
}
