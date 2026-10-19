//! Mouse support on the query screen: click to focus a pane, switch tabs,
//! place the cursor or pick a row; drag to select; wheel to scroll; double
//! click to open a cell or expand a table.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::*;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const WHEEL_LINES: usize = 3;
/// How often a drag held past the editor's edge scrolls it on.
const DRAG_REPEAT: Duration = Duration::from_millis(60);

pub(super) fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl App {
    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        self.on_input(|app| app.mouse(ev));
    }

    fn mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        // Only a drag goes on scrolling (a lost button-up mustn't).
        if !matches!(ev.kind, MouseEventKind::Drag(_)) {
            self.panes.drag_out = None;
        }
        let click = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
        // The project's name in the top bar opens the projects pane.
        if click && inside(self.hits.project, x, y) && self.screen != Screen::Projects && !self.busy_for_clicks() {
            return self.open_manager(Screen::Projects);
        }
        if !matches!(self.screen, Screen::Query) {
            if click {
                let double =
                    self.panes.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y);
                self.panes.last_click = Some((Instant::now(), x, y));
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
            // QUERY PLAN's text: lines selected (Shift extends), dragged on.
            MouseEventKind::Down(MouseButton::Left)
                if inside(self.hits.plan, x, y) && self.tab.result_tab == ResultTab::Plan && !self.grid_shown() =>
            {
                self.focus = Focus::Results;
                self.completion = None;
                self.click_plan(y, ev.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let double =
                    self.panes.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y);
                self.panes.last_click = Some((Instant::now(), x, y));
                self.click(x, y, double);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.panes.plan_dragging => self.drag_plan(y),
            MouseEventKind::Drag(MouseButton::Left) if self.panes.dragging => self.drag_editor(x, y),
            MouseEventKind::Up(MouseButton::Left) => {
                self.panes.dragging = false;
                self.panes.plan_dragging = false;
            }
            // Right-click: select what's under the pointer, then its menu.
            MouseEventKind::Down(MouseButton::Right) => {
                // Inside the editor's selection it stays, for Copy and Cut.
                let pos = self.editor_pos_at(x, y);
                let on_selection = inside(self.hits.editor_text, x, y)
                    && self.tab.editor.selection().is_some_and(|(a, b)| (a..=b).contains(&pos));
                if on_selection {
                    self.focus = Focus::Editor;
                    self.completion = None;
                } else {
                    self.click(x, y, false);
                }
                self.panes.dragging = false;
                let on_pane = [self.hits.editor, self.hits.results].iter().any(|r| inside(*r, x, y))
                    || self.hits.schema.is_some_and(|r| inside(r, x, y));
                if on_pane {
                    self.open_menu(Some((x, y)));
                }
            }
            // Shift turns the wheel sideways (in the results).
            MouseEventKind::ScrollDown if ev.modifiers.contains(KeyModifiers::SHIFT) => self.wheel_across(x, y, 1),
            MouseEventKind::ScrollUp if ev.modifiers.contains(KeyModifiers::SHIFT) => self.wheel_across(x, y, -1),
            MouseEventKind::ScrollRight => self.wheel_across(x, y, 1),
            MouseEventKind::ScrollLeft => self.wheel_across(x, y, -1),
            MouseEventKind::ScrollDown => self.wheel(x, y, true),
            MouseEventKind::ScrollUp => self.wheel(x, y, false),
            _ => {}
        }
    }

    /// A drag selecting in the editor, at (`x`, `y`). Past the top or the
    /// bottom of the text it scrolls that way, faster the further out, and
    /// the selection goes on; held there, it keeps scrolling
    /// ([`Self::drag_scroll`]), as editors do.
    fn drag_editor(&mut self, x: u16, y: u16) {
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

    pub(super) fn popup_open(&self) -> bool {
        self.top_overlay().is_some() || self.help_visible
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

    fn click(&mut self, x: u16, y: u16, double: bool) {
        let hits = self.hits.clone();
        if inside(hits.connection, x, y) && self.connected() {
            return self.show_connection_info();
        }
        if inside(hits.agent_note, x, y) {
            return self.open_agents();
        }
        if self.click_results_top(&hits, x, y) {
            return;
        }
        // A syntax hint: to the spot in the editor it's about.
        if inside(hits.syntax_hint, x, y)
            && let Some(offset) = self.tab.syntax_hint.as_ref().and_then(|h| h.offset)
        {
            let pos = self.tab.editor.pos_of_offset(offset);
            self.tab.editor.clear_selection();
            self.tab.editor.set_cursor(pos);
            self.focus = Focus::Editor;
            return;
        }
        // The line above a plan: to its statement in the editor.
        if inside(hits.plan_source, x, y) {
            return self.go_to_plan_statement();
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
            // The tabs are the editor's, even while one can't be switched to.
            self.focus = Focus::Editor;
            return;
        }
        if inside(hits.editor_text, x, y) {
            self.focus = Focus::Editor;
            self.completion = None;
            let pos = self.editor_pos_at(x, y);
            self.tab.editor.clear_selection();
            self.tab.editor.set_cursor(pos);
            self.tab.editor.set_anchor(Some(pos));
            self.panes.dragging = true;
            return;
        }
        if inside(hits.editor, x, y) {
            self.focus = Focus::Editor;
            return;
        }
        if inside(hits.messages, x, y) && self.tab.result_tab == ResultTab::Messages {
            self.focus = Focus::Results;
            return self.click_message(y, double);
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
            return;
        }
        if inside(hits.results, x, y) {
            self.focus = Focus::Results;
            return;
        }
        if hits.schema.is_some_and(|r| inside(r, x, y)) {
            self.click_schema(x, y, double);
        }
    }

    /// The results pane's top rows: its tabs, `export ▾` / `menu ▾`, and
    /// above a result the kept results' numbers and its statement. True
    /// if `x`, `y` was one.
    fn click_results_top(&mut self, hits: &Hits, x: u16, y: u16) -> bool {
        let find = |spots: &[(u16, u16, usize)]| spots.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x)).copied();
        if !inside(hits.results, x, y) {
            return false;
        }
        if y == hits.result_tabs_y
            && let Some(&(x0, _, export)) = hits.result_buttons.iter().find(|(x0, x1, _)| (*x0..*x1).contains(&x))
        {
            self.focus = Focus::Results;
            let at = Some((x0, y + 1));
            if export { self.open_export_menu(at) } else { self.open_menu(at) }
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
        } else if y == hits.run_source.y
            && let Some((_, _, i)) = find(&hits.run_tabs)
        {
            self.focus = Focus::Results;
            self.show_run(i);
        } else if inside(hits.run_source, x, y) {
            self.go_to_run_statement();
        } else {
            return false;
        }
        true
    }

    /// A click in the navigator: its filter line, or a row of the tree.
    fn click_schema(&mut self, x: u16, y: u16, double: bool) {
        let hits = &self.hits;
        self.focus = Focus::Schema;
        if inside(hits.schema_filter, x, y) {
            if let Some(panel) = &mut self.connection.schema {
                panel.filter_editing = true;
            }
            return;
        }
        let on_list = inside(hits.schema_list, x, y);
        let index = hits.schema_offset + y.saturating_sub(hits.schema_list.y) as usize;
        let Some(panel) = &mut self.connection.schema else { return };
        // Clicking past the filter line ends its typing (the filter stays):
        // keys then go to the tree, to what was clicked.
        panel.filter_editing = false;
        if !on_list {
            return;
        }
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

    /// A sideways wheel over the results: the next column, or the plan
    /// and messages scrolled across.
    fn wheel_across(&mut self, x: u16, y: u16, dir: i16) {
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

    fn wheel(&mut self, x: u16, y: u16, down: bool) {
        let hits = &self.hits;
        // The view moves, not the cursor: a selection stays whole.
        if inside(hits.editor, x, y) {
            let rows = if down { WHEEL_LINES as isize } else { -(WHEEL_LINES as isize) };
            self.tab.editor.scroll_view(rows, self.panes.editor_height);
        } else if inside(hits.results, x, y) {
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
        } else if hits.schema.is_some_and(|r| inside(r, x, y))
            && let Some(panel) = &mut self.connection.schema
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
