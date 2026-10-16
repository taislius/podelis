//! Selecting QUERY PLAN's lines: a click selects one, a drag (or a
//! Shift+click) more, `Shift+↑/↓` too; `y` or Ctrl+C copies them (`Y` the
//! whole plan), `Esc` lets go. They're kept by line, so scrolling keeps
//! the selection whole and copies it all, out of sight or not.

use super::*;

impl App {
    /// A key in QUERY PLAN's text; false leaves it to scrolling.
    pub(super) fn handle_plan_key(&mut self, key: KeyEvent) -> bool {
        let Some(lines) = self.plan_line_count() else { return false };
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up | KeyCode::Down if shift => {
                let top = self.plan_top_line().min(lines - 1);
                let (from, at) = self.tab.plan_selected.unwrap_or((top, top));
                let at = if key.code == KeyCode::Up { at.saturating_sub(1) } else { (at + 1).min(lines - 1) };
                self.tab.plan_selected = Some((from, at));
                self.reveal_plan_line(at);
            }
            KeyCode::Char('y') => self.copy_plan(false),
            KeyCode::Char('Y') => self.copy_plan(true),
            KeyCode::Esc if self.tab.plan_selected.is_some() => self.tab.plan_selected = None,
            _ => return false,
        }
        true
    }

    /// Copies the selected lines, else (or with `all`) the whole plan.
    pub(super) fn copy_plan(&mut self, all: bool) {
        let Some(plan) = &self.tab.plan else { return };
        let lines: Vec<&str> = plan.text.lines().collect();
        if let Some((a, b)) = self.tab.plan_selected.filter(|_| !all) {
            let (lo, hi) = (a.min(b), a.max(b).min(lines.len().saturating_sub(1)));
            let text = lines[lo..=hi].join("\n");
            let n = hi - lo + 1;
            self.copy_text(text, &format!("Copied {n} line{} of the plan", if n == 1 { "" } else { "s" }));
        } else {
            let text = plan.text.clone();
            self.copy_text(text, "Copied the plan");
        }
    }

    /// A click on the plan's row `y`: that line selected, or with Shift the
    /// lines from where the selection started to it. A drag goes on from it.
    pub(super) fn click_plan(&mut self, y: u16, shift: bool) {
        let Some(line) = self.plan_line_at(y) else {
            self.tab.plan_selected = None;
            return;
        };
        self.tab.plan_selected = match self.tab.plan_selected {
            Some((from, _)) if shift => Some((from, line)),
            _ => Some((line, line)),
        };
        self.plan_dragging = true;
    }

    /// Dragging over the plan: the selection follows; past its top or
    /// bottom edge the plan scrolls, and the selection with it.
    pub(super) fn drag_plan(&mut self, y: u16) {
        let Some(lines) = self.plan_line_count() else { return };
        let area = self.hits.plan;
        if y < area.y {
            self.tab.tab_scroll = self.tab.tab_scroll.saturating_sub(1);
        } else if y >= area.bottom() {
            // Not past the plan's last line (its notes are above it).
            let notes = self.hits.plan_first_row - i32::from(area.y) + i32::from(self.tab.tab_scroll);
            let last = notes + i32::try_from(lines).unwrap_or(i32::MAX) - i32::from(area.height);
            if i32::from(self.tab.tab_scroll) < last {
                self.tab.tab_scroll += 1;
            }
        }
        let y = y.clamp(area.y, area.bottom().saturating_sub(1));
        let line = self.plan_line_at(y).unwrap_or(if y == area.y { 0 } else { lines - 1 });
        if let Some((from, _)) = self.tab.plan_selected {
            self.tab.plan_selected = Some((from, line.min(lines - 1)));
        }
    }

    /// The plan's text lines, if a text plan is shown.
    fn plan_line_count(&self) -> Option<usize> {
        let plan = self.tab.plan.as_ref().filter(|_| !self.grid_shown())?;
        Some(plan.text.lines().count()).filter(|n| *n > 0)
    }

    /// The plan line drawn on screen row `y` (its notes above aren't any).
    fn plan_line_at(&self, y: u16) -> Option<usize> {
        let lines = self.plan_line_count()?;
        let line = i32::from(y) - self.hits.plan_first_row;
        usize::try_from(line).ok().filter(|l| *l < lines)
    }

    /// The first plan line in sight.
    fn plan_top_line(&self) -> usize {
        usize::try_from(i32::from(self.hits.plan.y) - self.hits.plan_first_row).unwrap_or(0)
    }

    /// Scrolls so plan line `line` is in sight.
    fn reveal_plan_line(&mut self, line: usize) {
        let area = self.hits.plan;
        let row = self.hits.plan_first_row + i32::try_from(line).unwrap_or(i32::MAX);
        let shift = if row < i32::from(area.y) {
            row - i32::from(area.y)
        } else if row >= i32::from(area.bottom()) {
            row - i32::from(area.bottom()) + 1
        } else {
            0
        };
        self.tab.tab_scroll = u16::try_from((i32::from(self.tab.tab_scroll) + shift).max(0)).unwrap_or(u16::MAX);
    }
}
