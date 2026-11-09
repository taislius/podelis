//! The panes' layout: the navigator shown or not and its width, the
//! editor's share of the height, the zooms, and the mouse's drags.

use std::time::Instant;

/// What's shown and how big, the zooms, and the mouse's drags.
#[derive(Default)]
pub struct Panes {
    /// The schema navigator is toggled on (Ctrl+K n, F2).
    pub schema_visible: bool,
    /// Whether the schema navigator is actually on screen: hidden in
    /// narrow terminals even when toggled on (`fit_query_screen`).
    pub schema_shown: bool,
    /// Pane sizes moved with Ctrl+arrows: the navigator's width (`None`
    /// for a quarter of the screen) and the editor's share of the height,
    /// in percent.
    pub schema_width: Option<u16>,
    pub editor_percent: u16,
    /// Lines visible in the editor, recorded by the UI for PageUp/PageDown.
    pub editor_height: usize,
    /// The focused pane fills the screen (`z`, Ctrl+K z / F12).
    pub zoomed: bool,
    /// The editor shows only the statement at the cursor (or the
    /// selection's lines), in a pop-over (Ctrl+K b / Shift+F12).
    pub statement_zoom: bool,
    /// For double clicks and drags: the last click (time, column, row), and
    /// whether a drag in the editor is selecting.
    pub(in crate::app) last_click: Option<(Instant, u16, u16)>,
    pub(in crate::app) dragging: bool,
    /// That drag held past the editor's top or bottom: where (column,
    /// row), and when it scrolls on again with the pointer kept still.
    pub(in crate::app) drag_out: Option<(u16, u16, Instant)>,
    /// A drag selecting the plan's lines.
    pub plan_dragging: bool,
}

/// Below this width the navigator is hidden even when toggled on.
const NAVIGATOR_MIN_SCREEN_WIDTH: u16 = 90;

impl crate::app::App {
    /// The query screen is `width` columns wide: whether the navigator fits
    /// (said for the commands that need it), and the keys taken off it when
    /// it's hidden. Asked by the UI as it lays the screen out.
    pub fn fit_query_screen(&mut self, width: u16) -> bool {
        let fits = width >= NAVIGATOR_MIN_SCREEN_WIDTH;
        self.hits.navigator_too_narrow = !fits;
        self.panes.schema_shown = self.panes.schema_visible && fits;
        if !self.panes.schema_shown && self.focus == crate::app::Focus::Schema {
            self.focus = crate::app::Focus::Editor;
        }
        self.panes.schema_shown
    }
}
