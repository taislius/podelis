//! The popups that can open over the query screen, in one place: which one
//! is in front decides who gets the keys, what's drawn on top, and whether
//! clicks and `?` reach the panes beneath.

use crate::app::*;

/// The popups' own state: each open one is `Some`. Which is in front is
/// [`App::overlays`]' order, not this struct's.
#[derive(Default)]
pub struct Popups {
    /// A question waiting for y/n.
    pub confirm: Option<Confirm>,
    /// A pop-up menu of what can be done in the focused pane.
    pub menu: Option<super::menu::Menu>,
    /// The command the rows are piped through (`|`), being typed.
    pub pipe_prompt: Option<LineInput>,
    /// `Ctrl+K q`: the question being typed (the one being answered is
    /// `App::ask`).
    pub ask_prompt: Option<LineInput>,
    /// `Ctrl+K y k`: the project's memory.
    pub memory_view: Option<windows::memory::MemoryView>,
    /// The add-row form (`a`).
    pub row_form: Option<super::results::rows::RowForm>,
    /// The command palette (Ctrl+K).
    pub palette: Option<super::palette::Palette>,
    /// Full-value popup for the selected cell.
    pub cell_view: Option<CellView>,
    /// The activity monitor.
    pub activity: Option<windows::activity::ActivityView>,
    /// `E`: where the rows go, in what format, which of them.
    pub export_form: Option<export_form::ExportForm>,
    /// Picking the write mode: the highlighted one.
    pub tx_picker: Option<TxMode>,
    /// The sandbox's window (`Ctrl+K x` while one is open).
    pub sandbox_view: Option<windows::sandbox::SandboxView>,
    /// Asking to commit or roll back writes run in a transaction.
    pub review: Option<super::review::Review>,
    pub param_form: Option<ParamForm>,
    /// Save-as / rename prompt.
    pub tab_prompt: Option<TabPrompt>,
    /// The F3 library/search window.
    pub library_view: Option<LibraryView>,
    /// The settings screen (Ctrl+K ,).
    pub settings_view: Option<windows::settings::SettingsView>,
    pub completion: Option<Completion>,
}

/// A popup over the query screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Confirm,
    Menu,
    PipePrompt,
    /// The question to ask a model (`Ctrl+K q`).
    AskPrompt,
    /// The project's memory (`Ctrl+K y k`).
    Memory,
    RowForm,
    Palette,
    /// The text view: a cell's full value, a definition, a report.
    CellView,
    Activity,
    /// What AI agents did.
    Agents,
    TxPicker,
    ExportForm,
    /// The sandbox's steps and how to end it.
    Sandbox,
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
    fn overlay_states(&self) -> [(Overlay, bool); 20] {
        // The text view shows a session's query or an agent's call in front
        // of their windows.
        let over_list = self.popups.activity.is_some() || self.agents.view.is_some();
        [
            (Overlay::Confirm, self.popups.confirm.is_some()),
            (Overlay::Menu, self.popups.menu.is_some()),
            (Overlay::PipePrompt, self.popups.pipe_prompt.is_some()),
            (Overlay::AskPrompt, self.popups.ask_prompt.is_some()),
            (Overlay::Memory, self.popups.memory_view.is_some()),
            (Overlay::RowForm, self.popups.row_form.is_some()),
            (Overlay::Palette, self.popups.palette.is_some()),
            (Overlay::CellView, self.popups.cell_view.is_some() && over_list),
            (Overlay::Activity, self.popups.activity.is_some()),
            (Overlay::Agents, self.agents.view.is_some()),
            (Overlay::TxPicker, self.popups.tx_picker.is_some()),
            (Overlay::ExportForm, self.popups.export_form.is_some()),
            (Overlay::Sandbox, self.popups.sandbox_view.is_some()),
            (Overlay::Review, self.popups.review.is_some()),
            (Overlay::ParamForm, self.popups.param_form.is_some()),
            (Overlay::TabPrompt, self.popups.tab_prompt.is_some()),
            (Overlay::Library, self.popups.library_view.is_some()),
            (Overlay::Settings, self.popups.settings_view.is_some()),
            (Overlay::CellView, self.popups.cell_view.is_some() && !over_list),
            (Overlay::Completion, self.popups.completion.is_some()),
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
