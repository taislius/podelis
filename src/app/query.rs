use super::*;
use crate::keys::Action;

/// How soon a second Space counts as a double press.
const DOUBLE_SPACE: Duration = Duration::from_millis(250);

impl App {
    pub(super) fn handle_query_key(&mut self, key: KeyEvent) {
        let after_esc = std::mem::take(&mut self.esc_then_tab);
        self.esc_then_tab = key.code == KeyCode::Esc && self.focus == Focus::Editor;
        // The popup in front takes the keys; the text view and completion
        // let tab keys through, so they're handled further down.
        match self.top_overlay() {
            Some(Overlay::Menu) => return self.handle_menu_key(key),
            Some(Overlay::PipePrompt) => return self.handle_pipe_prompt_key(key),
            Some(Overlay::AskPrompt) => return self.handle_ask_prompt_key(key),
            Some(Overlay::Memory) => return self.handle_memory_key(key),
            Some(Overlay::RowForm) => return self.handle_row_form_key(key),
            Some(Overlay::Palette) => return self.handle_palette_key(key),
            Some(Overlay::Activity) => return self.handle_activity_key(key),
            Some(Overlay::Agents) => return self.handle_agents_key(key),
            Some(Overlay::TxPicker) => return self.handle_tx_picker_key(key),
            Some(Overlay::Review) => return self.handle_review_key(key),
            Some(Overlay::ParamForm) => return self.handle_param_key(key),
            Some(Overlay::TabPrompt) => return self.handle_tab_prompt_key(key),
            Some(Overlay::Library) => return self.handle_library_key(key),
            Some(Overlay::Confirm | Overlay::CellView | Overlay::Completion) | None => {}
        }
        // Ctrl+U and Ctrl+W edit the input being typed in, before any key
        // binding (Ctrl+W also closes the tab).
        let edit_key = key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Char('u' | 'w'));
        if edit_key && self.cell_view.is_none() {
            let schema_filter = self.schema.as_ref().is_some_and(|s| s.filter_editing);
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
            Some(a) => return self.run_action(a),
        }
        match key.code {
            KeyCode::Esc if self.export_task.is_some() => return self.stop_export(),
            // (In another tab, Esc is that tab's.)
            KeyCode::Esc if self.running_here() => return self.cancel_query(),
            // Esc leaves the zoom, after a results filter (which it clears
            // first) and outside vim mode in the editor (where it's vim's).
            KeyCode::Esc
                if self.zoomed
                    && (self.focus != Focus::Results || self.tab.grid.filter.is_empty())
                    && (self.focus != Focus::Editor || self.vim_mode().is_none()) =>
            {
                return self.toggle_zoom();
            }
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
            // else to do here (cancelling a query, the zoom, watch mode,
            // a filter, vim's modes, a selection come first).
            KeyCode::Esc if self.asking.is_some() && !self.esc_has_a_job() => return self.stop_asking(),
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
    fn esc_has_a_job(&self) -> bool {
        match self.focus {
            Focus::Results => !self.tab.grid.filter.is_empty(),
            Focus::Schema => self.schema.as_ref().is_some_and(|s| !s.filter.is_empty()),
            Focus::Editor => {
                self.vim_mode().is_some_and(|m| m != VimMode::Normal) || self.tab.editor.selection().is_some()
            }
        }
    }

    /// Ctrl+↑/↓ move the border between the editor and the results;
    /// Ctrl+←/→ move the navigator's (not from the editor, where they move
    /// by word). True if handled.
    fn resize_panes(&mut self, code: KeyCode) -> bool {
        const STEP: u16 = 5;
        if self.zoomed {
            return false;
        }
        match code {
            KeyCode::Up => self.editor_percent = self.editor_percent.saturating_sub(STEP).max(10),
            KeyCode::Down => self.editor_percent = (self.editor_percent + STEP).min(90),
            KeyCode::Left | KeyCode::Right if self.focus != Focus::Editor && self.schema_shown => {
                // From the width drawn, border included (the limits it's
                // drawn within depend on the screen).
                let Some(area) = self.hits.schema else { return false };
                self.schema_width = Some(match code {
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
    pub(super) fn paste_on_query_screen(&mut self, text: &str) {
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
            Some(Overlay::RowForm) => self.paste_row_form(text),
            // Popups without text to type into.
            Some(
                Overlay::Confirm
                | Overlay::Menu
                | Overlay::CellView
                | Overlay::Activity
                | Overlay::Agents
                | Overlay::TxPicker
                | Overlay::Review,
            ) => {}
            Some(Overlay::Completion) | None => self.paste_into_pane(text),
        }
    }

    /// Ctrl+C: copies the editor's selection (or its line), the open cell,
    /// the results row (or the marked rows), or the navigator's name.
    pub(super) fn copy_focused(&mut self) {
        if self.cell_view.is_some() {
            return self.copy_cell();
        }
        let has_row = self.grid_shown() && self.selected_row().is_some();
        match self.focus {
            _ if self.popup_open() => {}
            Focus::Editor => {
                let ed = &self.tab.editor;
                if let Some(text) = ed.selected_text() {
                    return self.copy_text(text, "Copied selection");
                }
                let row = ed.cursor().0;
                let (start, end) = (ed.offset_of((row, 0)), ed.offset_of((row, ed.line_len(row))));
                let line = ed.text()[start..end].to_string();
                if !line.trim().is_empty() {
                    return self.copy_text(line, &format!("Copied line {}", row + 1));
                }
            }
            Focus::Results if has_row => return self.copy_row(),
            Focus::Results if self.tab.result_tab == ResultTab::Messages && !self.messages.is_empty() => {
                return self.copy_message();
            }
            Focus::Schema => {
                if let Some((name, what)) = self.schema_name() {
                    return self.copy_text(name.clone(), &format!("Copied {what} {name}"));
                }
            }
            Focus::Results => {}
        }
        self.set_status("Nothing to copy here · Ctrl+Q quits".to_string(), false);
    }

    /// The navigator's selection as it's written in SQL (a table with its
    /// schema), and what it is.
    fn schema_name(&self) -> Option<(String, &'static str)> {
        let panel = self.schema.as_ref()?;
        let engine = self.engine();
        let quote = |name: &str| engine.quote_ident(name);
        Some(match *panel.nodes().get(panel.selected)? {
            SchemaNode::Table(t) => {
                let info = &panel.tables[t].info;
                (format!("{}.{}", quote(&info.schema), quote(&info.name)), if info.is_view { "view" } else { "table" })
            }
            SchemaNode::Column(t, c) => (quote(&panel.tables[t].columns[c].name), "column"),
            SchemaNode::Index(t, i) => (quote(&panel.tables[t].details.as_ref()?.indexes.get(i)?.name), "index"),
            // Its name with the argument types, as Postgres shows overloads.
            SchemaNode::Routine(r) => (panel.routines.get(r)?.name.clone(), "routine"),
            SchemaNode::Section(_) => return None,
        })
    }

    /// Pastes the system clipboard (Ctrl+V, or the editor's menu).
    pub(super) fn paste_clipboard(&mut self) {
        match crate::clipboard::read() {
            Ok(text) => self.paste_into_pane(&text),
            Err(e) => self.set_status(crate::clipboard::read_failed(&e), true),
        }
    }

    pub(super) fn cut_selection(&mut self) {
        if let Some(text) = self.tab.editor.cut() {
            self.edited();
            self.copy_text(text, "Cut selection");
        }
    }

    fn paste_into_pane(&mut self, text: &str) {
        if let Some(edit) = &mut self.cell_edit {
            return edit.text.insert_str(text);
        }
        match self.focus {
            Focus::Results if self.tab.grid.filter_editing => {
                self.tab.grid.filter.insert_str(text);
                self.tab.grid.refresh_view();
            }
            Focus::Schema => {
                if let Some(panel) = self.schema.as_mut().filter(|p| p.filter_editing) {
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
    fn handle_tab_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c @ '1'..='9') if key.modifiers == KeyModifiers::ALT => {
                self.switch_tab(c as usize - '1' as usize);
                true
            }
            _ => false,
        }
    }

    /// Does what a (configurable) key binding is for.
    pub(super) fn run_action(&mut self, action: Action) {
        let n = self.tabs.len();
        match action {
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
            Action::Heartbeat => self.toggle_heartbeat(),
            Action::TxMode => self.open_tx_picker(),
            Action::Projects => self.open_manager(Screen::Projects),
            Action::Commit => self.end_transaction("COMMIT"),
            Action::Rollback => self.end_transaction("ROLLBACK"),
        }
    }

    /// Shows the focused pane full-screen, or the normal layout again.
    pub(super) fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
        self.completion = None;
        let msg = if self.zoomed {
            format!("Zoomed: Tab moves between panes; {} or Esc returns", self.keymap.label(Action::Zoom))
        } else {
            String::new()
        };
        self.set_status(msg, false);
    }

    pub(super) fn cycle_focus(&mut self, forward: bool) {
        let mut order = vec![Focus::Editor, Focus::Results];
        if self.schema_shown {
            order.push(Focus::Schema);
        }
        let idx = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        self.focus = order[if forward { (idx + 1) % n } else { (idx + n - 1) % n }];
    }

    pub(super) fn handle_editor_key(&mut self, key: KeyEvent) {
        if self.double_space(key) {
            return self.open_palette(false);
        }
        if self.vim_enabled() {
            self.handle_vim_key(key);
        } else {
            self.handle_standard_editor_key(key);
        }
        let space = key.code == KeyCode::Char(' ') && (key.modifiers - KeyModifiers::SHIFT).is_empty();
        if space && self.settings.leader_double_space && self.editor_inserting() {
            self.last_space = Some((Instant::now(), self.tab.editor.cursor()));
        }
    }

    /// With `leader_double_space`: whether this Space came right after
    /// another one, which it then takes back.
    fn double_space(&mut self, key: KeyEvent) -> bool {
        let Some((at, pos)) = self.last_space.take() else { return false };
        let space = key.code == KeyCode::Char(' ') && (key.modifiers - KeyModifiers::SHIFT).is_empty();
        if !space || at.elapsed() > DOUBLE_SPACE || pos != self.tab.editor.cursor() || !self.editor_inserting() {
            return false;
        }
        self.tab.editor.backspace();
        self.edited();
        self.completion = None;
        true
    }

    /// Whether the editor takes typed text now (always, unless vim is in
    /// Normal/Visual mode). Autocomplete only applies then.
    fn editor_inserting(&self) -> bool {
        self.vim_mode().is_none_or(|m| m == VimMode::Insert)
    }

    pub(super) fn handle_standard_editor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.editor_height.saturating_sub(1).max(1);
        let ed = &mut self.tab.editor;

        // Movement; Shift extends the selection. macOS terminals send
        // Option+←/→ as Alt+b / Alt+f.
        let movement: Option<fn(&mut TextEditor)> = match key.code {
            KeyCode::Left if ctrl || alt => Some(TextEditor::move_word_left),
            KeyCode::Right if ctrl || alt => Some(TextEditor::move_word_right),
            KeyCode::Char('b') if alt => Some(TextEditor::move_word_left),
            KeyCode::Char('f') if alt => Some(TextEditor::move_word_right),
            KeyCode::Home if ctrl => Some(TextEditor::move_doc_start),
            KeyCode::End if ctrl => Some(TextEditor::move_doc_end),
            KeyCode::Left => Some(TextEditor::move_left),
            KeyCode::Right => Some(TextEditor::move_right),
            KeyCode::Up => Some(TextEditor::move_up),
            KeyCode::Down => Some(TextEditor::move_down),
            KeyCode::Home => Some(TextEditor::move_home),
            KeyCode::End => Some(TextEditor::move_end),
            _ => None,
        };
        if let Some(movement) = movement {
            ed.prepare_move(shift);
            movement(ed);
            self.completion = None;
            return;
        }
        match key.code {
            KeyCode::PageUp => {
                ed.prepare_move(shift);
                ed.page_up(page);
                return;
            }
            KeyCode::PageDown => {
                ed.prepare_move(shift);
                ed.page_down(page);
                return;
            }
            KeyCode::Char('a') if ctrl => return ed.select_all(),
            KeyCode::Char('x') if ctrl => return self.cut_selection(),
            // Where the terminal doesn't paste by itself.
            KeyCode::Char('v') if ctrl => return self.paste_clipboard(),
            KeyCode::Esc => return ed.clear_selection(),
            _ => {}
        }
        if ctrl || alt {
            return;
        }
        match key.code {
            KeyCode::Char(c) => ed.insert_char(c),
            KeyCode::Enter => ed.insert_newline(),
            KeyCode::Backspace => ed.backspace(),
            KeyCode::Delete => ed.delete(),
            _ => return,
        }
        self.edited();
        // Keep an open popup in sync with the word being typed; a `.` opens
        // it for `table.` / `alias.` / `schema.`.
        let keep_open = self.completion.is_some() && self.word_before_cursor();
        self.completion = if keep_open || key.code == KeyCode::Char('.') { self.compute_completion() } else { None };
    }

    /// Whether Tab in the editor would complete a word rather than move
    /// to the next pane.
    pub fn tab_completes(&self) -> bool {
        self.focus == Focus::Editor && !self.esc_then_tab && self.editor_inserting() && self.word_before_cursor()
    }

    /// Whether the char before the cursor is part of an identifier.
    fn word_before_cursor(&self) -> bool {
        let text = self.tab.editor.text();
        text[..self.tab.editor.cursor_offset()]
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }

    fn compute_completion(&self) -> Option<Completion> {
        let tables: Vec<TableRef> = self
            .schema
            .iter()
            .flat_map(|p| &p.tables)
            .map(|t| TableRef { schema: &t.info.schema, name: &t.info.name, columns: &t.columns })
            .collect();
        let text = self.tab.editor.text();
        let (prefix_len, mut items) =
            complete::complete(&text, self.tab.editor.cursor_offset(), self.engine(), &tables);
        // Snippets by name: saved queries in the `snippets` group.
        let prefix: String = {
            let line = &self.tab.editor.lines[self.tab.editor.cursor_row];
            line.chars()
                .take(self.tab.editor.cursor_col)
                .skip(self.tab.editor.cursor_col.saturating_sub(prefix_len))
                .collect()
        };
        if !prefix.is_empty() {
            let lower = prefix.to_lowercase();
            items.extend(
                self.library
                    .queries
                    .iter()
                    .filter(|q| q.group == crate::library::SNIPPETS && q.name.to_lowercase().starts_with(&lower))
                    .map(|q| complete::Candidate {
                        label: q.name.clone(),
                        insert: q.sql.trim_end().to_string(),
                        kind: complete::CandKind::Snippet,
                        detail: "snippet".into(),
                    }),
            );
        }
        (!items.is_empty()).then_some(Completion { items, selected: 0, prefix_len })
    }

    fn open_completion(&mut self) {
        self.completion = self.compute_completion();
        if self.completion.is_none() {
            let loading = self.schema.as_ref().is_some_and(|s| s.loading);
            let msg = if loading { "No completions yet (schema still loading)" } else { "No completions" };
            self.editor_note = Some(msg.to_string());
            self.editor_note_is_error = false;
        }
    }

    /// Keys for the open popup. Returns false for keys it doesn't handle
    /// (they go to the editor and the popup refilters).
    fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
        let Some(c) = &mut self.completion else { return false };
        if let Some(i) = list::step(c.selected, c.items.len(), key, Nav::COMPLETION) {
            c.selected = i;
            return true;
        }
        match key.code {
            KeyCode::Esc => self.completion = None,
            KeyCode::Tab | KeyCode::Enter => {
                let item = c.items[c.selected].insert.clone();
                let len = c.prefix_len;
                self.completion = None;
                // Already typed in full: Enter is a new line, not a choice.
                let line = &self.tab.editor.lines[self.tab.editor.cursor_row];
                let typed: String = line
                    .chars()
                    .take(self.tab.editor.cursor_col)
                    .skip(self.tab.editor.cursor_col.saturating_sub(len))
                    .collect();
                if key.code == KeyCode::Enter && typed.eq_ignore_ascii_case(&item) {
                    return false;
                }
                self.tab.editor.replace_before_cursor(len, &item);
                self.edited();
            }
            _ => return false,
        }
        true
    }

    pub(super) fn undo(&mut self) {
        self.focus = Focus::Editor;
        if self.tab.editor.undo() {
            self.edited();
        } else {
            self.set_status("Nothing to undo".to_string(), false);
        }
    }

    pub(super) fn redo(&mut self) {
        self.focus = Focus::Editor;
        if self.tab.editor.redo() {
            self.edited();
        } else {
            self.set_status("Nothing to redo".to_string(), false);
        }
    }

    pub(super) fn history_older(&mut self) {
        if let Some(text) = self.history.older(&self.tab.editor.text()) {
            let text = text.to_string();
            self.tab.editor.set_text(&text);
            self.focus = Focus::Editor;
            self.tab.error_marker = None;
            self.tab.syntax_hint = None;
        }
    }

    pub(super) fn history_newer(&mut self) {
        if let Some(text) = self.history.newer() {
            let text = text.to_string();
            self.tab.editor.set_text(&text);
            self.focus = Focus::Editor;
            self.tab.error_marker = None;
            self.tab.syntax_hint = None;
        }
    }

    pub(super) fn handle_results_key(&mut self, key: KeyEvent) {
        let tabs = [ResultTab::Rows, ResultTab::Messages, ResultTab::Plan];
        let current = tabs.iter().position(|t| *t == self.tab.result_tab).unwrap_or(0);
        let switch = match key.code {
            KeyCode::Char('1') => Some(0),
            KeyCode::Char('2') => Some(1),
            KeyCode::Char('3') => Some(2),
            KeyCode::Char(']') => Some((current + 1) % 3),
            KeyCode::Char('[') => Some((current + 2) % 3),
            _ => None,
        };
        if let Some(i) = switch {
            return self.set_result_tab(tabs[i]);
        }
        match key.code {
            KeyCode::Char('m') => return self.open_menu(None),
            // The results kept from earlier runs.
            // (Not over a plan's table, which isn't a run's result.)
            KeyCode::Char(',') if !self.tab.plan_swapped => return self.step_run(false),
            KeyCode::Char('.') if !self.tab.plan_swapped => return self.step_run(true),
            KeyCode::Char('p') if !self.tab.plan_swapped => return self.toggle_pin_run(),
            _ => {}
        }
        if self.tab.result_tab == ResultTab::Messages && self.handle_messages_key(key) {
            return;
        }
        if !self.grid_shown() {
            return self.handle_tab_scroll_key(key);
        }
        // A plan's table can be looked at, not changed.
        if self.tab.plan_swapped && matches!(key.code, KeyCode::Char('c' | 'a' | 'd' | 'f' | 'M' | 'i' | '<' | '>')) {
            return self.set_status("That's for query results, not the plan".to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        let cols = results.columns.len();
        if cols == 0 {
            return;
        }
        match key.code {
            KeyCode::Char('/') => return self.tab.grid.filter_editing = true,
            KeyCode::Esc if !self.tab.grid.filter.is_empty() => {
                self.tab.grid.filter.clear();
                return self.tab.grid.refresh_view();
            }
            KeyCode::Char('s') => return self.cycle_sort(),
            KeyCode::Char('b') => return self.chart_column(),
            KeyCode::Char('e') => return self.export(ExportFormat::Csv),
            KeyCode::Char('J') => return self.export(ExportFormat::Json),
            KeyCode::Char('i') => return self.export(ExportFormat::Sql),
            KeyCode::Char('M') => return self.load_more(),
            KeyCode::Char('c') => return self.start_cell_edit(),
            KeyCode::Char('a') => return self.start_row_insert(),
            KeyCode::Char('d') => return self.delete_rows(),
            KeyCode::Char('C') => return self.copy_markdown(),
            KeyCode::Char('z') => return self.toggle_zoom(),
            KeyCode::Char('>') => return self.step_result_set(true),
            KeyCode::Char('<') => return self.step_result_set(false),
            KeyCode::Char('X') => return self.toggle_vertical(),
            _ => {}
        }
        let len = self.tab.grid.view.len();
        if len == 0 {
            return;
        }
        if self.tab.grid.vertical && self.vertical_move(key.code, len, cols) {
            return self.maybe_fetch_more();
        }
        let current = self.tab.grid.table_state.selected().unwrap_or(0);
        let row = match key.code {
            KeyCode::Char('g') => 0,
            KeyCode::Char('G') => len - 1,
            _ => list::step(current, len, key, Nav::LIST).unwrap_or(current),
        };
        self.tab.grid.table_state.select(Some(row));
        self.maybe_fetch_more();
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.tab.grid.selected_col = self.tab.grid.selected_col.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.tab.grid.selected_col = (self.tab.grid.selected_col + 1).min(cols - 1);
            }
            KeyCode::Char('0' | '^') => self.tab.grid.selected_col = 0,
            KeyCode::Char('$') => self.tab.grid.selected_col = cols - 1,
            KeyCode::Char('y') => self.copy_cell(),
            KeyCode::Char('Y') => self.copy_row(),
            KeyCode::Enter | KeyCode::Char('v') => self.open_cell_view(),
            KeyCode::Char('x') => self.open_record_view(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('f') => self.follow_foreign_key(),
            KeyCode::Char('|') => self.open_pipe_prompt(),
            KeyCode::Char('o') => self.open_in_viewer(),
            _ => {}
        }
    }

    /// `X`: the rows one column per line (like mysql's `\G`), or the table.
    pub(super) fn toggle_vertical(&mut self) {
        let grid = &mut self.tab.grid;
        grid.vertical = !grid.vertical;
        grid.vertical_by_g = false;
        let msg = if grid.vertical { "One column per line (X: back to the table)" } else { "Table view" };
        self.set_status(msg.to_string(), false);
    }

    /// Moving in the vertical layout: ↑/↓ go field by field, on into the
    /// next or previous row; PageUp/PageDown a whole row. True if handled.
    fn vertical_move(&mut self, code: KeyCode, len: usize, cols: usize) -> bool {
        let grid = &mut self.tab.grid;
        let row = grid.table_state.selected().unwrap_or(0);
        let col = grid.selected_col;
        let (row, col) = match code {
            KeyCode::Down | KeyCode::Char('j') if col + 1 < cols => (row, col + 1),
            KeyCode::Down | KeyCode::Char('j') if row + 1 < len => (row + 1, 0),
            KeyCode::Up | KeyCode::Char('k') if col > 0 => (row, col - 1),
            KeyCode::Up | KeyCode::Char('k') if row > 0 => (row - 1, cols - 1),
            KeyCode::Down | KeyCode::Up | KeyCode::Char('j' | 'k') => (row, col),
            KeyCode::PageDown => ((row + 1).min(len - 1), col),
            KeyCode::PageUp => (row.saturating_sub(1), col),
            _ => return false,
        };
        grid.table_state.select(Some(row));
        grid.selected_col = col;
        true
    }

    /// The selected row as shown (after filtering/sorting).
    pub(super) fn selected_row(&self) -> Option<&Vec<Option<String>>> {
        let results = self.tab.grid.results.as_ref()?;
        let idx = *self.tab.grid.view.get(self.tab.grid.table_state.selected()?)?;
        results.rows.get(idx)
    }

    pub(super) fn handle_filter_key(&mut self, key: KeyEvent) {
        match self.tab.grid.filter.handle(key) {
            InputResult::Submit => self.tab.grid.filter_editing = false,
            InputResult::Cancel => {
                self.tab.grid.filter_editing = false;
                self.tab.grid.filter.clear();
            }
            InputResult::Changed => {}
            InputResult::Moved | InputResult::Ignored => return,
        }
        self.tab.grid.refresh_view();
    }

    /// Ascending → descending → unsorted, on the selected column.
    pub(super) fn cycle_sort(&mut self) {
        let col = self.tab.grid.selected_col;
        self.tab.grid.sort = match self.tab.grid.sort {
            Some((c, false)) if c == col => Some((col, true)),
            Some((c, true)) if c == col => None,
            _ => Some((col, false)),
        };
        self.tab.grid.refresh_view();
    }

    pub(super) fn open_cell_view(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let Some(row) = self.selected_row() else { return };
        let Some(cell) = row.get(self.tab.grid.selected_col) else { return };
        let title = results.columns[self.tab.grid.selected_col].clone();
        let text = cell.as_deref().map_or_else(|| NULL_TEXT.to_string(), pretty_value);
        self.cell_view = Some(CellView::new(title, text));
    }

    /// `x`: the selected row as one `column  value` line per column.
    pub(super) fn open_record_view(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let Some(row) = self.selected_row() else { return };
        let name_w = results.columns.iter().map(|c| c.chars().count()).max().unwrap_or(0).min(30);
        let mut text = String::new();
        for (name, cell) in results.columns.iter().zip(row) {
            let value = cell.as_deref().map_or_else(|| NULL_TEXT.to_string(), pretty_value);
            // Continuation lines of multi-line values line up with the first.
            let value = value.replace('\n', &format!("\n{:w$}  ", "", w = name_w));
            let _ = writeln!(text, "{name:<name_w$}  {value}");
        }
        let at = self.tab.grid.table_state.selected().unwrap_or(0);
        let title = format!("ROW {} of {}", at + 1, self.tab.grid.view.len());
        self.cell_view = Some(CellView { record: true, ..CellView::new(title, text) });
    }

    /// The previous or next row in the row view (`x`), from its top.
    fn step_record(&mut self, back: bool) {
        let at = self.tab.grid.table_state.selected().unwrap_or(0);
        let next = if back { at.saturating_sub(1) } else { (at + 1).min(self.tab.grid.view.len().saturating_sub(1)) };
        if next == at {
            return;
        }
        self.tab.grid.table_state.select(Some(next));
        self.maybe_fetch_more();
        self.open_record_view();
    }

    pub(super) fn handle_cell_view_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.cell_view else { return };
        let page = self.editor_height.max(5) as u16;
        // A row (`x`): past its end ↓ goes on to the next row, and ↑ above
        // its start to the previous one.
        let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
        let up = matches!(key.code, KeyCode::Up | KeyCode::Char('k'));
        if view.record && ((down && view.scroll >= view.max_scroll) || (up && view.scroll == 0)) {
            return self.step_record(up);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'v') => self.cell_view = None,
            KeyCode::Down | KeyCode::Char('j') => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(page),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(page),
            KeyCode::Home => view.scroll = 0,
            KeyCode::Char('y') if view.sql.is_some() => {
                let text = view.sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
                self.copy_text(text, "Copied the SQL");
            }
            KeyCode::Char('e') if view.sql.is_some() => {
                let Some((sql, conn)) = view.sql.take() else { return };
                self.cell_view = None;
                self.open_tab_on(conn, &sql);
            }
            KeyCode::Char('y') if view.browse => {
                let text = view.text.clone();
                self.copy_text(text, "Copied the definition");
            }
            KeyCode::Char('y') if view.record => {
                let text = view.text.clone();
                self.copy_text(text, "Copied the row");
            }
            KeyCode::Char('y') if view.copies_text.is_some() => {
                let (text, said) = (view.text.clone(), view.copies_text.unwrap_or_default());
                self.copy_text(text, said);
            }
            KeyCode::Char('y') => self.copy_cell(),
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') if view.browse => {
                self.step_definition(matches!(key.code, KeyCode::Left | KeyCode::Char('h')));
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') if view.record => {
                self.step_record(matches!(key.code, KeyCode::Left | KeyCode::Char('h')));
            }
            KeyCode::Char('x') if view.record => self.cell_view = None,
            _ => {}
        }
    }

    pub(super) fn copy_cell(&mut self) {
        let Some(row) = self.selected_row() else { return };
        let Some(cell) = row.get(self.tab.grid.selected_col) else { return };
        let text = cell.clone().unwrap_or_default();
        let status = format!("Copied cell ({} chars)", text.chars().count());
        self.copy_text(text, &status);
    }

    /// The marked rows, or else the current one, a line each (tab-separated).
    pub(super) fn copy_row(&mut self) {
        let line = |cells: &[Option<String>]| {
            cells.iter().map(|c| c.as_deref().unwrap_or("").replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")
        };
        let marked = &self.tab.grid.marked;
        if marked.is_empty() {
            let Some(cells) = self.selected_row() else { return };
            let text = line(cells);
            return self.copy_text(text, "Copied row (tab-separated)");
        }
        let Some(results) = self.tab.grid.results.as_ref() else { return };
        let lines: Vec<_> = marked.iter().filter_map(|&i| results.rows.get(i)).map(|r| line(r)).collect();
        let status = format!("Copied {} rows (tab-separated, a line each)", lines.len());
        self.copy_text(lines.join("\n"), &status);
    }

    /// `podelis-export-<timestamp>.<ext>` in `export_dir` (or the current folder).
    fn export_path(&self, format: ExportFormat) -> PathBuf {
        let ext = match format {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
        };
        let dir = match self.settings.export_dir.as_deref() {
            Some(d) if d.starts_with("~/") => crate::settings::expand_home(d),
            Some(d) => PathBuf::from(d),
            None => PathBuf::new(),
        };
        dir.join(format!("podelis-export-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S")))
    }

    /// The table `INSERT` exports go into: the query's, when it reads just one.
    pub(super) fn insert_table(&self) -> String {
        let engine = self.engine();
        self.tab.last_sql.as_ref().and_then(|(sql, _)| analyze::edit_source(&parse(sql, engine))).map_or_else(
            || "result".to_string(),
            |s| match s.schema {
                Some(schema) => format!("{}.{}", engine.quote_ident(&schema), engine.quote_ident(&s.table)),
                None => engine.quote_ident(&s.table),
            },
        )
    }

    /// Writes the rows as shown to a CSV or JSON file in the current folder.
    pub(super) fn export(&mut self, format: ExportFormat) {
        let path = self.export_path(format);
        let table = self.insert_table();
        let Some(results) = &self.tab.grid.results else { return };
        let truncated = results.truncated() || self.row_stream.is_some();
        let rows = self.tab.grid.view.len();
        let engine = self.engine();
        let text = match format {
            ExportFormat::Csv => crate::export::csv(results, &self.tab.grid.view),
            ExportFormat::Json => crate::export::json(results, &self.tab.grid.view),
            ExportFormat::Sql => crate::export::sql_inserts(results, &self.tab.grid.view, &table, engine),
        };
        match std::fs::write(&path, text) {
            Ok(()) => {
                let mut note = format!(" ({rows} rows");
                if truncated && self.row_stream.is_some() {
                    note.push_str(" loaded; Ctrl+K “export all” writes every row");
                } else if truncated {
                    let _ = write!(note, " of {}; M loads more", results.total_rows);
                } else if !self.tab.grid.filter.is_empty() {
                    note.push_str(", filtered");
                }
                note.push(')');
                let shown = std::fs::canonicalize(&path).unwrap_or(path);
                self.set_status(format!("Exported to {}{note}", shown.display()), false);
            }
            Err(e) => self.set_status(format!("Export failed: {e}"), true),
        }
    }

    /// `b`: a bar chart of the selected column, for the rows shown.
    pub(super) fn chart_column(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let col = self.tab.grid.selected_col;
        match crate::chart::chart(results, &self.tab.grid.view, col) {
            Ok(text) => {
                let title = format!("CHART · {}", results.columns.get(col).map_or("", String::as_str));
                self.cell_view = Some(CellView::new(title, text));
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Writes every row of the result to a file: those loaded, then the
    /// rest of the stream page by page, straight to the file rather than
    /// into the grid. Esc stops it.
    pub(super) fn export_all(&mut self, format: ExportFormat) {
        const PAGE: usize = 10_000;
        if self.export_task.is_some() {
            return self.set_status("An export is already running (Esc stops it)".to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if self.row_stream.is_none() {
            if results.truncated() {
                return self.set_status(
                    "Only a statement run on its own (F6) can be exported in full; this result was cut at the row limit"
                        .to_string(),
                    true,
                );
            }
            return self.export(format);
        }
        if self.fetching_more {
            return self.set_status("Rows are still loading; try again in a moment".to_string(), true);
        }
        let Some(stream) = self.row_stream.take() else { return };
        let path = self.export_path(format);
        let paged = match format {
            ExportFormat::Csv => crate::export::Paged::Csv,
            ExportFormat::Json => crate::export::Paged::Json,
            ExportFormat::Sql => crate::export::Paged::Inserts { table: self.insert_table(), engine: self.engine() },
        };
        let (columns, types, loaded) = (results.columns.clone(), results.column_types.clone(), results.rows.clone());
        let send = self.sender();
        let target = path.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let written = async {
                let file = std::io::BufWriter::new(std::fs::File::create(&target)?);
                let mut out = crate::export::PagedWriter::new(file, paged, &columns, &types);
                out.page(loaded)?;
                loop {
                    let page = tokio::select! {
                        page = stream.next_page(PAGE) => page.map_err(|e| anyhow::anyhow!(e))?,
                        _ = &mut stopped => anyhow::bail!("stopped"),
                    };
                    let done = page.done;
                    out.page(page.rows)?;
                    send(DbEventKind::ExportProgress(out.rows));
                    if done {
                        break;
                    }
                }
                let rows = out.rows;
                out.finish()?;
                anyhow::Ok(rows)
            }
            .await;
            // The stream goes with this task, which cancels the query; an
            // unfinished file isn't left behind.
            drop(stream);
            if written.is_err() {
                let _ = std::fs::remove_file(&target);
            }
            send(DbEventKind::ExportDone(target, written.map_err(|e| format!("{e:#}"))));
        });
        self.export_task = Some((stop, path));
        self.set_status("Exporting every row… (Esc stops it)".to_string(), false);
    }

    /// Stops a running export; it removes its unfinished file.
    pub(super) fn stop_export(&mut self) {
        let Some((stop, _)) = self.export_task.take() else { return };
        let _ = stop.send(());
        self.set_status("Export stopped; the unfinished file is removed".to_string(), false);
    }

    pub(super) fn toggle_schema(&mut self) {
        self.schema_visible = !self.schema_visible;
        if self.schema_visible {
            self.focus = Focus::Schema;
            if self.schema.is_none() {
                self.load_catalog();
            }
        } else if self.focus == Focus::Schema {
            self.focus = Focus::Editor;
        }
    }

    /// (Re)loads the catalog in the background, keeping the current one
    /// visible meanwhile.
    pub(super) fn load_catalog(&mut self) {
        let Some(db) = self.database.clone() else { return };
        self.schema.get_or_insert_with(SchemaPanel::default).loading = true;
        let send = self.sender();
        let system = self.settings.system_tables;
        tokio::spawn(async move {
            // A `SET sql_mode` may have changed what lint rules should see.
            db.refresh_sql_mode().await;
            send(DbEventKind::Catalog(db.catalog(system).await.map_err(|e| crate::db::error_message(&e))));
            // Listing routines may need privileges the user lacks: then
            // the section just stays empty.
            send(DbEventKind::Routines(db.routines().await.unwrap_or_default()));
        });
    }

    /// Shows or hides the database's own catalog tables, for this session.
    pub(super) fn toggle_system_tables(&mut self) {
        self.settings.system_tables = !self.settings.system_tables;
        let shown = if self.settings.system_tables { "Showing" } else { "Hiding" };
        self.set_status(format!("{shown} system tables"), false);
        self.load_catalog();
    }

    #[allow(clippy::too_many_lines)] // one arm per key
    pub(super) fn handle_schema_key(&mut self, key: KeyEvent) {
        let engine = self.engine();
        let Some(panel) = &mut self.schema else { return };
        if panel.filter_editing {
            match panel.filter.handle(key) {
                InputResult::Submit => panel.filter_editing = false,
                InputResult::Cancel => {
                    panel.filter_editing = false;
                    panel.filter.clear();
                }
                InputResult::Changed => {}
                InputResult::Moved | InputResult::Ignored => return,
            }
            panel.selected = step_selectable(&panel.nodes(), 0, 1);
            return;
        }
        match key.code {
            KeyCode::Char('m') => return self.open_menu(None),
            KeyCode::Char(' ') => return self.open_palette(false),
            KeyCode::Char('z') => return self.toggle_zoom(),
            KeyCode::Char('r') => return self.load_catalog(),
            KeyCode::Char('.') => return self.toggle_system_tables(),
            KeyCode::Char('/') => return panel.filter_editing = true,
            KeyCode::Char('o') => return self.cycle_schema_sort(),
            KeyCode::Char('y') => return self.copy_focused(),
            KeyCode::Esc if !panel.filter.is_empty() => {
                panel.filter.clear();
                panel.selected = step_selectable(&panel.nodes(), 0, 1);
                return;
            }
            _ => {}
        }
        let nodes = panel.nodes();
        let Some(&node) = nodes.get(panel.selected) else { return };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => panel.selected = step_selectable(&nodes, panel.selected + 1, 1),
            KeyCode::PageDown => panel.selected = step_selectable(&nodes, panel.selected + panel.page.max(1), -1),
            KeyCode::PageUp => {
                panel.selected = step_selectable(&nodes, panel.selected.saturating_sub(panel.page.max(1)), 1);
            }
            KeyCode::Home | KeyCode::Char('g') => panel.selected = step_selectable(&nodes, 0, 1),
            KeyCode::End | KeyCode::Char('G') => panel.selected = step_selectable(&nodes, nodes.len(), -1),
            KeyCode::Up | KeyCode::Char('k') => {
                panel.selected = step_selectable(&nodes, panel.selected.saturating_sub(1), -1);
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match node {
                SchemaNode::Table(t) => {
                    let table = &mut panel.tables[t];
                    table.expanded = !table.expanded || key.code != KeyCode::Enter;
                    if table.expanded {
                        self.request_details(t);
                    }
                }
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {}
            },
            KeyCode::Char('f') => {
                if let Some(t) = node.table() {
                    self.show_related(t);
                }
            }
            KeyCode::Char('d') => match node {
                SchemaNode::Routine(r) => self.show_routine(r),
                _ => {
                    if let Some(t) = node.table() {
                        self.show_table_definition(t);
                    }
                }
            },
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(t) = node.table() {
                    panel.tables[t].expanded = false;
                    panel.selected = panel.nodes().iter().position(|n| *n == SchemaNode::Table(t)).unwrap_or(0);
                }
            }
            KeyCode::Char('s') => {
                let Some(t) = node.table() else { return };
                let info = &panel.tables[t].info;
                let query = format!(
                    "SELECT * FROM {}.{} LIMIT 100;",
                    engine.quote_ident(&info.schema),
                    engine.quote_ident(&info.name)
                );
                // After everything, a blank line apart: never inside another
                // statement. Then it runs, alone (as F6 would).
                self.append_statement(&query);
                if self.database.is_some() {
                    self.execute_current();
                }
            }
            _ => {}
        }
    }

    /// `o` in the navigator: orders the tables by name, rows or size,
    /// keeping the selected one selected.
    fn cycle_schema_sort(&mut self) {
        let sizes = self.engine() != Engine::Sqlite;
        let Some(panel) = &mut self.schema else { return };
        let before = panel.nodes().get(panel.selected).copied();
        panel.sort = panel.sort.next();
        // SQLite keeps no sizes: there's nothing to order by.
        if panel.sort == SchemaSort::Size && !sizes {
            panel.sort = panel.sort.next();
        }
        let nodes = panel.nodes();
        panel.selected = before.and_then(|n| nodes.iter().position(|m| *m == n)).unwrap_or(0);
        panel.selected = step_selectable(&nodes, panel.selected, 1);
        panel.scroll = 0;
        let order = panel.sort.label();
        self.set_status(format!("Navigator ordered by {order}"), false);
    }

    /// Runs the selection if there is one, otherwise the whole editor.
    pub(super) fn execute_all(&mut self) {
        let text = self.tab.editor.text();
        match self.tab.editor.selection_offsets() {
            Some((a, b)) => self.execute(text[a..b].to_string(), a),
            None => self.execute(text, 0),
        }
    }

    /// Runs the selection if there is one, otherwise the statement at the cursor.
    pub(super) fn execute_current(&mut self) {
        if self.tab.editor.selection().is_some() {
            return self.execute_all();
        }
        let text = self.tab.editor.text();
        match sql::statement_at(&text, self.tab.editor.cursor_offset(), self.engine()) {
            Some(stmt) => {
                let offset = stmt.as_ptr() as usize - text.as_ptr() as usize;
                self.execute(stmt.to_string(), offset);
            }
            None => self.set_status("No statement at cursor".to_string(), true),
        }
    }

    /// Checks what `sql` would do against the connection's mode, asking for
    /// confirmation where needed. `offset`: where `sql` starts in the editor.
    pub(super) fn execute(&mut self, sql: String, offset: usize) {
        self.execute_with(sql, offset, RunPurpose::Query);
    }

    pub(super) fn execute_with(&mut self, sql: String, offset: usize, purpose: RunPurpose) {
        if self.executing {
            return self.set_status(self.running_elsewhere("wait for it, or Esc stops it"), true);
        }
        if self.database.is_none() {
            return self.set_status(self.not_connected_message(), true);
        }
        if sql.trim().is_empty() {
            self.set_status("Nothing to run".to_string(), true);
            return;
        }
        if matches!(purpose, RunPurpose::Query) && self.ask_for_params(&sql, offset, false) {
            return;
        }
        self.check_and_run(sql, offset, purpose, None);
    }

    pub(super) fn not_connected_message(&self) -> String {
        if self.conns.restoring.is_empty() {
            "This tab isn't connected: Ctrl+O picks a connection".to_string()
        } else {
            format!("Still reconnecting to {}…", self.conns.restoring.join(", "))
        }
    }

    /// Runs `sql` unless the connection's mode forbids it or it needs
    /// confirming first. `filled`: the parameters substituted into it.
    pub(super) fn check_and_run(&mut self, sql: String, offset: usize, purpose: RunPurpose, filled: Option<Filled>) {
        let infos = analyze::analyze(&parse(&sql, self.engine()));
        let writes: Vec<&StatementInfo> = infos.iter().filter(|i| i.modifies()).collect();
        if self.active_mode == ConnectionMode::ReadOnly
            && let Some(first) = writes.first()
        {
            let msg = format!("Read-only connection: not running “{}” (it changes data)", first.summary);
            return self.set_status(msg, true);
        }
        if let Some(msg) = self.tx_mode_refusal(&infos, &purpose) {
            return self.set_status(msg, true);
        }
        let risky: Vec<String> = infos.iter().filter_map(|i| i.risk.map(|r| format!("{r}: {}", i.summary))).collect();
        let production = self.active_mode == ConnectionMode::Production && !writes.is_empty();
        if production || !risky.is_empty() {
            let message = if production {
                format!("PRODUCTION: {} statement(s) will change data. Run?", writes.len())
            } else {
                "This looks destructive. Run anyway?".to_string()
            };
            let mut details = risky;
            if production {
                details.extend(writes.iter().filter(|w| w.risk.is_none()).map(|w| w.summary.clone()));
            }
            details.truncate(6);
            // Writes can instead run in a transaction, to review first.
            let offer_transaction = !writes.is_empty()
                && self.tx_mode == TxMode::AutoCommit
                && self.txn == TxnState::Idle
                && matches!(purpose, RunPurpose::Query | RunPurpose::RowChange(_));
            let action = ConfirmAction::Run { sql, offset, infos, purpose, filled };
            self.confirm = Some(Confirm { message, details, danger: true, action, offer_transaction });
            return;
        }
        // A read on production that would read a whole big table asks
        // first: planned (not run) in the background, then run or asked.
        let guarded = self.active_mode == ConnectionMode::Production
            && self.settings.guard_rows > 0
            && matches!(purpose, RunPurpose::Query)
            && !infos.is_empty()
            && infos.iter().all(|i| i.kind == Kind::Read);
        if guarded && let Some(db) = self.database.clone() {
            self.set_result_status("Checking the plan before running on production…".to_string(), false);
            // Running, as far as the rest of the app goes: nothing else
            // starts, and Esc stops it.
            self.executing = true;
            self.running_since = Some(Instant::now());
            self.run_owner = None;
            let send = self.sender();
            tokio::spawn(async move {
                let scans = db.full_scans(&sql).await.unwrap_or_default();
                send(DbEventKind::Guarded { sql, offset, infos, filled, scans });
            });
            return;
        }
        self.run_writes(sql, offset, infos, purpose, filled);
    }

    /// After the plan of a read on production: runs it, or asks first when
    /// it would read a whole table of at least `guard_rows` rows.
    pub(super) fn guarded(
        &mut self,
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        filled: Option<Filled>,
        scans: &[(String, f64)],
    ) {
        self.executing = false;
        if std::mem::take(&mut self.cancelling) {
            return self.set_status("Cancelled".to_string(), false);
        }
        #[allow(clippy::cast_precision_loss)] // a threshold
        let limit = self.settings.guard_rows as f64;
        let heavy: Vec<String> = scans
            .iter()
            .filter(|(_, rows)| *rows >= limit)
            .map(|(table, rows)| format!("{table}: about {} rows, read in full", crate::db::human_rows(*rows)))
            .collect();
        if heavy.is_empty() {
            return self.run_writes(sql, offset, infos, RunPurpose::Query, filled);
        }
        self.set_status(String::new(), false);
        self.confirm = Some(Confirm {
            message: "PRODUCTION: this reads a whole big table. Run anyway?".to_string(),
            details: heavy,
            danger: true,
            action: ConfirmAction::RunHeavy { sql, offset, infos, filled },
            offer_transaction: false,
        });
    }

    /// Re-runs the last query with a larger row limit (read-only queries only,
    /// since it executes them again).
    pub(super) fn load_more(&mut self) {
        if self.executing {
            return self.set_status(self.running_elsewhere("load more once it's done"), true);
        }
        if self.row_stream.is_some() {
            return self.fetch_more(self.settings.row_limit * 10);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if !results.truncated() && !self.tab.grid.cut_short {
            return self.set_status("All rows are already loaded".to_string(), false);
        }
        if !self.tab.last_query_infos.iter().all(|i| i.kind == Kind::Read) {
            return self.set_status("Only read-only queries can be re-run to load more rows".to_string(), true);
        }
        let Some((sql, offset)) = self.tab.last_sql.clone() else { return };
        self.next_row_limit = Some((results.rows.len() * 10).min(MAX_ROW_LIMIT));
        let infos = self.tab.last_query_infos.clone();
        self.rerun_shown(sql, offset, infos);
    }

    /// Shows the plan for the selection or the statement at the cursor,
    /// without running it.
    /// While a query runs, it's made as soon as that finishes.
    pub(super) fn explain_current(&mut self) {
        if self.database.is_none() {
            return;
        }
        let Some((stmt, line)) = self.statement_to_explain() else {
            return self.set_status("No statement at cursor".to_string(), true);
        };
        if stmt.to_ascii_uppercase().starts_with("EXPLAIN") {
            return self.set_status("That's already an EXPLAIN; run it with F6".to_string(), true);
        }
        if self.executing {
            return self.explain_aside(stmt, line);
        }
        self.explain_statement(stmt, line);
    }

    /// F7 while a query runs: made at once on a connection of its own where
    /// that sees the same as the session, else after the query.
    fn explain_aside(&mut self, stmt: String, line: usize) {
        let version = self.database.as_ref().map_or("", |db| db.version());
        let explain = self.engine().dialect().explain(&stmt, version);
        // (SQLite never: see `run_aside`.)
        let aside = self.engine() != Engine::Sqlite
            && self.txn == TxnState::Idle
            && crate::params::find(&explain, self.engine()).is_empty();
        match self.database.clone().filter(|_| aside) {
            Some(db) => {
                let send = self.sender();
                let limit = self.settings.row_limit;
                let tab = self.active_tab;
                tokio::spawn(async move {
                    let plan = db.run_aside(&explain, limit).await.ok().flatten();
                    send(DbEventKind::PlanAside { tab, stmt, line, plan });
                });
            }
            None => self.queue_plan(stmt, line),
        }
    }

    /// Only in the tab whose query runs: it's made there after it.
    fn queue_plan(&mut self, stmt: String, line: usize) {
        if !self.running_here() {
            return self.set_status(self.running_elsewhere("F7 again once it's done"), true);
        }
        self.queued_plan = Some((stmt, line));
        self.set_status("The plan follows when the running query finishes (Esc stops both)".into(), false);
    }

    /// The plan made beside the running query, or (where it couldn't be)
    /// made after it.
    pub(super) fn plan_aside(&mut self, stmt: String, line: usize, plan: Option<QueryOutcome>) {
        match plan {
            Some(outcome) => {
                self.plan_request = Some((stmt, false, line));
                self.show_plan(outcome);
            }
            None if self.executing => self.queue_plan(stmt, line),
            None => self.explain_statement(stmt, line),
        }
    }

    /// "A query is running", naming its tab when that isn't this one.
    pub(super) fn running_elsewhere(&self, then: &str) -> String {
        match self.run_owner.filter(|&t| t != self.active_tab).and_then(|t| self.tabs.get(t)) {
            Some(tab) => format!("Tab “{}” is running a query: {then}", tab.name),
            None => format!("A query is running: {then}"),
        }
    }

    /// Settings changed by the last run (`USE`, `SET`…) are the session's
    /// alone: from then on plans aren't made on another connection.
    pub(super) fn note_session_changes(&mut self) {
        if self.last_run_infos.iter().any(|i| i.kind == Kind::Session)
            && let Some(db) = &self.database
        {
            db.mark_session_changed();
        }
    }

    /// The plan queued while a query ran, now that it's done.
    /// Not while something still runs (the plan waits) or asks the user
    /// something (a review, a confirmation: the plan is dropped).
    pub(super) fn run_queued_plan(&mut self) {
        if self.executing {
            return;
        }
        let Some((stmt, line)) = self.queued_plan.take() else { return };
        if !self.popup_open() {
            self.explain_statement(stmt, line);
        }
    }

    /// After a failed run, the queued plan isn't made: the error stays in view.
    pub(super) fn drop_queued_plan(&mut self) {
        if self.queued_plan.take().is_some() {
            self.status.push_str(" · the plan wasn't made");
        }
    }

    fn explain_statement(&mut self, stmt: String, line: usize) {
        let version = self.database.as_ref().map_or("", |db| db.version());
        let explain = self.engine().dialect().explain(&stmt, version);
        self.plan_request = Some((stmt, false, line));
        if self.ask_for_params(&explain, 0, true) {
            return;
        }
        let infos = analyze::analyze(&parse(&explain, self.engine()));
        self.run_now(explain, 0, infos, RunPurpose::Explain, None);
    }

    /// Shows `tab` in the results pane: the QUERY PLAN's table, if it has
    /// one, takes the grid's place while it's shown.
    pub(super) fn set_result_tab(&mut self, tab: ResultTab) {
        // A filter being typed stays with its grid.
        self.tab.grid.filter_editing = false;
        self.tab.result_tab = tab;
        self.tab.tab_scroll = 0;
        self.tab.tab_hscroll = 0;
        self.sync_plan_grid();
    }

    /// Swaps the plan's grid in or out to match the tab shown.
    pub(super) fn sync_plan_grid(&mut self) {
        let want = self.tab.result_tab == ResultTab::Plan && self.tab.plan_grid.is_some();
        if want != self.tab.plan_swapped
            && let Some(other) = &mut self.tab.plan_grid
        {
            std::mem::swap(&mut self.tab.grid, other);
            self.tab.plan_swapped = want;
            self.cell_edit = None;
        }
    }

    /// The result's own grid back in place (before a run's outcome lands).
    pub(super) fn unswap_plan_grid(&mut self) {
        if self.tab.plan_swapped
            && let Some(other) = &mut self.tab.plan_grid
        {
            std::mem::swap(&mut self.tab.grid, other);
            self.tab.plan_swapped = false;
        }
    }

    /// Whether the pane shows a grid of rows: the result, or a plan's table.
    pub(super) fn grid_shown(&self) -> bool {
        self.tab.result_tab == ResultTab::Rows || self.tab.plan_swapped
    }

    /// Shows the statement the plan is for in the editor, selected and
    /// scrolled to from its first line, if it's still there.
    pub(super) fn go_to_plan_statement(&mut self) {
        let Some(source) = self.tab.plan_source.clone() else { return };
        self.go_to_source(&source, "The plan's statement", "F7 explains it again");
    }

    /// Selects `source`'s statement in the editor and scrolls to it, if
    /// it's still there; `what` names it, `again` says how to redo it.
    pub(super) fn go_to_source(&mut self, source: &PlanSource, what: &str, again: &str) {
        let text = self.tab.editor.text();
        let Some((offset, line)) = source.find_in(&text) else {
            return self.set_status(format!("{what} isn't in the editor any more"), true);
        };
        let ed = &mut self.tab.editor;
        let start = ed.pos_of_offset(offset);
        let end = ed.pos_of_offset(offset + source.sql.len());
        // The cursor at its start (so the view starts there), the rest selected.
        ed.set_cursor(start);
        ed.set_anchor(Some(end));
        ed.scroll_row = start.0.saturating_sub(1);
        self.completion = None;
        // Zoomed, the editor is what's shown now.
        self.focus = Focus::Editor;
        let last = line + source.sql.matches('\n').count();
        let lines =
            if last > line { format!("lines {}–{}", line + 1, last + 1) } else { format!("line {}", line + 1) };
        self.set_status(format!("{what}, {lines} ({again})"), false);
    }

    /// The selection, or the statement at the cursor, to EXPLAIN, and the
    /// editor line it starts on (from 0).
    pub(super) fn statement_to_explain(&self) -> Option<(String, usize)> {
        let text = self.tab.editor.text();
        let (start, stmt) = if let Some((a, b)) = self.tab.editor.selection_offsets() {
            let selected = &text[a..b];
            let lead = selected.len() - selected.trim_start().len();
            (a + lead, selected.trim().trim_end_matches(';').trim())
        } else {
            let s = sql::statement_at(&text, self.tab.editor.cursor_offset(), self.engine())?;
            (s.as_ptr() as usize - text.as_ptr() as usize, s)
        };
        Some((stmt.to_string(), text[..start].matches('\n').count()))
    }

    pub(super) fn show_plan(&mut self, outcome: QueryOutcome) {
        let plan = self
            .engine()
            .dialect()
            .format_plan(&outcome.columns, &outcome.rows)
            .unwrap_or_else(|| crate::db::Plan { text: "The server returned no plan.".to_string(), notes: Vec::new() });
        self.tab.plan = Some(plan);
        self.tab.plan_source =
            self.plan_request.take().map(|(sql, analyzed, line)| PlanSource::new(sql, analyzed, line));
        // A plan with columns (MySQL's classic EXPLAIN) is a grid too.
        self.unswap_plan_grid();
        self.tab.plan_grid = (outcome.columns.len() > 1 && self.engine() != Engine::Sqlite).then(|| {
            let mut grid = Box::<ResultGrid>::default();
            grid.show(outcome);
            grid.table_state.select(Some(0));
            grid
        });
        self.set_result_tab(ResultTab::Plan);
        // Ready to scroll the plan; `1` goes back to the result.
        self.focus = Focus::Results;
        self.set_status("Query plan shown (the statement was not executed)".to_string(), false);
    }

    /// Keys for the MESSAGES and QUERY PLAN tabs: scrolling. The plan
    /// scrolls down from its top; messages scroll up from the newest line.
    /// ←/→ scroll across (drawing stops at the widest line).
    fn handle_tab_scroll_key(&mut self, key: KeyEvent) {
        let page = self.editor_height.max(5) as u16;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => return self.scroll_tab_across(-1),
            KeyCode::Right | KeyCode::Char('l') => return self.scroll_tab_across(1),
            KeyCode::Char('0') => {
                self.tab.tab_hscroll = 0;
                return;
            }
            _ => {}
        }
        let delta: i32 = match key.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::PageDown => i32::from(page),
            KeyCode::PageUp => -i32::from(page),
            KeyCode::Home => {
                self.tab.tab_scroll = 0;
                self.tab.tab_hscroll = 0;
                return;
            }
            _ => return,
        };
        let delta = if self.tab.result_tab == ResultTab::Messages { -delta } else { delta };
        self.tab.tab_scroll = self.tab.tab_scroll.saturating_add_signed(delta.clamp(-1000, 1000) as i16);
    }

    /// Scrolls the MESSAGES / QUERY PLAN text a few columns left or right.
    pub(super) fn scroll_tab_across(&mut self, dir: i16) {
        const STEP: i16 = 8;
        self.tab.tab_hscroll = self.tab.tab_hscroll.saturating_add_signed(dir * STEP);
    }

    pub(super) fn run_now(
        &mut self,
        sql: String,
        offset: usize,
        infos: Vec<StatementInfo>,
        purpose: RunPurpose,
        filled: Option<Filled>,
    ) {
        // A query's own result, unless it's the shown one run again.
        // (Taken whatever runs, so it can't outlive the run it was for.)
        let same_result = std::mem::take(&mut self.same_result);
        let new_result = matches!(purpose, RunPurpose::Query) && !self.watch_rerun && !same_result;
        // Anything else run stops watch mode.
        if !self.watch_rerun {
            self.stop_watch(": another query ran");
            // The tab for what's coming, while it runs.
            match purpose {
                RunPurpose::Query => self.set_result_tab(ResultTab::Rows),
                RunPurpose::Explain => self.set_result_tab(ResultTab::Plan),
                _ => {}
            }
        }
        let Some(db) = self.database.clone() else { return };
        // A still-open streamed result holds the session: close it (which
        // cancels it on the server) so this run can start; so does an export.
        self.stop_export();
        self.close_stream();
        self.completion = None;
        // One read-only statement can be streamed page by page.
        let stream = matches!(purpose, RunPurpose::Query) && infos.len() == 1 && infos[0].kind == Kind::Read;
        self.last_run_infos = infos;
        self.running_purpose = purpose;
        self.run_extra.vertical = false;
        if matches!(self.running_purpose, RunPurpose::Query) {
            self.run_extra.vertical = sql::ends_vertical(&sql, self.engine());
            let original = filled.as_ref().map_or(&sql, |f| &f.original);
            if new_result {
                self.open_run(Some((original, offset)));
            }
            // Remembered for "load more" and error locations; helper runs
            // (EXPLAIN, cell edits) don't replace them.
            self.tab.last_sql = Some((sql.clone(), offset));
            self.tab.last_filled.clone_from(&filled);
            self.tab.last_query_infos.clone_from(&self.last_run_infos);
            // History keeps the placeholders, not the values, and no
            // statement that sets a password.
            if !sql::sets_password(original, self.engine()) {
                self.history.push(original);
            }
            self.tab.last_run_text = self.tab.editor.text();
        } else {
            self.tab.last_run_text.clear();
        }
        self.tab.last_run_offset = offset;
        self.last_run_filled = filled;
        self.last_activity = Instant::now();
        self.run_extra.timed_out = None;
        self.run_extra.started = Some(Instant::now());
        self.run_extra.deadline =
            self.connect_form.query_timeout().ok().flatten().map(|limit| (Instant::now() + limit, limit));
        self.tab.error_marker = None;
        self.executing = true;
        self.running_since = Some(Instant::now());
        // A read leaves the other tabs free meanwhile (see `in_tab`).
        self.run_id += 1;
        let reads = !self.last_run_infos.is_empty() && self.last_run_infos.iter().all(|i| i.kind == Kind::Read);
        let plan = self.plan_request.as_ref().is_some_and(|(_, analyzed, _)| !analyzed);
        let free = match self.running_purpose {
            RunPurpose::Query => reads,
            RunPurpose::Explain => plan,
            _ => false,
        };
        self.run_owner = free.then_some(self.active_tab);
        self.set_result_status("Running… (Esc to cancel)".to_string(), false);
        let send = self.run_sender();
        let row_limit = self.next_row_limit.take().unwrap_or(self.settings.row_limit);
        tracing::debug!(statements = self.last_run_infos.len(), stream, row_limit, "run");
        let sql = if self.run_extra.vertical { sql::without_vertical(&sql, self.engine()) } else { sql };
        self.task = Some(tokio::spawn(async move {
            if stream {
                // No transaction probe: the open stream holds the session,
                // and a single read can't change the transaction state.
                match db.run_streaming(&sql, row_limit).await {
                    Ok((outcome, rows)) => send(DbEventKind::QueryDone(outcome, None, rows)),
                    Err(e) => {
                        let txn = after_error(&db, &e).await;
                        send(DbEventKind::QueryFailed(
                            crate::db::error_message(&e),
                            crate::db::error_position(&e),
                            txn,
                        ));
                    }
                }
                return;
            }
            match db.run(&sql, row_limit).await {
                Ok(outcome) => {
                    let txn = db.transaction_state().await;
                    send(DbEventKind::QueryDone(outcome, txn, None));
                }
                Err(e) => {
                    let txn = after_error(&db, &e).await;
                    send(DbEventKind::QueryFailed(crate::db::error_message(&e), crate::db::error_position(&e), txn));
                }
            }
        }));
    }

    /// Asks the open stream for `rows` more rows (once at a time).
    pub(super) fn fetch_more(&mut self, rows: usize) {
        let Some(stream) = self.row_stream.clone() else { return };
        if self.fetching_more {
            return;
        }
        self.fetching_more = true;
        let send = self.sender();
        tokio::spawn(async move {
            match stream.next_page(rows).await {
                Ok(page) => send(DbEventKind::MoreRows(page)),
                Err(e) => send(DbEventKind::MoreFailed(e)),
            }
        });
    }

    /// Adds a fetched page to the results, keeping the selection.
    pub(super) fn append_rows(&mut self, page: Page) {
        self.fetching_more = false;
        let Some(results) = &mut self.tab.grid.results else { return };
        let start = results.rows.len();
        results.total_rows += page.rows.len() as u64;
        self.tab.grid.col_widths = widen_columns(std::mem::take(&mut self.tab.grid.col_widths), &page.rows);
        results.rows.extend(page.rows);
        if page.done {
            self.row_stream = None;
        }
        let loaded = results.rows.len();
        if self.tab.grid.sort.is_some() || !self.tab.grid.filter.is_empty() {
            let selected = self.tab.grid.table_state.selected();
            self.tab.grid.view = compute_view(results, self.tab.grid.filter.text(), self.tab.grid.sort);
            self.tab.grid.table_state.select(selected.map(|s| s.min(self.tab.grid.view.len().saturating_sub(1))));
        } else {
            self.tab.grid.view.extend(start..loaded);
        }
        let more = if self.row_stream.is_some() { " · more as you scroll" } else { " · all rows loaded" };
        self.set_status(format!("{loaded} rows loaded{more}"), false);
    }

    /// Near the end of the loaded rows of a streamed result, fetch more.
    pub(super) fn maybe_fetch_more(&mut self) {
        const LOOKAHEAD: usize = 50;
        // The stream's rows are the result's, not the plan's.
        if self.tab.plan_swapped {
            return;
        }
        let near_end = self.tab.grid.table_state.selected().is_some_and(|s| s + LOOKAHEAD >= self.tab.grid.view.len());
        if near_end {
            self.fetch_more(self.settings.row_limit);
        }
    }

    pub(super) fn cancel_query(&mut self) {
        if self.cancelling {
            return;
        }
        let Some(db) = self.database.clone() else { return };
        self.cancelling = true;
        self.queued_plan = None;
        self.set_result_status("Cancelling...".to_string(), false);
        let send = self.sender();
        tokio::spawn(async move {
            if let Err(e) = db.cancel().await {
                send(DbEventKind::CancelFailed(crate::db::error_message(&e)));
            }
        });
    }

    /// Disconnects, asking first if a transaction would be rolled back.
    pub(super) fn request_disconnect(&mut self) {
        // (In its own tab, disconnecting stops it.)
        if self.executing && !self.running_here() {
            return self.set_status(self.running_elsewhere("disconnect once it's done"), true);
        }
        if self.txn == TxnState::Idle {
            return self.disconnect();
        }
        self.confirm = Some(Confirm {
            message: "A transaction is open. Disconnect and roll it back?".to_string(),
            details: vec![],
            danger: true,
            action: ConfirmAction::Disconnect,
            offer_transaction: false,
        });
    }
}

/// The transaction state after a failed run: none once the session was
/// reopened, otherwise what the server says.
async fn after_error(db: &Database, err: &anyhow::Error) -> Option<TxnState> {
    if crate::db::is_reconnected(err) { Some(TxnState::Idle) } else { db.transaction_state().await }
}

/// The nearest selectable (non-section) row from `from` in direction
/// `dir`, falling back to the other direction.
pub(super) fn step_selectable(nodes: &[SchemaNode], from: usize, dir: isize) -> usize {
    let ok = |i: usize| nodes.get(i).is_some_and(|n| !matches!(n, SchemaNode::Section(_)));
    let mut i = from.min(nodes.len().saturating_sub(1));
    loop {
        if ok(i) {
            return i;
        }
        match i.checked_add_signed(dir) {
            Some(next) if next < nodes.len() => i = next,
            _ => break,
        }
    }
    (0..nodes.len())
        .rev()
        .find(|&i| ok(i))
        .filter(|_| dir < 0)
        .or_else(|| (0..nodes.len()).find(|&i| ok(i)))
        .unwrap_or(0)
}
