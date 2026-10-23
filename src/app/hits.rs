//! Where the UI last drew what can be clicked, for the mouse.

use ratatui::layout::Rect;

use super::ResultTab;

/// Where the UI last drew the clickable parts, for the mouse.
#[derive(Clone, Debug, Default)]
pub struct Hits {
    pub schema: Option<Rect>,
    /// The schema's filter line (a click starts typing in it).
    pub schema_filter: Rect,
    /// The schema tree's rows, and the index of the first one shown.
    pub schema_list: Rect,
    pub schema_offset: usize,
    /// The connection in the status bar (a click shows its info).
    pub connection: Rect,
    /// The note about what agents did, in the status bar (a click opens
    /// the agent window).
    pub agent_note: Rect,
    /// The project's name in the top bar (a click switches project).
    pub project: Rect,
    pub editor: Rect,
    pub editor_text: Rect,
    /// The write modes in the editor's footer.
    pub tx_mode: Rect,
    /// The line above a plan saying what it's for (a click goes there).
    pub plan_source: Rect,
    /// The same above a result, and its kept results' numbers: x ranges
    /// and indices into `runs`.
    pub run_source: Rect,
    pub run_tabs: Vec<(u16, u16, usize)>,
    /// The MESSAGES lines, and the index of the first one shown.
    pub messages: Rect,
    pub messages_offset: usize,
    /// QUERY PLAN's text, and how many lines of notes come above the plan
    /// itself: a plan line's row is worked out from them and the scroll
    /// as it is now (`plan_lines.rs`), never kept from a frame before.
    pub plan: Rect,
    pub plan_notes: usize,
    /// The syntax hint under the editor (a click goes to what it's about).
    pub syntax_hint: Rect,
    /// Editor tab strip: (row, then each tab's x range and index).
    pub tabs: Vec<(u16, u16, usize)>,
    pub tabs_y: u16,
    /// "‹N" / "›N" (tabs out of sight): a click lists them all.
    pub tab_more: Vec<(u16, u16)>,
    /// The `+` after the tabs.
    pub new_tab: Option<(u16, u16)>,
    /// The run buttons right of the tabs: x ranges and actions.
    pub run_buttons: Vec<(u16, u16, crate::config::keys::Action)>,
    pub results: Rect,
    /// The result table's body rows, and each visible column's x range.
    pub result_rows: Rect,
    pub result_cols: Vec<(u16, u16, usize)>,
    /// The open pop-up menu.
    pub menu: Rect,
    /// The library window's rows, and the index of the first one shown.
    pub library_list: (Rect, usize),
    /// The projects and connections panes: each list's rows and the
    /// index of the first one shown, and the settings pane.
    pub manager_projects: (Rect, usize),
    pub manager_connections: (Rect, usize),
    pub manager_settings: Rect,
    /// The results pane's tabs: their x ranges, and their row.
    pub result_tabs: Vec<(u16, u16, ResultTab)>,
    /// `export ▾` and `menu ▾` right of them: x ranges, and whether it's
    /// the export menu.
    pub result_buttons: Vec<(u16, u16, bool)>,
    pub result_tabs_y: u16,
}
