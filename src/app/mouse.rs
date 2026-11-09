//! The mouse, wherever it's used: the top bar's project (on any screen),
//! the manager screens' clicks, then the query screen's
//! (`query_screen::mouse`, which hands each pane its own: `editor::mouse`,
//! `results::mouse`, `navigator::mouse`). What they share: a double click,
//! the wheel's step, `inside`.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::*;

pub(in crate::app) const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Lines (or rows) a turn of the wheel moves.
pub(in crate::app) const WHEEL_LINES: usize = 3;

pub(in crate::app) fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl App {
    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        self.on_input(|app| app.mouse(ev));
    }

    /// A click at (`x`, `y`), remembered: whether it's the second of a
    /// double click.
    pub(in crate::app) fn double_click(&mut self, x: u16, y: u16) -> bool {
        let double = self.panes.last_click.is_some_and(|(t, cx, cy)| t.elapsed() < DOUBLE_CLICK && cx == x && cy == y);
        self.panes.last_click = Some((Instant::now(), x, y));
        double
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
                let double = self.double_click(x, y);
                self.click_manager(x, y, double);
            }
            return;
        }
        self.query_screen_mouse(ev);
    }
}
