//! The editor's keys: typing, completion, undo, history.

use super::super::*;

/// How soon a second Space counts as a double press.
const DOUBLE_SPACE: Duration = Duration::from_millis(250);

impl App {
    pub(in crate::app) fn handle_editor_key(&mut self, key: KeyEvent) {
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
    pub(in crate::app) fn double_space(&mut self, key: KeyEvent) -> bool {
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
    pub(in crate::app) fn editor_inserting(&self) -> bool {
        self.vim_mode().is_none_or(|m| m == VimMode::Insert)
    }

    pub(in crate::app) fn handle_standard_editor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.panes.editor_height.saturating_sub(1).max(1);
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
    pub(in crate::app) fn word_before_cursor(&self) -> bool {
        let text = self.tab.editor.text();
        text[..self.tab.editor.cursor_offset()]
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }

    pub(in crate::app) fn compute_completion(&self) -> Option<Completion> {
        let tables: Vec<TableRef> = self
            .connection
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
                    .filter(|q| {
                        q.group == crate::config::library::SNIPPETS && q.name.to_lowercase().starts_with(&lower)
                    })
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

    pub(in crate::app) fn open_completion(&mut self) {
        self.completion = self.compute_completion();
        if self.completion.is_none() {
            let loading = self.connection.schema.as_ref().is_some_and(|s| s.loading);
            let msg = if loading { t!("editor.no_completions_yet") } else { t!("editor.no_completions") };
            self.notes.editor_note = Some(msg.to_string());
            self.notes.editor_note_is_error = false;
        }
    }

    /// Keys for the open popup. Returns false for keys it doesn't handle
    /// (they go to the editor and the popup refilters).
    pub(in crate::app) fn handle_completion_key(&mut self, key: KeyEvent) -> bool {
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

    pub(in crate::app) fn undo(&mut self) {
        self.focus = Focus::Editor;
        if self.tab.editor.undo() {
            self.edited();
        } else {
            self.set_status(t!("editor.nothing_to_undo").to_string(), false);
        }
    }

    pub(in crate::app) fn redo(&mut self) {
        self.focus = Focus::Editor;
        if self.tab.editor.redo() {
            self.edited();
        } else {
            self.set_status(t!("editor.nothing_to_redo").to_string(), false);
        }
    }

    pub(in crate::app) fn history_older(&mut self) {
        if let Some(text) = self.history.older(&self.tab.editor.text()) {
            let text = text.to_string();
            self.tab.editor.set_text(&text);
            self.focus = Focus::Editor;
            self.tab.error_marker = None;
            self.tab.syntax_hint = None;
        }
    }

    pub(in crate::app) fn history_newer(&mut self) {
        if let Some(text) = self.history.newer() {
            let text = text.to_string();
            self.tab.editor.set_text(&text);
            self.focus = Focus::Editor;
            self.tab.error_marker = None;
            self.tab.syntax_hint = None;
        }
    }
}
