//! The results' keys: moving, sorting, filtering, the cell and record
//! views, copying, and scrolling the result tabs.

use super::super::*;

impl App {
    pub(in crate::app) fn handle_results_key(&mut self, key: KeyEvent) {
        let tabs = [ResultTab::Rows, ResultTab::Messages, ResultTab::Plan];
        let current = tabs.iter().position(|t| *t == self.tab.result_tab).unwrap_or(0);
        let switch = match key.code {
            KeyCode::Char('1') => Some(0),
            KeyCode::Char('2') => Some(1),
            KeyCode::Char('3') => Some(2),
            KeyCode::Char(']') => Some((current + 1) % 3),
            KeyCode::Char('[') => Some((current + 2) % 3),
            _ => None,
        };
        if let Some(i) = switch {
            return self.set_result_tab(tabs[i]);
        }
        match key.code {
            KeyCode::Char('m') => return self.open_menu(None),
            // The results kept from earlier runs.
            // (Not over a plan's table, which isn't a run's result.)
            KeyCode::Char(',') if !self.tab.plan_swapped => return self.step_run(false),
            KeyCode::Char('.') if !self.tab.plan_swapped => return self.step_run(true),
            KeyCode::Char('p') if !self.tab.plan_swapped => return self.toggle_pin_run(),
            _ => {}
        }
        if self.tab.result_tab == ResultTab::Messages && self.handle_messages_key(key) {
            return;
        }
        if self.tab.result_tab == ResultTab::Plan && !self.grid_shown() && self.handle_plan_key(key) {
            return;
        }
        if !self.grid_shown() {
            return self.handle_tab_scroll_key(key);
        }
        // A plan's table can be looked at, not changed.
        if self.tab.plan_swapped && matches!(key.code, KeyCode::Char('c' | 'a' | 'd' | 'f' | 'M' | 'i' | '<' | '>')) {
            return self.set_status("That's for query results, not the plan".to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        let cols = results.columns.len();
        if cols == 0 {
            return;
        }
        match key.code {
            KeyCode::Char('/') => return self.tab.grid.filter_editing = true,
            KeyCode::Esc if !self.tab.grid.filter.is_empty() => {
                self.tab.grid.filter.clear();
                return self.tab.grid.refresh_view();
            }
            KeyCode::Char('s') => return self.cycle_sort(),
            KeyCode::Char('b') => return self.chart_column(),
            KeyCode::Char('e') => return self.export(ExportFormat::Csv),
            KeyCode::Char('J') => return self.export(ExportFormat::Json),
            KeyCode::Char('i') => return self.export(ExportFormat::Sql),
            KeyCode::Char('M') => return self.load_more(),
            KeyCode::Char('c') => return self.start_cell_edit(),
            KeyCode::Char('a') => return self.start_row_insert(),
            KeyCode::Char('d') => return self.delete_rows(),
            KeyCode::Char('C') => return self.copy_markdown(),
            KeyCode::Char('z') => return self.toggle_zoom(),
            KeyCode::Char('>') => return self.step_result_set(true),
            KeyCode::Char('<') => return self.step_result_set(false),
            KeyCode::Char('X') => return self.toggle_vertical(),
            _ => {}
        }
        let len = self.tab.grid.view.len();
        if len == 0 {
            return;
        }
        if self.tab.grid.vertical && self.vertical_move(key.code, len, cols) {
            return self.maybe_fetch_more();
        }
        let current = self.tab.grid.table_state.selected().unwrap_or(0);
        let row = match key.code {
            KeyCode::Char('g') => 0,
            KeyCode::Char('G') => len - 1,
            _ => list::step(current, len, key, Nav::LIST).unwrap_or(current),
        };
        self.tab.grid.table_state.select(Some(row));
        self.maybe_fetch_more();
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.tab.grid.selected_col = self.tab.grid.selected_col.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab.grid.selected_col = (self.tab.grid.selected_col + 1).min(cols - 1);
            }
            KeyCode::Char('0' | '^') => self.tab.grid.selected_col = 0,
            KeyCode::Char('$') => self.tab.grid.selected_col = cols - 1,
            KeyCode::Char('y') => self.copy_cell(),
            KeyCode::Char('Y') => self.copy_row(),
            KeyCode::Enter | KeyCode::Char('v') => self.open_cell_view(),
            KeyCode::Char('x') => self.open_record_view(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('f') => self.follow_foreign_key(),
            KeyCode::Char('|') => self.open_pipe_prompt(),
            KeyCode::Char('o') => self.open_in_viewer(),
            _ => {}
        }
    }

    /// `X`: the rows one column per line (like mysql's `\G`), or the table.
    pub(in crate::app) fn toggle_vertical(&mut self) {
        let grid = &mut self.tab.grid;
        grid.vertical = !grid.vertical;
        grid.vertical_by_g = false;
        let msg = if grid.vertical { "One column per line (X: back to the table)" } else { "Table view" };
        self.set_status(msg.to_string(), false);
    }

    /// Moving in the vertical layout: ↑/↓ go field by field, on into the
    /// next or previous row; PageUp/PageDown a whole row. True if handled.
    pub(in crate::app) fn vertical_move(&mut self, code: KeyCode, len: usize, cols: usize) -> bool {
        let grid = &mut self.tab.grid;
        let row = grid.table_state.selected().unwrap_or(0);
        let col = grid.selected_col;
        let (row, col) = match code {
            KeyCode::Down | KeyCode::Char('j') if col + 1 < cols => (row, col + 1),
            KeyCode::Down | KeyCode::Char('j') if row + 1 < len => (row + 1, 0),
            KeyCode::Up | KeyCode::Char('k') if col > 0 => (row, col - 1),
            KeyCode::Up | KeyCode::Char('k') if row > 0 => (row - 1, cols - 1),
            KeyCode::Down | KeyCode::Up | KeyCode::Char('j' | 'k') => (row, col),
            KeyCode::PageDown => ((row + 1).min(len - 1), col),
            KeyCode::PageUp => (row.saturating_sub(1), col),
            _ => return false,
        };
        grid.table_state.select(Some(row));
        grid.selected_col = col;
        true
    }

    /// The selected row as shown (after filtering/sorting).
    pub(in crate::app) fn selected_row(&self) -> Option<&Vec<Option<String>>> {
        let results = self.tab.grid.results.as_ref()?;
        let idx = *self.tab.grid.view.get(self.tab.grid.table_state.selected()?)?;
        results.rows.get(idx)
    }

    pub(in crate::app) fn handle_filter_key(&mut self, key: KeyEvent) {
        match self.tab.grid.filter.handle(key) {
            InputResult::Submit => self.tab.grid.filter_editing = false,
            InputResult::Cancel => {
                self.tab.grid.filter_editing = false;
                self.tab.grid.filter.clear();
            }
            InputResult::Changed => {}
            InputResult::Moved | InputResult::Ignored => return,
        }
        self.tab.grid.refresh_view();
    }

    /// Ascending → descending → unsorted, on the selected column.
    pub(in crate::app) fn cycle_sort(&mut self) {
        let col = self.tab.grid.selected_col;
        self.tab.grid.sort = match self.tab.grid.sort {
            Some((c, false)) if c == col => Some((col, true)),
            Some((c, true)) if c == col => None,
            _ => Some((col, false)),
        };
        self.tab.grid.refresh_view();
    }

    pub(in crate::app) fn open_cell_view(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let Some(row) = self.selected_row() else { return };
        let Some(cell) = row.get(self.tab.grid.selected_col) else { return };
        let title = results.columns[self.tab.grid.selected_col].clone();
        let text = cell.as_deref().map_or_else(|| NULL_TEXT.to_string(), pretty_value);
        self.cell_view = Some(CellView::new(title, text));
    }

    /// `x`: the selected row as one `column  value` line per column.
    pub(in crate::app) fn open_record_view(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let Some(row) = self.selected_row() else { return };
        let name_w = results.columns.iter().map(|c| c.chars().count()).max().unwrap_or(0).min(30);
        let mut text = String::new();
        for (name, cell) in results.columns.iter().zip(row) {
            let value = cell.as_deref().map_or_else(|| NULL_TEXT.to_string(), pretty_value);
            // Continuation lines of multi-line values line up with the first.
            let value = value.replace('\n', &format!("\n{:w$}  ", "", w = name_w));
            let _ = writeln!(text, "{name:<name_w$}  {value}");
        }
        let at = self.tab.grid.table_state.selected().unwrap_or(0);
        let title = format!("ROW {} of {}", at + 1, self.tab.grid.view.len());
        self.cell_view = Some(CellView { record: true, ..CellView::new(title, text) });
    }

    /// The previous or next row in the row view (`x`), from its top.
    pub(in crate::app) fn step_record(&mut self, back: bool) {
        let at = self.tab.grid.table_state.selected().unwrap_or(0);
        let next = if back { at.saturating_sub(1) } else { (at + 1).min(self.tab.grid.view.len().saturating_sub(1)) };
        if next == at {
            return;
        }
        self.tab.grid.table_state.select(Some(next));
        self.maybe_fetch_more();
        self.open_record_view();
    }

    pub(in crate::app) fn handle_cell_view_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.cell_view else { return };
        let page = self.editor_height.max(5) as u16;
        // A row (`x`): past its end ↓ goes on to the next row, and ↑ above
        // its start to the previous one.
        let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
        let up = matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
        if view.record && ((down && view.scroll >= view.max_scroll) || (up && view.scroll == 0)) {
            return self.step_record(up);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'v') => self.cell_view = None,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(page),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(page),
            KeyCode::Home => view.scroll = 0,
            KeyCode::Char('y') if view.sql.is_some() => {
                let text = view.sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
                self.copy_text(text, "Copied the SQL");
            }
            KeyCode::Char('e') if view.follow.is_some() => {
                let Some((tab, sql)) = view.follow.take() else { return };
                self.open_followed(tab, &sql);
            }
            KeyCode::Char('y') if view.follow.is_some() => {
                let text = view.text.clone();
                self.copy_text(text, "Copied the row");
            }
            KeyCode::Char('e') if view.sql.is_some() => {
                let Some((sql, conn)) = view.sql.take() else { return };
                self.cell_view = None;
                self.open_tab_on(conn, &sql);
            }
            KeyCode::Char('y') if view.browse => {
                let text = view.text.clone();
                self.copy_text(text, "Copied the definition");
            }
            KeyCode::Char('y') if view.record => {
                let text = view.text.clone();
                self.copy_text(text, "Copied the row");
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
            KeyCode::Char('x') if view.record => self.cell_view = None,
            _ => {}
        }
    }

    pub(in crate::app) fn copy_cell(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let Some(cell) = row.get(self.tab.grid.selected_col) else { return };
        let text = cell.clone().unwrap_or_default();
        let status = format!("Copied cell ({} chars)", text.chars().count());
        self.copy_text(text, &status);
    }

    /// The marked rows, or else the current one, a line each (tab-separated).
    pub(in crate::app) fn copy_row(&mut self) {
        let line = |cells: &[Option<String>]| {
            cells.iter().map(|c| c.as_deref().unwrap_or("").replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")
        };
        let marked = &self.tab.grid.marked;
        if marked.is_empty() {
            let Some(cells) = self.selected_row() else { return };
            let text = line(cells);
            return self.copy_text(text, "Copied row (tab-separated)");
        }
        let Some(results) = self.tab.grid.results.as_ref() else { return };
        let lines: Vec<_> = marked.iter().filter_map(|&i| results.rows.get(i)).map(|r| line(r)).collect();
        let status = format!("Copied {} rows (tab-separated, a line each)", lines.len());
        self.copy_text(lines.join("\n"), &status);
    }

    /// Keys for the MESSAGES and QUERY PLAN tabs: scrolling. The plan
    /// scrolls down from its top; messages scroll up from the newest line.
    /// ←/→ scroll across (drawing stops at the widest line).
    pub(in crate::app) fn handle_tab_scroll_key(&mut self, key: KeyEvent) {
        let page = self.editor_height.max(5) as u16;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => return self.scroll_tab_across(-1),
            KeyCode::Right | KeyCode::Char('l') => return self.scroll_tab_across(1),
            KeyCode::Char('0') => {
                self.tab.tab_hscroll = 0;
                return;
            }
            _ => {}
        }
        let delta: i32 = match key.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown => i32::from(page),
            KeyCode::PageUp => -i32::from(page),
            KeyCode::Home => {
                self.tab.tab_scroll = 0;
                self.tab.tab_hscroll = 0;
                return;
            }
            _ => return,
        };
        let delta = if self.tab.result_tab == ResultTab::Messages { -delta } else { delta };
        self.tab.tab_scroll = self.tab.tab_scroll.saturating_add_signed(delta.clamp(-1000, 1000) as i16);
    }

    /// Scrolls the MESSAGES / QUERY PLAN text a few columns left or right.
    pub(in crate::app) fn scroll_tab_across(&mut self, dir: i16) {
        const STEP: i16 = 8;
        self.tab.tab_hscroll = self.tab.tab_hscroll.saturating_add_signed(dir * STEP);
    }
}
