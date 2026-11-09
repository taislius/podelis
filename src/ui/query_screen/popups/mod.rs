//! The query screen's popups, drawn back to front over its panes: each
//! kind in its file (`menus`, `prompts`, `windows`, `writes`); those of
//! every screen are `ui::popups`.

mod menus;
mod prompts;
mod windows;
mod writes;

use super::*;

/// Every popup open, back to front; `cursor` is the editor's, for the
/// completion list under it.
pub(super) fn draw_popups(f: &mut Frame, app: &mut App, cursor: Option<(u16, u16)>) {
    let area = f.area();
    for overlay in app.overlays().into_iter().rev() {
        match overlay {
            Overlay::Completion => {
                if let Some(cursor) = cursor {
                    super::editor::draw_completion(f, app, cursor, area);
                }
            }
            Overlay::CellView => crate::ui::popups::draw_cell_view(f, app, area),
            Overlay::Library => windows::draw_library(f, app, area),
            Overlay::Settings => crate::ui::settings::draw_settings(f, app, area),
            Overlay::TabPrompt => prompts::draw_tab_prompt(f, app, area),
            Overlay::ParamForm => prompts::draw_param_form(f, app, area),
            Overlay::Review => writes::draw_review(f, app, area),
            Overlay::TxPicker => writes::draw_tx_picker(f, app, area),
            Overlay::ExportForm => writes::draw_export_form(f, app, area),
            Overlay::Sandbox => writes::draw_sandbox(f, app, area),
            Overlay::Activity => windows::draw_activity(f, app, area),
            Overlay::Agents => crate::ui::agents::draw_agents(f, app, area),
            Overlay::Palette => menus::draw_palette(f, app, area),
            Overlay::RowForm => prompts::draw_row_form(f, app, area),
            Overlay::PipePrompt => {
                let command = app.popups.pipe_prompt.clone().unwrap_or_default();
                prompts::draw_pipe_prompt(f, &command, area);
            }
            Overlay::AskPrompt => prompts::draw_ask_prompt(f, app, area),
            Overlay::Memory => windows::draw_memory(f, app, area),
            Overlay::Menu => menus::draw_menu(f, app, area),
            Overlay::Confirm => crate::ui::popups::draw_confirm(f, app, area),
        }
    }
}
