//! The query screen's keys: what each pane does with them, the panes'
//! sizes and zoom, focus, copy and paste. Each pane's own keys, running
//! queries, plans and exports are in the files beside this one.

mod editor_keys;
mod export;
mod plan;
mod results_keys;
mod run;
mod schema_keys;

pub(super) use schema_keys::step_selectable;

use super::*;
use crate::config::keys::Action;

impl App {
    pub(in crate::app) fn handle_query_key(&mut self, key: KeyEvent) {
        let after_esc = std::mem::take(&mut self.esc_then_tab);
        self.esc_then_tab = key.code == KeyCode::Esc && self.focus == Focus::Editor;
        if self.overlay_key(key) {
            return;
        }
        // Ctrl+U and Ctrl+W edit the input being typed in, before any key
        // binding (Ctrl+W also closes the tab).
        let edit_key = key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Char('u' | 'w'));
        if edit_key && self.cell_view.is_none() {
            let schema_filter = self.connection.schema.as_ref().is_some_and(|s| s.filter_editing);
            if self.cell_edit.is_some() {
                return self.handle_cell_edit_key(key);
            }
            match self.focus {
                Focus::Results if self.tab.grid.filter_editing => return self.handle_filter_key(key),
                Focus::Schema if schema_filter => return self.handle_schema_key(key),
                Focus::Editor if self.find.is_some() => return self.handle_find_key(key),
                _ => {}
            }
        }
        // The find bar takes typing; bound actions (run, palette…) still work.
        if self.find.is_some() && self.focus == Focus::Editor && self.keymap.action(key).is_none() {
            return self.handle_find_key(key);
        }
        let action = self.keymap.action(key);
        if let Some(a) = action.filter(|a| a.is_tab_action()) {
            return self.run_action(a);
        }
        if self.handle_tab_key(key) {
            return;
        }
        if self.cell_view.is_some() {
            return self.handle_cell_view_key(key);
        }
        if self.focus == Focus::Results && self.tab.grid.filter_editing {
            return self.handle_filter_key(key);
        }
        if self.cell_edit.is_some() {
            return self.handle_cell_edit_key(key);
        }
        if self.focus == Focus::Editor && self.completion.is_some() && self.handle_completion_key(key) {
            return;
        }
        // Tab after a partial word completes it; otherwise (or after Esc)
        // it moves focus.
        if key.code == KeyCode::Tab && !after_esc && self.tab_completes() {
            return self.open_completion();
        }
        match action {
            Some(Action::Complete) if self.focus == Focus::Editor && self.editor_inserting() => {
                return self.open_completion();
            }
            Some(Action::Complete) | None => {}
            Some(a) => return self.run_bound(a, key),
        }
        match key.code {
            KeyCode::Esc if self.export_task.is_some() => return self.stop_export(),
            // Esc leaves the zoom first (before cancelling a query), after a
            // results filter (which it clears first) and outside vim mode in
            // the editor (where it's vim's).
            KeyCode::Esc
                if self.panes.zoomed
                    && (self.focus != Focus::Results || self.tab.grid.filter.is_empty())
                    && (self.focus != Focus::Editor || self.vim_mode().is_none()) =>
            {
                return self.leave_zoom_on_esc(false);
            }
            // Esc leaves the statement's pop-over once it has nothing else
            // to do in the editor (a selection, vim's modes).
            KeyCode::Esc if self.panes.statement_zoom && self.focus == Focus::Editor && !self.esc_has_a_job() => {
                return self.leave_zoom_on_esc(true);
            }
            // Then a query running here: the zooms come first, so a second
            // Esc cancels it. (In another tab, Esc is that tab's.)
            KeyCode::Esc if self.running_here() => return self.cancel_query(),
            // Esc stops watching, unless it has a job here: clearing a
            // results filter, or vim's in the editor.
            KeyCode::Esc
                if self.watch.is_some()
                    && (self.focus != Focus::Results || self.tab.grid.filter.is_empty())
                    && (self.focus != Focus::Editor || self.vim_mode().is_none()) =>
            {
                return self.stop_watch("");
            }
            // A question being answered: Esc stops it once it has nothing
            // else to do here (the zooms, cancelling a query, watch mode,
            // a filter, vim's modes, a selection come first).
            KeyCode::Esc if self.asking.is_some() && !self.esc_has_a_job() => return self.stop_asking(),
            // Another tab's query on this connection isn't stopped from
            // here: Esc says whose it is and how to get there.
            KeyCode::Esc if self.busy_conn_tab().is_some() && !self.esc_has_a_job() => {
                return self.set_status(self.running_elsewhere(t!("run.esc_there")), false);
            }
            KeyCode::Tab => return self.cycle_focus(true),
            KeyCode::BackTab => return self.cycle_focus(false),
            _ => {}
        }

        if key.modifiers == KeyModifiers::CONTROL && self.resize_panes(key.code) {
            return;
        }
        match self.focus {
            Focus::Editor => self.handle_editor_key(key),
            Focus::Results => self.handle_results_key(key),
            Focus::Schema => self.handle_schema_key(key),
        }
    }

    /// Whether Esc does something in the focused pane: clears the results
    /// filter, leaves a vim mode, or drops the editor's selection.
    pub(in crate::app) fn esc_has_a_job(&self) -> bool {
        match self.focus {
            Focus::Results => !self.tab.grid.filter.is_empty(),
            Focus::Schema => self.connection.schema.as_ref().is_some_and(|s| !s.filter.is_empty()),
            Focus::Editor => {
                self.vim_mode().is_some_and(|m| m != VimMode::Normal) || self.tab.editor.selection().is_some()
            }
        }
    }

    /// Ctrl+↑/↓ move the border between the editor and the results;
    /// Ctrl+←/→ move the navigator's (not from the editor, where they move
    /// by word). True if handled.
    pub(in crate::app) fn resize_panes(&mut self, code: KeyCode) -> bool {
        const STEP: u16 = 5;
        if self.panes.zoomed {
            return false;
        }
        match code {
            KeyCode::Up => self.panes.editor_percent = self.panes.editor_percent.saturating_sub(STEP).max(10),
            KeyCode::Down => self.panes.editor_percent = (self.panes.editor_percent + STEP).min(90),
            KeyCode::Left | KeyCode::Right if self.focus != Focus::Editor && self.panes.schema_shown => {
                // From the width drawn, border included (the limits it's
                // drawn within depend on the screen).
                let Some(area) = self.hits.schema else { return false };
                self.panes.schema_width = Some(match code {
                    KeyCode::Left => area.width.saturating_sub(2),
                    _ => area.width + 2,
                });
            }
            _ => return false,
        }
        true
    }

    /// Pastes into what has the typing: the popup in front, else the cell
    /// being edited, a filter, the find bar or the editor.
    pub(in crate::app) fn paste_on_query_screen(&mut self, text: &str) {
        match self.top_overlay() {
            Some(Overlay::PipePrompt) => {
                if let Some(input) = &mut self.pipe_prompt {
                    input.insert_str(text);
                }
            }
            Some(Overlay::AskPrompt) => {
                if let Some(input) = &mut self.ask_prompt {
                    input.insert_str(text);
                }
            }
            Some(Overlay::Memory) => {
                if let Some(input) = self.memory_view.as_mut().and_then(|v| v.adding.as_mut()) {
                    input.insert_str(text);
                }
            }
            Some(Overlay::Palette) => {
                if let Some(palette) = &mut self.palette {
                    palette.searching = true;
                    palette.query.insert_str(text);
                    palette.selected = 0;
                }
            }
            Some(Overlay::TabPrompt) => {
                if let Some(prompt) = &mut self.tab_prompt {
                    prompt.text.insert_str(text);
                }
            }
            Some(Overlay::Library) => {
                if let Some(view) = &mut self.library_view {
                    view.query.insert_str(text);
                    view.selected = 0;
                }
            }
            Some(Overlay::ParamForm) => self.paste_param(text),
            Some(Overlay::Settings) => self.paste_setting(text),
            Some(Overlay::RowForm) => self.paste_row_form(text),
            // Popups without text to type into.
            Some(
                Overlay::Confirm
                | Overlay::Menu
                | Overlay::CellView
                | Overlay::Activity
                | Overlay::Agents
                | Overlay::TxPicker
                | Overlay::Sandbox
                | Overlay::Review,
            ) => {}
            Some(Overlay::Completion) | None => self.paste_into_pane(text),
        }
    }

    /// Ctrl+C: copies the editor's selection (or its line), the open cell,
    /// the results row (or the marked rows), or the navigator's name.
    pub(in crate::app) fn copy_focused(&mut self) {
        if self.cell_view.is_some() {
            return self.copy_cell();
        }
        let has_row = self.grid_shown() && self.selected_row().is_some();
        match self.focus {
            _ if self.popup_open() => {}
            Focus::Editor => {
                let ed = &self.tab.editor;
                if let Some(text) = ed.selected_text() {
                    return self.copy_text(text, t!("copy.selection"));
                }
                let row = ed.cursor().0;
                let (start, end) = (ed.offset_of((row, 0)), ed.offset_of((row, ed.line_len(row))));
                let line = ed.text()[start..end].to_string();
                if !line.trim().is_empty() {
                    return self.copy_text(line, &t!("copy.line", line = row + 1));
                }
            }
            Focus::Results if has_row => return self.copy_row(),
            Focus::Results if self.tab.result_tab == ResultTab::Messages && !self.notes.log.is_empty() => {
                return self.copy_message();
            }
            Focus::Results if self.tab.result_tab == ResultTab::Plan && self.tab.plan.is_some() => {
                return self.copy_plan(false);
            }
            Focus::Schema => {
                if let Some((name, what)) = self.schema_name() {
                    return self.copy_text(name.clone(), &t!("copy.name", what = what, name = name));
                }
            }
            Focus::Results => {}
        }
        self.set_status(t!("copy.nothing").to_string(), false);
    }

    /// The navigator's selection as it's written in SQL (a table with its
    /// schema), and what it is.
    pub(in crate::app) fn schema_name(&self) -> Option<(String, &'static str)> {
        let panel = self.connection.schema.as_ref()?;
        let engine = self.engine();
        let quote = |name: &str| engine.quote_ident(name);
        Some(match *panel.nodes().get(panel.selected)? {
            SchemaNode::Table(t) => {
                let info = &panel.tables[t].info;
                (
                    format!("{}.{}", quote(&info.schema), quote(&info.name)),
                    if info.is_view { t!("kind.view") } else { t!("kind.table") },
                )
            }
            SchemaNode::Column(t, c) => (quote(&panel.tables[t].columns[c].name), t!("kind.column")),
            SchemaNode::Index(t, i) => {
                (quote(&panel.tables[t].details.as_ref()?.indexes.get(i)?.name), t!("kind.index"))
            }
            // Its name with the argument types, as Postgres shows overloads.
            SchemaNode::Routine(r) => (panel.routines.get(r)?.name.clone(), t!("kind.routine")),
            SchemaNode::Section(_) => return None,
        })
    }

    /// Pastes the system clipboard (Ctrl+V, or the editor's menu).
    pub(in crate::app) fn paste_clipboard(&mut self) {
        match crate::system::clipboard::read() {
            Ok(text) => self.paste_into_pane(&text),
            Err(e) => self.set_status(crate::system::clipboard::read_failed(&e), true),
        }
    }

    pub(in crate::app) fn cut_selection(&mut self) {
        if let Some(text) = self.tab.editor.cut() {
            self.edited();
            self.copy_text(text, t!("copy.cut"));
        }
    }

    pub(in crate::app) fn paste_into_pane(&mut self, text: &str) {
        if let Some(edit) = &mut self.cell_edit {
            return edit.text.insert_str(text);
        }
        match self.focus {
            Focus::Results if self.tab.grid.filter_editing => {
                self.tab.grid.filter.insert_str(text);
                self.tab.grid.refresh_view();
            }
            Focus::Schema => {
                if let Some(panel) = self.connection.schema.as_mut().filter(|p| p.filter_editing) {
                    panel.filter.insert_str(text);
                    panel.selected = step_selectable(&panel.nodes(), 0, 1);
                }
            }
            Focus::Editor if self.find.is_some() => self.paste_find(text),
            Focus::Editor => {
                self.tab.editor.insert_str(text);
                self.edited();
            }
            Focus::Results => {}
        }
    }

    /// Tab and library keys, available from every pane. True if handled.
    /// Alt+1…9 switch tabs (Ctrl+K 1…9 does too, and works where Alt
    /// doesn't). True if handled.
    pub(in crate::app) fn handle_tab_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c @ '1'..='9') if key.modifiers == KeyModifiers::ALT => {
                self.switch_tab(c as usize - '1' as usize);
                true
            }
            _ => false,
        }
    }

    /// The popup in front takes the keys; true if one did. The text view
    /// and completion let tab keys through, so they're handled further on.
    fn overlay_key(&mut self, key: KeyEvent) -> bool {
        match self.top_overlay() {
            Some(Overlay::Menu) => self.handle_menu_key(key),
            Some(Overlay::PipePrompt) => self.handle_pipe_prompt_key(key),
            Some(Overlay::AskPrompt) => self.handle_ask_prompt_key(key),
            Some(Overlay::Memory) => self.handle_memory_key(key),
            Some(Overlay::RowForm) => self.handle_row_form_key(key),
            Some(Overlay::Palette) => self.handle_palette_key(key),
            Some(Overlay::Activity) => self.handle_activity_key(key),
            Some(Overlay::Agents) => self.handle_agents_key(key),
            Some(Overlay::TxPicker) => self.handle_tx_picker_key(key),
            Some(Overlay::Sandbox) => self.handle_sandbox_key(key),
            Some(Overlay::Review) => self.handle_review_key(key),
            Some(Overlay::ParamForm) => self.handle_param_key(key),
            Some(Overlay::TabPrompt) => self.handle_tab_prompt_key(key),
            Some(Overlay::Library) => self.handle_library_key(key),
            Some(Overlay::Settings) => self.handle_settings_key(key),
            Some(Overlay::Confirm | Overlay::CellView | Overlay::Completion) | None => return false,
        }
        true
    }

    /// What `key`, bound to `action`, does: the action, except that
    /// Ctrl+Shift+K (where the terminal tells it from Ctrl+K) shows Ctrl+K's
    /// menu at once, not after a pause (or never, with `leader_menu` off).
    fn run_bound(&mut self, action: Action, key: KeyEvent) {
        let k = matches!(key.code, KeyCode::Char('k' | 'K'));
        if action == Action::Palette && k && key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
            return self.open_palette(true);
        }
        self.run_action(action);
    }

    /// Does what a (configurable) key binding is for.
    pub(in crate::app) fn run_action(&mut self, action: Action) {
        let n = self.tabs.len();
        // A tab whose connection couldn't be opened again: a run tries it
        // again, and goes ahead once it's open.
        if matches!(action, Action::Run | Action::RunStatement | Action::Explain | Action::ExplainAnalyze)
            && let Some(profile) = self.tab_connection_to_retry()
        {
            return self.retry_tab_connection(profile, action);
        }
        match action {
            // With the statement alone in sight, "everything" is it (or
            // the selection): not the rest of the script, out of sight.
            Action::Run if self.panes.statement_zoom => self.execute_current(),
            Action::Run => self.execute_all(),
            Action::RunStatement => self.execute_current(),
            Action::Explain => self.explain_current(),
            Action::Complete => self.open_completion(),
            Action::HistoryPrev => self.history_older(),
            Action::HistoryNext => self.history_newer(),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::ToggleSchema => self.toggle_schema(),
            Action::OpenConnection => self.open_connection(),
            Action::Disconnect => self.request_disconnect(),
            Action::NewTab => self.new_tab(None, "", None),
            Action::CloseTab => self.request_close_tab(),
            Action::NextTab => self.switch_tab((self.active_tab + 1) % n),
            Action::PrevTab => self.switch_tab((self.active_tab + n - 1) % n),
            Action::SaveTab => self.save_tab(),
            Action::SaveStatement => self.save_statement(),
            Action::AiExplain => self.ai_explain(),
            Action::RenameTab => self.rename_tab(),
            Action::Library => self.open_library(),
            Action::Find => self.open_find(),
            Action::Comment => self.toggle_comment(),
            Action::Format => self.format_sql(),
            Action::NextWarning => self.next_warning(),
            Action::Ask => self.open_ask_prompt(),
            Action::Memory => self.open_memory(),
            Action::Palette => self.open_palette(false),
            Action::ConnectionInfo => self.show_connection_info(),
            Action::Activity => self.open_activity(),
            Action::ExplainAnalyze => self.explain_analyze_current(),
            Action::InsertSnippet => self.open_snippets(),
            Action::Menu => self.open_menu(None),
            Action::Zoom => self.toggle_zoom(),
            Action::ZoomStatement => self.toggle_statement_zoom(),
            Action::TabList => self.open_tab_list(),
            Action::Settings => self.open_settings(),
            Action::SelectStatement => self.select_statement(),
            Action::Heartbeat => self.toggle_heartbeat(),
            Action::TxMode => self.open_tx_picker(),
            Action::Projects => self.open_manager(Screen::Projects),
            Action::Commit => self.end_transaction("COMMIT"),
            Action::Rollback => self.end_transaction("ROLLBACK"),
        }
    }

    /// Shows the focused pane full-screen, or the normal layout again.
    pub(in crate::app) fn toggle_zoom(&mut self) {
        self.panes.zoomed = !self.panes.zoomed;
        self.panes.statement_zoom = false;
        self.completion = None;
        let msg =
            if self.panes.zoomed { t!("zoom.pane", key = self.keymap.label(Action::Zoom)) } else { String::new() };
        self.set_status(msg, false);
    }

    /// `Esc` leaves the pane's zoom (or the statement's pop-over): a query
    /// running here doesn't stop, and the status says the next `Esc` does
    /// ("Esc to cancel" said so before).
    fn leave_zoom_on_esc(&mut self, statement: bool) {
        if statement {
            self.toggle_statement_zoom();
        } else {
            self.toggle_zoom();
        }
        // Said only when the next `Esc` is the cancel: no zoom left.
        if self.running_here() && !self.panes.zoomed && !self.panes.statement_zoom {
            self.set_result_status(t!("run.zoom_left_still_running").to_string(), false);
        }
    }

    /// Shows the statement at the cursor (or the selected lines) alone, in
    /// a pop-over over the query screen, or the whole text again. It's the
    /// tab's own editor, so what's typed there is in the tab at once.
    pub(in crate::app) fn toggle_statement_zoom(&mut self) {
        self.panes.statement_zoom = !self.panes.statement_zoom;
        self.panes.zoomed = false;
        self.completion = None;
        if self.panes.statement_zoom {
            self.focus = Focus::Editor;
            self.set_status(t!("zoom.statement", key = self.keymap.label(Action::ZoomStatement)), false);
        } else {
            self.set_status(String::new(), false);
        }
    }

    /// Selects the statement at the cursor (a right-click puts the cursor
    /// under the pointer first), from its first word to its end.
    pub(in crate::app) fn select_statement(&mut self) {
        let text = self.tab.editor.text();
        let Some(range) = sql::statement_range(&text, self.tab.editor.cursor_offset(), self.engine()) else {
            return self.set_status(t!("editor.no_statement_here").into(), true);
        };
        self.focus = Focus::Editor;
        self.completion = None;
        self.tab.editor.select_offsets(range.start, range.end);
        let which = sql::Statements::of(&text, self.engine()).position(range.start);
        let said = which.map_or_else(String::new, |(n, of)| format!(" ({})", t!("editor.n_of", n = n, of = of)));
        self.set_status(t!("editor.selected_statement", said = said), false);
    }

    pub(in crate::app) fn cycle_focus(&mut self, forward: bool) {
        let mut order = vec![Focus::Editor, Focus::Results];
        if self.panes.schema_shown {
            order.push(Focus::Schema);
        }
        let idx = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        self.focus = order[if forward { (idx + 1) % n } else { (idx + n - 1) % n }];
    }
}
