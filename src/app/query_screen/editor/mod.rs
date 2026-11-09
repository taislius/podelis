//! The editor pane: its keys and its mouse, find and replace, vim's modes.

pub mod find;
mod keys;
pub(in crate::app) mod lint;
mod mouse;
pub mod vim;

use std::time::Instant;

use crate::text_editor::Pos;

/// The editor's own state beside the tab's text.
#[derive(Default)]
pub struct Editing {
    /// The find/replace bar (Ctrl+F).
    pub find: Option<find::FindBar>,
    /// Vim state, used when `editor_mode = "vim"`.
    pub(in crate::app) vim: vim::Vim,
    /// When and where the editor last got a typed Space, for
    /// `leader_double_space`.
    pub(in crate::app) last_space: Option<(Instant, Pos)>,
    /// Esc was the last key in the editor: Tab then moves to the next pane
    /// even right after a word.
    pub(in crate::app) esc_then_tab: bool,
    /// When the checks run, once typing pauses.
    pub(in crate::app) lint_due: Option<Instant>,
}
