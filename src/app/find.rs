//! Find and replace in the editor (Ctrl+F), comment toggling and SQL
//! formatting.

use std::ops::Range;

use super::*;

/// The find bar under the editor.
#[derive(Default)]
pub struct FindBar {
    pub query: LineInput,
    pub replacement: LineInput,
    /// Typing goes into the replacement (Tab switches).
    pub in_replace: bool,
}

/// Byte ranges of `needle` in `text`, ignoring case.
pub fn find_all(text: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    // An ASCII search, the usual case: lowercasing ASCII keeps every byte
    // where it was, so the standard library's fast search finds the same
    // (non-overlapping) matches as the char by char comparison below.
    if needle.is_ascii() {
        let needle = needle.to_ascii_lowercase();
        return text.to_ascii_lowercase().match_indices(&needle).map(|(i, _)| i..i + needle.len()).collect();
    }
    let mut out = Vec::new();
    let mut from = 0;
    while from < text.len() {
        let Some(len) = match_len(&text[from..], needle) else {
            from += text[from..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        out.push(from..from + len);
        from += len.max(1);
    }
    out
}

/// Length in bytes of `needle` at the start of `hay`, ignoring case.
fn match_len(hay: &str, needle: &str) -> Option<usize> {
    let mut chars = hay.char_indices();
    for n in needle.chars() {
        let (_, h) = chars.next()?;
        if !h.to_lowercase().eq(n.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map_or(hay.len(), |(i, _)| i))
}

impl App {
    pub(super) fn open_find(&mut self) {
        let mut bar = self.find.take().unwrap_or_default();
        // A one-line selection becomes the search.
        if let Some(text) = self.tab.editor.selected_text().filter(|t| !t.contains('\n')) {
            bar.query.set(text);
        }
        bar.in_replace = false;
        self.find = Some(bar);
        self.focus = Focus::Editor;
    }

    /// The matches of the find bar's query, and which one is selected.
    pub fn find_matches(&self) -> (Vec<Range<usize>>, Option<usize>) {
        let Some(bar) = &self.find else { return (Vec::new(), None) };
        let matches = find_all(&self.tab.editor.text(), bar.query.text());
        let selected = self.tab.editor.selection_offsets();
        let current = matches.iter().position(|m| selected == Some((m.start, m.end)));
        (matches, current)
    }

    pub(super) fn handle_find_key(&mut self, key: KeyEvent) {
        let Some(bar) = &mut self.find else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let field = if bar.in_replace { &mut bar.replacement } else { &mut bar.query };
        match key.code {
            KeyCode::Esc => self.find = None,
            KeyCode::Tab | KeyCode::BackTab => bar.in_replace = !bar.in_replace,
            KeyCode::Char('a') if ctrl || alt => self.replace_all(),
            KeyCode::Enter if bar.in_replace => self.replace_current(),
            KeyCode::Up => self.find_step(false),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.find_step(false),
            KeyCode::Enter | KeyCode::Down => self.find_step(true),
            _ => {
                if field.handle(key) == InputResult::Changed {
                    self.find_from_cursor();
                }
            }
        }
    }

    pub(super) fn paste_find(&mut self, text: &str) {
        if let Some(bar) = &mut self.find {
            let field = if bar.in_replace { &mut bar.replacement } else { &mut bar.query };
            field.insert_str(text);
            self.find_from_cursor();
        }
    }

    /// Selects the first match at or after the selection's start (typing
    /// in the search box refines the match in place).
    fn find_from_cursor(&mut self) {
        if self.find.as_ref().is_some_and(|b| b.in_replace) {
            return;
        }
        let (matches, _) = self.find_matches();
        let from = self.tab.editor.selection_offsets().map_or_else(|| self.tab.editor.cursor_offset(), |(a, _)| a);
        match matches.iter().find(|m| m.start >= from).or(matches.first()) {
            Some(m) => self.tab.editor.select_offsets(m.start, m.end),
            None => self.tab.editor.clear_selection(),
        }
    }

    fn find_step(&mut self, forward: bool) {
        let (matches, current) = self.find_matches();
        if matches.is_empty() {
            return self.set_status("No matches".to_string(), true);
        }
        let n = matches.len();
        let at = self.tab.editor.cursor_offset();
        let next = match (current, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => matches.iter().position(|m| m.start >= at).unwrap_or(0),
            (None, false) => matches.iter().rposition(|m| m.end <= at).unwrap_or(n - 1),
        };
        self.tab.editor.select_offsets(matches[next].start, matches[next].end);
    }

    fn replace_current(&mut self) {
        let (_, current) = self.find_matches();
        let Some(replacement) = self.find.as_ref().map(|b| b.replacement.clone()) else { return };
        if current.is_some() {
            self.tab.editor.insert_str(replacement.text());
            self.edited();
        }
        // On to the next match after the cursor.
        let (matches, _) = self.find_matches();
        let at = self.tab.editor.cursor_offset();
        if let Some(m) = matches.iter().find(|m| m.start >= at).or(matches.first()) {
            self.tab.editor.select_offsets(m.start, m.end);
        }
    }

    fn replace_all(&mut self) {
        let (matches, _) = self.find_matches();
        let Some(bar) = &self.find else { return };
        if matches.is_empty() {
            return self.set_status("No matches".to_string(), true);
        }
        let text = self.tab.editor.text();
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for m in &matches {
            out.push_str(&text[pos..m.start]);
            out.push_str(bar.replacement.text());
            pos = m.end;
        }
        out.push_str(&text[pos..]);
        let cursor = self.tab.editor.cursor_offset().min(out.len());
        let cursor = (0..=cursor).rev().find(|&i| out.is_char_boundary(i)).unwrap_or(0);
        self.tab.editor.replace_text(&out, cursor);
        self.edited();
        self.set_status(
            format!("Replaced {} match{}", matches.len(), if matches.len() == 1 { "" } else { "es" }),
            false,
        );
    }

    pub(super) fn toggle_comment(&mut self) {
        self.focus = Focus::Editor;
        self.tab.editor.toggle_comment();
        self.edited();
    }

    /// Formats the selection, or the whole text.
    pub(super) fn format_sql(&mut self) {
        use sqlformat::{FormatOptions, Indent, QueryParams};
        let options = FormatOptions {
            indent: Indent::Spaces(2),
            max_inline_arguments: Some(80),
            max_inline_top_level: Some(80),
            dialect: self.engine().dialect().formatter(),
            ..FormatOptions::default()
        };
        let text = self.tab.editor.text();
        let (start, end) = self.tab.editor.selection_offsets().unwrap_or((0, text.len()));
        let formatted = sqlformat::format(&text[start..end], &QueryParams::None, &options);
        if formatted.trim() == text[start..end].trim() {
            return self.set_status("Already formatted".to_string(), false);
        }
        let new = format!("{}{}{}", &text[..start], formatted, &text[end..]);
        self.tab.editor.replace_text(&new, start + formatted.len());
        self.edited();
        self.focus = Focus::Editor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_ignoring_case() {
        assert_eq!(find_all("Select x FROM t select", "select"), [0..6, 16..22]);
        assert_eq!(find_all("žąsis ŽĄSIS", "žą"), [0..4, 8..12]);
        assert_eq!(find_all("aaa", "aa"), vec![0..2]);
        assert!(find_all("abc", "").is_empty());
        // An ASCII search through non-ASCII text keeps byte positions right.
        let text = "'žąsis' INSERT ąž insert";
        let found = find_all(text, "Insert");
        assert_eq!(found.iter().map(|m| &text[m.clone()]).collect::<Vec<_>>(), ["INSERT", "insert"]);
    }
}
