//! The popups that can open over the query screen, in one place: which one
//! is in front decides who gets the keys, what's drawn on top, and whether
//! clicks and `?` reach the panes beneath.

use super::App;

/// A popup over the query screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Confirm,
    Menu,
    PipePrompt,
    /// The question to ask a model (`Ctrl+K q`).
    AskPrompt,
    /// The project's memory (`Ctrl+K k`).
    Memory,
    RowForm,
    Palette,
    /// The text view: a cell's full value, a definition, a report.
    CellView,
    Activity,
    /// What AI agents did.
    Agents,
    TxPicker,
    Review,
    ParamForm,
    TabPrompt,
    Library,
    /// The settings (`Ctrl+K ,`).
    Settings,
    Completion,
}

impl Overlay {
    /// Whether it takes typed text (so `?` is a character, not the keys).
    /// Completion is open while typing in the editor.
    pub fn takes_text(self) -> bool {
        matches!(
            self,
            Overlay::PipePrompt
                | Overlay::AskPrompt
                | Overlay::Memory
                | Overlay::RowForm
                | Overlay::Palette
                | Overlay::ParamForm
                | Overlay::TabPrompt
                | Overlay::Library
                | Overlay::Settings
                | Overlay::Agents
                | Overlay::Completion
        )
    }
}

impl App {
    /// Each popup and whether it's open, front first. The text view sits
    /// in front of the activity monitor (a session's full query) and
    /// behind the rest.
    fn overlay_states(&self) -> [(Overlay, bool); 18] {
        // The text view shows a session's query or an agent's call in front
        // of their windows.
        let over_list = self.activity.is_some() || self.agents.view.is_some();
        [
            (Overlay::Confirm, self.confirm.is_some()),
            (Overlay::Menu, self.menu.is_some()),
            (Overlay::PipePrompt, self.pipe_prompt.is_some()),
            (Overlay::AskPrompt, self.ask_prompt.is_some()),
            (Overlay::Memory, self.memory_view.is_some()),
            (Overlay::RowForm, self.row_form.is_some()),
            (Overlay::Palette, self.palette.is_some()),
            (Overlay::CellView, self.cell_view.is_some() && over_list),
            (Overlay::Activity, self.activity.is_some()),
            (Overlay::Agents, self.agents.view.is_some()),
            (Overlay::TxPicker, self.tx_picker.is_some()),
            (Overlay::Review, self.review.is_some()),
            (Overlay::ParamForm, self.param_form.is_some()),
            (Overlay::TabPrompt, self.tab_prompt.is_some()),
            (Overlay::Library, self.library_view.is_some()),
            (Overlay::Settings, self.settings_view.is_some()),
            (Overlay::CellView, self.cell_view.is_some() && !over_list),
            (Overlay::Completion, self.completion.is_some()),
        ]
    }

    /// The open popups, front first.
    pub fn overlays(&self) -> Vec<Overlay> {
        self.overlay_states().into_iter().filter_map(|(o, open)| open.then_some(o)).collect()
    }

    /// The popup in front, if any.
    pub fn top_overlay(&self) -> Option<Overlay> {
        self.overlay_states().into_iter().find_map(|(o, open)| open.then_some(o))
    }
}
