//! Mouse support on the query screen: click to focus a pane, switch tabs,
//! place the cursor or pick a row; drag to select; wheel to scroll; double
//! click to open a cell or expand a table.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::*;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_LINES: usize = 3;

pub(super) fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl App {
    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        let click = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
        // The project's name in the top bar opens the projects pane.
        if click && inside(self.hits.project, x, y) && self.screen != Screen::Projects && !self.busy_for_clicks() {
            return self.open_manager(Screen::Projects);
        }
        if !matches!(self.screen, Screen::Query) {
            if click {
                let double =
                    self.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y);
                self.last_click = Some((Instant::now(), x, y));
                self.click_manager(x, y, double);
            }
            return;
        }
        if self.menu.is_some() {
            if matches!(ev.kind, MouseEventKind::Down(_)) {
                self.click_menu(x, y);
            }
            return;
        }
        // Over a popup only the wheel does something (scrolling it).
        if self.popup_open() {
            if let Some(view) = &mut self.cell_view {
                match ev.kind {
                    MouseEventKind::ScrollDown => view.scroll = view.scroll.saturating_add(WHEEL_LINES as u16),
                    MouseEventKind::ScrollUp => view.scroll = view.scroll.saturating_sub(WHEEL_LINES as u16),
                    _ => {}
                }
            }
            return;
        }
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double =
                    self.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y);
                self.last_click = Some((Instant::now(), x, y));
                self.click(x, y, double);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let pos = self.editor_pos_at(x, y);
                let anchor = self.tab.editor.anchor().unwrap_or_else(|| self.tab.editor.cursor());
                self.tab.editor.set_cursor(pos);
                self.tab.editor.set_anchor((anchor != pos).then_some(anchor));
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = false,
            // Right-click: select what's under the pointer, then its menu.
            MouseEventKind::Down(MouseButton::Right) => {
                self.click(x, y, false);
                self.dragging = false;
                let on_pane = [self.hits.editor, self.hits.results].iter().any(|r| inside(*r, x, y))
                    || self.hits.schema.is_some_and(|r| inside(r, x, y));
                if on_pane {
                    self.open_menu(Some((x, y)));
                }
            }
            MouseEventKind::ScrollDown => self.wheel(x, y, true),
            MouseEventKind::ScrollUp => self.wheel(x, y, false),
            _ => {}
        }
    }

    pub(super) fn popup_open(&self) -> bool {
        self.top_overlay().is_some() || self.help_visible
    }

    /// A tab, the `+`, or a run button. True if `x` was one.
    fn click_tab_row(&mut self, hits: &Hits, x: u16) -> bool {
        let at = |x0: u16, x1: u16| (x0..x1).contains(&x);
        if let Some(&(_, _, tab)) = hits.tabs.iter().find(|(x0, x1, _)| at(*x0, *x1)) {
            self.switch_tab(tab);
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

    fn click(&mut self, x: u16, y: u16, double: bool) {
        let hits = self.hits.clone();
        if inside(hits.connection, x, y) && self.connected() {
            return self.show_connection_info();
        }
        // The results pane's tabs.
        if y == hits.result_tabs_y
            && inside(hits.results, x, y)
            && let Some(&(_, _, tab)) = hits.result_tabs.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x))
        {
            self.focus = Focus::Results;
            // Clicking the shown RESULT tab again steps through a script's result sets.
            if tab == ResultTab::Rows && self.tab.result_tab == ResultTab::Rows && self.tab.grid.result_sets.len() > 1 {
                return self.step_result_set(true);
            }
            self.tab.result_tab = tab;
            self.tab.tab_scroll = 0;
            return;
        }
        if inside(hits.tx_mode, x, y) {
            return self.cycle_tx_mode();
        }
        // Double-clicking a pane's title zooms it (and back).
        let title_of = |r: Rect| y == r.y && inside(r, x, y);
        let titled = if title_of(hits.results) {
            Some(Focus::Results)
        } else if title_of(hits.editor) {
            Some(Focus::Editor)
        } else {
            hits.schema.filter(|r| title_of(*r)).map(|_| Focus::Schema)
        };
        if let Some(pane) = titled {
            self.focus = pane;
            if double {
                self.toggle_zoom();
            }
            return;
        }
        if y == hits.tabs_y && inside(hits.editor, x, y) && self.click_tab_row(&hits, x) {
            return;
        }
        if inside(hits.editor_text, x, y) {
            self.focus = Focus::Editor;
            self.completion = None;
            let pos = self.editor_pos_at(x, y);
            self.tab.editor.clear_selection();
            self.tab.editor.set_cursor(pos);
            self.tab.editor.set_anchor(Some(pos));
            self.dragging = true;
            return;
        }
        if inside(hits.editor, x, y) {
            self.focus = Focus::Editor;
            return;
        }
        if inside(hits.result_rows, x, y) && self.tab.result_tab == ResultTab::Rows {
            self.focus = Focus::Results;
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
            return;
        }
        if inside(hits.results, x, y) {
            self.focus = Focus::Results;
            return;
        }
        if hits.schema.is_some_and(|r| inside(r, x, y)) {
            self.focus = Focus::Schema;
            if !inside(hits.schema_list, x, y) {
                return;
            }
            let index = hits.schema_offset + (y - hits.schema_list.y) as usize;
            let Some(panel) = &mut self.schema else { return };
            let nodes = panel.nodes();
            let Some(&node) = nodes.get(index) else { return };
            if matches!(node, SchemaNode::Section(_)) {
                return;
            }
            panel.selected = index;
            if double {
                match node {
                    SchemaNode::Table(t) => {
                        panel.tables[t].expanded = !panel.tables[t].expanded;
                        if panel.tables[t].expanded {
                            self.request_details(t);
                        }
                    }
                    SchemaNode::Routine(r) => self.show_routine(r),
                    _ => {}
                }
            }
        }
    }

    fn wheel(&mut self, x: u16, y: u16, down: bool) {
        let hits = &self.hits;
        if inside(hits.editor, x, y) {
            for _ in 0..WHEEL_LINES {
                self.tab.editor.clear_selection();
                if down { self.tab.editor.move_down() } else { self.tab.editor.move_up() }
            }
        } else if inside(hits.results, x, y) {
            if self.tab.result_tab != ResultTab::Rows {
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
        } else if hits.schema.is_some_and(|r| inside(r, x, y))
            && let Some(panel) = &mut self.schema
        {
            let nodes = panel.nodes();
            let mut at = panel.selected;
            for _ in 0..WHEEL_LINES {
                at = if down {
                    query::step_selectable(&nodes, at + 1, 1)
                } else {
                    query::step_selectable(&nodes, at.saturating_sub(1), -1)
                };
            }
            panel.selected = at;
        }
    }

    /// The editor position under screen cell (x, y), clamped to the text.
    fn editor_pos_at(&self, x: u16, y: u16) -> Pos {
        let area = self.hits.editor_text;
        let row = (self.tab.editor.scroll_row + y.saturating_sub(area.y) as usize).min(self.tab.editor.lines.len() - 1);
        let display = self.tab.editor.scroll_col + x.saturating_sub(area.x) as usize;
        (row, self.tab.editor.col_at_display(row, display))
    }
}
