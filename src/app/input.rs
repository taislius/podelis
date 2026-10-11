//! A one-line text input, with the same keys everywhere it's used: the
//! prompts, filters, the find bar, the palette and the library's search.
//! Forms (connect, parameters, a new row) edit their fields at the end
//! with the same keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What a key did to an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputResult {
    /// The text changed.
    Changed,
    /// Only the cursor moved.
    Moved,
    /// Enter.
    Submit,
    /// Esc.
    Cancel,
    /// Not an editing key: the caller may handle it.
    Ignored,
}

/// One line of text and a cursor in it (a byte offset at a char boundary).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineInput {
    text: String,
    cursor: usize,
}

impl LineInput {
    /// `text`, with the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        LineInput { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text before and after the cursor.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replaces the text; the cursor goes to the end.
    pub fn set(&mut self, text: impl Into<String>) {
        *self = LineInput::new(text);
    }

    /// Inserts pasted text at the cursor: a trailing line break is dropped
    /// (copied lines often end in one), other line breaks and tabs become
    /// spaces, and other control characters are dropped.
    pub fn insert_str(&mut self, text: &str) {
        let clean: String = text
            .trim_end_matches(['\r', '\n'])
            .replace("\r\n", " ")
            .chars()
            .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// Typing, Backspace/Delete, ←/→, Home/End, Ctrl+U (clear before the
    /// cursor) and Ctrl+W (delete the word before it); Enter and Esc are
    /// reported for the caller.
    pub fn handle(&mut self, key: KeyEvent) -> InputResult {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter => InputResult::Submit,
            KeyCode::Esc => InputResult::Cancel,
            KeyCode::Char('u') if ctrl => self.delete_back_to(0),
            KeyCode::Char('w') if ctrl => self.delete_back_to(self.word_start()),
            KeyCode::Backspace if alt => self.delete_back_to(self.word_start()),
            KeyCode::Char(c) if !ctrl && !alt => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                InputResult::Changed
            }
            KeyCode::Backspace => match self.prev_boundary() {
                Some(at) => self.delete_back_to(at),
                None => InputResult::Ignored,
            },
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
                InputResult::Changed
            }
            KeyCode::Left => self.move_to(self.prev_boundary().unwrap_or(0)),
            KeyCode::Right => {
                let next = self.text[self.cursor..].chars().next().map_or(self.cursor, |c| self.cursor + c.len_utf8());
                self.move_to(next)
            }
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.text.len()),
            _ => InputResult::Ignored,
        }
    }

    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i)
    }

    /// Start of the word before the cursor (skipping spaces first).
    fn word_start(&self) -> usize {
        let trimmed = self.text[..self.cursor].trim_end();
        trimmed.char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(0, |(i, c)| i + c.len_utf8())
    }

    fn delete_back_to(&mut self, at: usize) -> InputResult {
        if at == self.cursor {
            return InputResult::Ignored;
        }
        self.text.replace_range(at..self.cursor, "");
        self.cursor = at;
        InputResult::Changed
    }

    fn move_to(&mut self, at: usize) -> InputResult {
        if at == self.cursor {
            return InputResult::Ignored;
        }
        self.cursor = at;
        InputResult::Moved
    }
}

/// Edits a form's field with `LineInput`'s keys, at its end.
pub fn edit_at_end(text: &mut String, key: KeyEvent) -> InputResult {
    let mut input = LineInput::new(std::mem::take(text));
    let result = input.handle(key);
    *text = input.text;
    result
}

/// Pastes at the end of a form's field.
pub fn paste_at_end(text: &mut String, pasted: &str) {
    let mut input = LineInput::new(std::mem::take(text));
    input.insert_str(pasted);
    *text = input.text;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut LineInput, code: KeyCode) -> InputResult {
        input.handle(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(input: &mut LineInput, c: char) -> InputResult {
        input.handle(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn types_and_moves_by_characters_not_bytes() {
        let mut input = LineInput::new("Šiauliai");
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Right);
        assert_eq!(input.split(), ("Š", "iauliai"));
        press(&mut input, KeyCode::Char('ė'));
        assert_eq!(input.text(), "Šėiauliai");
        assert_eq!(press(&mut input, KeyCode::Backspace), InputResult::Changed);
        press(&mut input, KeyCode::Left);
        assert_eq!(press(&mut input, KeyCode::Delete), InputResult::Changed);
        assert_eq!(input.text(), "iauliai");
        assert_eq!(press(&mut input, KeyCode::Left), InputResult::Ignored);
    }

    #[test]
    fn deletes_words_and_lines_before_the_cursor() {
        let mut input = LineInput::new("grep -i  vilnius");
        assert_eq!(ctrl(&mut input, 'w'), InputResult::Changed);
        assert_eq!(input.text(), "grep -i  ");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "grep ");
        press(&mut input, KeyCode::Char('x'));
        press(&mut input, KeyCode::Home);
        press(&mut input, KeyCode::Right);
        ctrl(&mut input, 'u');
        assert_eq!(input.split(), ("", "rep x"));
        assert_eq!(press(&mut input, KeyCode::Enter), InputResult::Submit);
        assert_eq!(press(&mut input, KeyCode::Esc), InputResult::Cancel);
        assert_eq!(press(&mut input, KeyCode::F(5)), InputResult::Ignored);
    }

    #[test]
    fn deletes_words_after_wide_spaces() {
        let mut input = LineInput::new("a\u{a0}b\u{3000}c");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "a\u{a0}b\u{3000}");
        ctrl(&mut input, 'w');
        assert_eq!(input.text(), "a\u{a0}");
        input.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(input.text(), "");
    }

    #[test]
    fn pastes_one_line_at_the_cursor() {
        let mut input = LineInput::new("ab");
        press(&mut input, KeyCode::Left);
        input.insert_str("1\r\n2\n3\t4");
        assert_eq!(input.split(), ("a1 2 3 4", "b"));
        let mut field = "x".to_string();
        paste_at_end(&mut field, "y\nz\r\n");
        assert_eq!(field, "xy z", "a copied line's own line break is dropped");
        edit_at_end(&mut field, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(field, "xy ");
    }
}
