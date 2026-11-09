//! The query screen's mouse: what's in front takes it (a menu, every key's
//! window, the library, a small popup), then which pane a click, a drag or
//! the wheel is over; each pane's own is in its `mouse.rs`. A pane's title
//! zooms it on a double click; a right-click opens its menu.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::super::mouse::{WHEEL_LINES, inside};
use crate::app::*;

impl App {
    pub(in crate::app) fn query_screen_mouse(&mut self, ev: MouseEvent) {
        let (x, y) = (ev.column, ev.row);
        let click = matches!(ev.kind, MouseEventKind::Down(MouseButton::Left));
        if self.popups.menu.is_some() {
            return self.menu_mouse(ev.kind, x, y);
        }
        if self.help.visible {
            return self.help_mouse(ev.kind);
        }
        // The library: a click picks a row, a double click opens it, the
        // wheel moves through them.
        if self.popups.library_view.is_some() && self.top_overlay() == Some(Overlay::Library) {
            return self.library_mouse(ev);
        }
        if click && self.click_small_popup(x, y) {
            return;
        }
        // Over a popup only the wheel does something (scrolling it).
        if self.popup_open() {
            if let Some(view) = &mut self.popups.cell_view {
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
                self.popups.completion = None;
                self.click_plan(y, ev.modifiers.contains(KeyModifiers::SHIFT));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.double_click(x, y);
                self.click(x, y, double);
            }
            MouseEventKind::Down(MouseButton::Middle) if y == self.hits.tabs_y && inside(self.hits.editor, x, y) => {
                self.middle_click_tab(x);
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
                    self.popups.completion = None;
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

    /// A click in front of the popups that take one. False if none is open.
    fn click_small_popup(&mut self, x: u16, y: u16) -> bool {
        match self.top_overlay() {
            Some(Overlay::ExportForm) => self.export_form_click(x, y),
            Some(Overlay::TxPicker) => self.tx_picker_click(x, y),
            _ => return false,
        }
        true
    }

    /// Over a menu: a click chooses, the wheel moves.
    fn menu_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        let Some(menu) = &mut self.popups.menu else { return };
        match kind {
            MouseEventKind::Down(_) => self.click_menu(x, y),
            MouseEventKind::ScrollDown => menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1)),
            MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(1),
            _ => {}
        }
    }

    /// Over every key's window: the wheel scrolls it.
    fn help_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollDown => self.help.scroll = self.help.scroll.saturating_add(WHEEL_LINES as u16),
            MouseEventKind::ScrollUp => self.help.scroll = self.help.scroll.saturating_sub(WHEEL_LINES as u16),
            _ => {}
        }
    }

    pub(in crate::app) fn popup_open(&self) -> bool {
        self.top_overlay().is_some() || self.help.visible
    }

    /// A click on the screen: its bars, then the pane under it.
    fn click(&mut self, x: u16, y: u16, double: bool) {
        let hits = self.hits.clone();
        if inside(hits.connection, x, y) && self.connected() {
            return self.show_connection_info();
        }
        if inside(hits.agent_note, x, y) {
            return self.open_agents();
        }
        if self.click_results_top(&hits, x, y) || self.click_syntax_hint(&hits, x, y) {
            return;
        }
        if inside(hits.editor_note, x, y) || inside(hits.results_note, x, y) {
            return self.open_messages();
        }
        if self.click_tx_footer(&hits, x, y) {
            return;
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
        if self.click_editor(&hits, x, y, double) || self.click_results(&hits, x, y, double) {
            return;
        }
        if hits.schema.is_some_and(|r| inside(r, x, y)) {
            self.click_schema(x, y, double);
        }
    }

    /// The wheel over a pane: its view, or its selection, moves.
    fn wheel(&mut self, x: u16, y: u16, down: bool) {
        if inside(self.hits.editor, x, y) {
            self.wheel_editor(down);
        } else if inside(self.hits.results, x, y) {
            self.wheel_results(down);
        } else if self.hits.schema.is_some_and(|r| inside(r, x, y)) {
            self.wheel_navigator(down);
        }
    }
}
