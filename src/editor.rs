use unicode_width::UnicodeWidthStr;

/// A position in the buffer: (line, char column).
pub type Pos = (usize, usize);

const UNDO_LIMIT: usize = 200;
/// Undo keeps whole copies of the text: with a long script pasted, 200 of
/// them would take gigabytes. The oldest go once they add up to this (the
/// newest is always kept, so the last change can be undone).
const UNDO_BYTES: usize = 64 << 20;

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
}

impl Snapshot {
    fn bytes(&self) -> usize {
        self.lines.iter().map(|l| l.len() + std::mem::size_of::<String>()).sum()
    }
}

/// Undo or redo steps, newest last, within `UNDO_LIMIT` and `UNDO_BYTES`.
#[derive(Default)]
struct History {
    steps: std::collections::VecDeque<Snapshot>,
    bytes: usize,
}

impl History {
    fn push(&mut self, snap: Snapshot) {
        self.bytes += snap.bytes();
        self.steps.push_back(snap);
        while self.steps.len() > 1 && (self.steps.len() > UNDO_LIMIT || self.bytes > UNDO_BYTES) {
            let oldest = self.steps.pop_front().expect("more than one step");
            self.bytes -= oldest.bytes();
        }
    }

    fn pop(&mut self) -> Option<Snapshot> {
        let snap = self.steps.pop_back()?;
        self.bytes -= snap.bytes();
        Some(snap)
    }

    fn clear(&mut self) {
        self.steps.clear();
        self.bytes = 0;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    /// Typing a word; consecutive keystrokes undo together.
    Typing,
    Other,
}

/// A minimal multi-line text buffer used for the SQL query input.
/// Rolling a tiny editor here instead of pulling in `tui-textarea` avoids
/// that crate's ratatui version pin conflicting with ours.
pub struct TextEditor {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    /// Cursor position in chars (not bytes, not display columns).
    pub cursor_col: usize,
    /// First visible line.
    pub scroll_row: usize,
    /// First visible display column.
    pub scroll_col: usize,
    /// Other end of the selection, if one is active.
    anchor: Option<Pos>,
    undo: History,
    redo: History,
    last_edit: Option<EditKind>,
    /// Changes with every edit (and differs between editors), so what's
    /// worked out from the text can be kept until it changes.
    version: u64,
    /// The view was scrolled (the mouse wheel) with the cursor here and
    /// the text at this version: it stays where it was put, not following
    /// the cursor, until either changes.
    scrolled_from: Option<(Pos, u64)>,
}

/// The next editor version: one counter for all, so a new editor never
/// reuses another's.
fn next_version() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for TextEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEditor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            scroll_col: 0,
            anchor: None,
            undo: History::default(),
            redo: History::default(),
            last_edit: None,
            version: next_version(),
            scrolled_from: None,
        }
    }

    /// Changes whenever the text does.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// A fresh editor holding `text` (no undo history), cursor at the end.
    pub fn with_text(text: &str) -> Self {
        let mut e = Self::new();
        e.raw_insert_str(text);
        e
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn cursor(&self) -> Pos {
        (self.cursor_row, self.cursor_col)
    }

    /// Moves the cursor, clamped to the text.
    pub fn set_cursor(&mut self, pos: Pos) {
        self.cursor_row = pos.0.min(self.lines.len() - 1);
        self.cursor_col = pos.1.min(self.line_len(self.cursor_row));
    }

    /// Length of a line in chars.
    pub fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |l| l.chars().count())
    }

    /// The char at a position, if any (not the line break).
    pub fn char_at_pos(&self, (row, col): Pos) -> Option<char> {
        self.lines.get(row)?.chars().nth(col)
    }

    /// Selects from `anchor` to the cursor (exclusive of the cursor).
    pub fn set_anchor(&mut self, anchor: Option<Pos>) {
        self.anchor = anchor;
    }

    pub fn anchor(&self) -> Option<Pos> {
        self.anchor
    }

    /// Byte offset of a position within `text()`.
    pub fn offset_of(&self, (row, col): Pos) -> usize {
        let before: usize = self.lines[..row].iter().map(|l| l.len() + 1).sum();
        before + char_to_byte_idx(&self.lines[row], col)
    }

    /// Byte offset of the cursor within `text()`.
    pub fn cursor_offset(&self) -> usize {
        self.offset_of(self.cursor())
    }

    /// The position of a byte offset within `text()` (clamped to the end).
    pub fn pos_of_offset(&self, offset: usize) -> Pos {
        let mut remaining = offset;
        for (row, line) in self.lines.iter().enumerate() {
            if remaining <= line.len() {
                let col = line[..floor_char_boundary(line, remaining)].chars().count();
                return (row, col);
            }
            remaining -= line.len() + 1;
        }
        let last = self.lines.len() - 1;
        (last, self.lines[last].chars().count())
    }

    /// Display column of the cursor, accounting for wide characters.
    /// The char column at screen column `display` of `row` (for clicks).
    pub fn col_at_display(&self, row: usize, display: usize) -> usize {
        let Some(line) = self.lines.get(row) else { return 0 };
        let mut used = 0;
        for (i, c) in line.chars().enumerate() {
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if used + w > display {
                return i;
            }
            used += w;
        }
        line.chars().count()
    }

    pub fn cursor_display_col(&self) -> usize {
        let line = &self.lines[self.cursor_row];
        line[..char_to_byte_idx(line, self.cursor_col)].width()
    }

    /// Puts the cursor at (row, column) and the view at `scroll`, clamped to
    /// the text (it may have changed since they were saved).
    pub fn restore_cursor(&mut self, (row, col): (usize, usize), scroll: usize) {
        self.cursor_row = row.min(self.lines.len() - 1);
        self.cursor_col = col.min(self.lines[self.cursor_row].chars().count());
        self.scroll_row = scroll.min(self.cursor_row);
    }

    /// Scrolls the view by `rows` (down if positive), leaving the cursor
    /// and the selection where they are; `height` rows are shown.
    pub fn scroll_view(&mut self, rows: isize, height: usize) {
        let last = self.lines.len().saturating_sub(height.max(1));
        self.scroll_row = self.scroll_row.saturating_add_signed(rows).min(last);
        self.scrolled_from = Some((self.cursor(), self.version));
    }

    /// Whether the cursor's row is in a view of `height` rows.
    pub fn cursor_in_view(&self, height: usize) -> bool {
        (self.scroll_row..self.scroll_row + height).contains(&self.cursor_row)
    }

    /// Adjusts scroll offsets so the cursor is inside a `height` x `width`
    /// viewport (not after `scroll_view`, until the cursor or text changes).
    pub fn scroll_to_cursor(&mut self, height: usize, width: usize) {
        if self.scrolled_from.is_some_and(|from| from == (self.cursor(), self.version)) {
            self.scroll_row = self.scroll_row.min(self.lines.len().saturating_sub(1));
        } else if height > 0 {
            self.scrolled_from = None;
            if self.cursor_row < self.scroll_row {
                self.scroll_row = self.cursor_row;
            } else if self.cursor_row >= self.scroll_row + height {
                self.scroll_row = self.cursor_row + 1 - height;
            }
        }
        if width > 0 {
            let col = self.cursor_display_col();
            if col < self.scroll_col {
                // Scroll back as far as possible while keeping the cursor
                // visible, so short lines aren't left looking empty.
                self.scroll_col = col.saturating_sub(width - 1);
            } else if col >= self.scroll_col + width {
                self.scroll_col = col + 1 - width;
            }
        }
    }

    // ----------------------------------------------------------- selection

    /// The selected range, ordered, or `None` if nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        let cursor = self.cursor();
        match anchor.cmp(&cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, cursor)),
            std::cmp::Ordering::Greater => Some((cursor, anchor)),
        }
    }

    /// Byte range of the selection within `text()`.
    pub fn selection_offsets(&self) -> Option<(usize, usize)> {
        self.selection().map(|(a, b)| (self.offset_of(a), self.offset_of(b)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection_offsets()?;
        Some(self.text()[a..b].to_string())
    }

    /// Call before a cursor movement: `extend` (Shift held) starts or grows a
    /// selection; otherwise any selection is dropped.
    pub fn prepare_move(&mut self, extend: bool) {
        self.last_edit = None;
        if extend {
            self.anchor.get_or_insert(self.cursor());
        } else {
            self.anchor = None;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        let last = self.lines.len() - 1;
        self.set_cursor((last, self.lines[last].chars().count()));
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Removes the selected text. Returns false if nothing was selected.
    fn raw_delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = self.lines[end.0][char_to_byte_idx(&self.lines[end.0], end.1)..].to_string();
        let head_end = char_to_byte_idx(&self.lines[start.0], start.1);
        self.lines[start.0].truncate(head_end);
        self.lines[start.0].push_str(&tail);
        self.lines.drain(start.0 + 1..=end.0);
        self.set_cursor(start);
        self.anchor = None;
        true
    }

    /// Selects bytes `start..end` of the text, cursor at the end.
    pub fn select_offsets(&mut self, start: usize, end: usize) {
        let start = self.pos_of_offset(start);
        let end = self.pos_of_offset(end);
        self.set_cursor(end);
        self.anchor = (start != end).then_some(start);
    }

    /// Replaces the whole text (undoable) and puts the cursor at byte
    /// `cursor` of the new text.
    pub fn replace_text(&mut self, text: &str, cursor: usize) {
        self.checkpoint(EditKind::Other);
        self.lines = vec![String::new()];
        self.set_cursor((0, 0));
        self.anchor = None;
        self.raw_insert_str(text);
        let pos = self.pos_of_offset(cursor.min(text.len()));
        self.set_cursor(pos);
    }

    /// Comments out the selected lines (or the cursor's) with `-- `, or
    /// uncomments them if they all are.
    pub fn toggle_comment(&mut self) {
        let (first, last) = match self.selection() {
            // A selection ending at column 0 doesn't include that line.
            Some((a, b)) if b.1 == 0 && b.0 > a.0 => (a.0, b.0 - 1),
            Some((a, b)) => (a.0, b.0),
            None => (self.cursor_row, self.cursor_row),
        };
        let rows = first..=last;
        let code = |l: &String| !l.trim().is_empty();
        let all_commented =
            self.lines[rows.clone()].iter().filter(|l| code(l)).all(|l| l.trim_start().starts_with("--"));
        let indent = self.lines[rows.clone()]
            .iter()
            .filter(|l| code(l))
            .map(|l| l.chars().take_while(|c| *c == ' ').count())
            .min()
            .unwrap_or(0);
        self.checkpoint(EditKind::Other);
        let mut cursor_shift: isize = 0;
        let mut anchor_shift: isize = 0;
        let anchor_row = self.anchor.map(|a| a.0);
        for row in rows {
            let line = &mut self.lines[row];
            let delta: isize = if all_commented {
                let lead = line.len() - line.trim_start().len();
                if !line[lead..].starts_with("--") {
                    continue;
                }
                let n = if line[lead + 2..].starts_with(' ') { 3 } else { 2 };
                line.replace_range(lead..lead + n, "");
                -(n as isize)
            } else if code(line) {
                let at = char_to_byte_idx(line, indent);
                line.insert_str(at, "-- ");
                3
            } else {
                0
            };
            if row == self.cursor_row {
                cursor_shift = delta;
            }
            if Some(row) == anchor_row {
                anchor_shift = delta;
            }
        }
        let shift = |col: usize, d: isize| col.saturating_add_signed(d);
        self.cursor_col = shift(self.cursor_col, cursor_shift).min(self.lines[self.cursor_row].chars().count());
        if let Some((r, c)) = self.anchor {
            self.anchor = Some((r, shift(c, anchor_shift).min(self.lines[r].chars().count())));
        }
    }

    /// The bracket matching the one at or just before the cursor, as
    /// (that bracket, its match). Quotes aren't considered.
    pub fn matching_bracket(&self) -> Option<(Pos, Pos)> {
        let text = self.text();
        let at = self.cursor_offset();
        let is_bracket = |c: char| "()[]{}".contains(c);
        let here = text[at..].chars().next().filter(|c| is_bracket(*c)).map(|c| (at, c));
        let before = text[..at].chars().next_back().filter(|c| is_bracket(*c)).map(|c| (at - c.len_utf8(), c));
        let (pos, c) = here.or(before)?;
        let (open, close, forward) = match c {
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '{' => ('{', '}', true),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            _ => ('{', '}', false),
        };
        let mut depth = 0usize;
        let found = if forward {
            text[pos..].char_indices().find_map(|(i, ch)| {
                if ch == open {
                    depth += 1;
                } else if ch == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(pos + i);
                    }
                }
                None
            })
        } else {
            text[..=pos].char_indices().rev().find_map(|(i, ch)| {
                if ch == close {
                    depth += 1;
                } else if ch == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                None
            })
        }?;
        Some((self.pos_of_offset(pos), self.pos_of_offset(found)))
    }

    // ---------------------------------------------------------------- undo

    fn checkpoint(&mut self, kind: EditKind) {
        self.version = next_version();
        if !(kind == EditKind::Typing && self.last_edit == Some(EditKind::Typing)) {
            self.undo.push(Snapshot { lines: self.lines.clone(), cursor: self.cursor() });
        }
        self.redo.clear();
        self.last_edit = Some(kind);
    }

    fn restore(&mut self, snap: Snapshot) {
        self.version = next_version();
        self.lines = snap.lines;
        self.set_cursor(snap.cursor);
        self.anchor = None;
        self.last_edit = None;
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo.pop() else { return false };
        self.redo.push(Snapshot { lines: self.lines.clone(), cursor: self.cursor() });
        self.restore(snap);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo.pop() else { return false };
        self.undo.push(Snapshot { lines: self.lines.clone(), cursor: self.cursor() });
        self.restore(snap);
        true
    }

    // ------------------------------------------------------------- editing

    /// Replaces the whole contents (undoable), cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.checkpoint(EditKind::Other);
        self.lines = vec![String::new()];
        self.set_cursor((0, 0));
        self.anchor = None;
        self.raw_insert_str(text);
    }

    pub fn insert_char(&mut self, c: char) {
        let kind = if self.selection().is_some() || c.is_whitespace() { EditKind::Other } else { EditKind::Typing };
        self.checkpoint(kind);
        self.raw_delete_selection();
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_idx(line, self.cursor_col);
        line.insert(byte_idx, c);
        self.cursor_col += 1;
        if c.is_whitespace() {
            self.last_edit = None;
        }
    }

    /// Inserts pasted text, normalising line endings and expanding tabs.
    pub fn insert_str(&mut self, text: &str) {
        self.checkpoint(EditKind::Other);
        self.raw_delete_selection();
        self.raw_insert_str(text);
    }

    fn raw_insert_str(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.raw_newline();
            }
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = char_to_byte_idx(line, self.cursor_col);
            line.insert_str(byte_idx, part);
            self.cursor_col += part.chars().count();
        }
    }

    pub fn insert_newline(&mut self) {
        self.checkpoint(EditKind::Other);
        self.raw_delete_selection();
        self.raw_newline();
    }

    fn raw_newline(&mut self) {
        let line = &mut self.lines[self.cursor_row];
        let byte_idx = char_to_byte_idx(line, self.cursor_col);
        let rest = line.split_off(byte_idx);
        self.lines.insert(self.cursor_row + 1, rest);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// Deletes the selection, if any. Returns its text.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.checkpoint(EditKind::Other);
        self.raw_delete_selection();
        Some(text)
    }

    pub fn backspace(&mut self) {
        self.checkpoint(EditKind::Other);
        if self.raw_delete_selection() {
            return;
        }
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = char_to_byte_idx(line, self.cursor_col - 1);
            line.remove(byte_idx);
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let prev_len = self.lines[self.cursor_row - 1].chars().count();
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.lines[self.cursor_row].push_str(&current);
            self.cursor_col = prev_len;
        }
    }

    pub fn delete(&mut self) {
        self.checkpoint(EditKind::Other);
        if self.raw_delete_selection() {
            return;
        }
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_idx = char_to_byte_idx(line, self.cursor_col);
            line.remove(byte_idx);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    /// Replaces the `len` chars before the cursor on the current line with
    /// `text` (used by autocomplete).
    pub fn replace_before_cursor(&mut self, len: usize, text: &str) {
        self.checkpoint(EditKind::Other);
        self.anchor = None;
        let line = &mut self.lines[self.cursor_row];
        let start = char_to_byte_idx(line, self.cursor_col.saturating_sub(len));
        let end = char_to_byte_idx(line, self.cursor_col);
        line.replace_range(start..end, text);
        self.cursor_col = self.cursor_col.saturating_sub(len) + text.chars().count();
    }

    // ------------------------------------------------------------ movement

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].chars().count());
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].chars().count());
        }
    }

    pub fn page_up(&mut self, lines: usize) {
        for _ in 0..lines.max(1) {
            self.move_up();
        }
    }

    pub fn page_down(&mut self, lines: usize) {
        for _ in 0..lines.max(1) {
            self.move_down();
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    pub fn move_doc_start(&mut self) {
        self.set_cursor((0, 0));
    }

    pub fn move_doc_end(&mut self) {
        let last = self.lines.len() - 1;
        self.set_cursor((last, self.lines[last].chars().count()));
    }

    fn char_before(&self) -> Option<char> {
        let line = &self.lines[self.cursor_row];
        if self.cursor_col == 0 {
            return (self.cursor_row > 0).then_some('\n');
        }
        line.chars().nth(self.cursor_col - 1)
    }

    fn char_at(&self) -> Option<char> {
        let line = &self.lines[self.cursor_row];
        match line.chars().nth(self.cursor_col) {
            Some(c) => Some(c),
            None => (self.cursor_row + 1 < self.lines.len()).then_some('\n'),
        }
    }

    /// Moves to the start of the previous word.
    pub fn move_word_left(&mut self) {
        while self.char_before().is_some_and(|c| !is_word_char(c)) {
            self.move_left();
        }
        while self.char_before().is_some_and(is_word_char) {
            self.move_left();
        }
    }

    /// Moves to the end of the next word.
    pub fn move_word_right(&mut self) {
        while self.char_at().is_some_and(|c| !is_word_char(c)) {
            self.move_right();
        }
        while self.char_at().is_some_and(is_word_char) {
            self.move_right();
        }
    }
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> TextEditor {
        TextEditor::with_text(text)
    }

    #[test]
    fn insert_and_backspace_across_lines() {
        let mut e = TextEditor::new();
        e.insert_str("ab");
        e.insert_newline();
        e.insert_char('c');
        assert_eq!(e.text(), "ab\nc");
        e.backspace();
        e.backspace();
        assert_eq!(e.text(), "ab");
        assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
    }

    #[test]
    fn paste_normalises_line_endings() {
        let e = editor("a\r\nb\rc\td");
        assert_eq!(e.text(), "a\nb\nc    d");
        assert_eq!((e.cursor_row, e.cursor_col), (2, 6));
    }

    #[test]
    fn paste_in_middle_of_line() {
        let mut e = editor("ad");
        e.move_left();
        e.insert_str("b\nc");
        assert_eq!(e.text(), "ab\ncd");
        assert_eq!((e.cursor_row, e.cursor_col), (1, 1));
    }

    #[test]
    fn delete_joins_lines() {
        let mut e = editor("a\nb");
        e.move_up();
        e.move_end();
        e.delete();
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn multibyte_chars() {
        let mut e = editor("ąčę");
        e.move_left();
        e.backspace();
        assert_eq!(e.text(), "ąę");
        assert_eq!(e.cursor_offset(), "ą".len());
    }

    #[test]
    fn offsets_round_trip() {
        let e = editor("ab\nčd\n");
        for offset in [0, 1, 2, 3, 3 + "č".len(), 6, 7] {
            assert_eq!(e.offset_of(e.pos_of_offset(offset)), offset, "offset {offset}");
        }
        assert_eq!(e.pos_of_offset(999), (2, 0));
    }

    #[test]
    fn display_col_accounts_for_wide_chars() {
        let e = editor("日本x");
        assert_eq!(e.cursor_col, 3);
        assert_eq!(e.cursor_display_col(), 5);
    }

    #[test]
    fn scrolls_to_keep_cursor_visible() {
        let mut e = editor("1\n2\n3\n4\n5\n6\n7");
        e.scroll_to_cursor(3, 10);
        assert_eq!(e.scroll_row, 4);
        e.page_up(6);
        e.scroll_to_cursor(3, 10);
        assert_eq!(e.scroll_row, 0);

        let mut e = editor(&"x".repeat(30));
        e.scroll_to_cursor(1, 10);
        assert_eq!(e.scroll_col, 21);
        e.move_home();
        e.scroll_to_cursor(1, 10);
        assert_eq!(e.scroll_col, 0);
    }

    #[test]
    fn the_wheel_scrolls_the_view_and_keeps_the_cursor_and_selection() {
        let mut e = editor("1\n2\n3\n4\n5\n6\n7");
        e.select_offsets(0, 13);
        e.scroll_to_cursor(3, 10);
        assert_eq!(e.scroll_row, 4);
        e.scroll_view(-3, 3);
        e.scroll_to_cursor(3, 10);
        assert_eq!(e.scroll_row, 1, "stays where it was put");
        assert!(!e.cursor_in_view(3) && e.selection().is_some());
        e.scroll_view(-9, 3);
        assert_eq!(e.scroll_row, 0);
        e.scroll_view(9, 3);
        assert_eq!(e.scroll_row, 4, "not past the end");
        // Typing (or moving) brings the cursor back into view.
        e.scroll_view(-4, 3);
        e.insert_char('x');
        e.scroll_to_cursor(3, 10);
        assert!(e.cursor_in_view(3));
    }

    #[test]
    fn undo_keeps_within_its_memory_budget() {
        let mut e = TextEditor::new();
        let big = "x".repeat(10 << 20);
        for _ in 0..20 {
            e.set_text(&big);
        }
        assert!(e.undo.bytes <= UNDO_BYTES && e.undo.steps.len() < 20);
        assert_eq!(e.undo.bytes, e.undo.steps.iter().map(Snapshot::bytes).sum::<usize>());
        // A single step bigger than the budget is still kept.
        let mut e = TextEditor::new();
        e.set_text(&"y".repeat(UNDO_BYTES + 1));
        e.set_text("small");
        assert!(e.undo() && e.text().starts_with('y'));
        // The step before it no longer fits.
        assert!(!e.undo());
        assert!(e.redo() && e.text() == "small");
    }

    #[test]
    fn undo_groups_typing_by_word() {
        let mut e = TextEditor::new();
        for c in "select 1".chars() {
            e.insert_char(c);
        }
        assert!(e.undo());
        assert_eq!(e.text(), "select ");
        assert!(e.undo());
        assert_eq!(e.text(), "select");
        assert!(e.undo());
        assert_eq!(e.text(), "");
        assert!(!e.undo());
        assert!(e.redo());
        assert!(e.redo());
        assert_eq!(e.text(), "select ");
    }

    #[test]
    fn moving_the_cursor_ends_a_typing_group() {
        let mut e = TextEditor::new();
        e.insert_char('a');
        e.prepare_move(false);
        e.move_left();
        e.insert_char('b');
        e.undo();
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut e = editor("x");
        e.insert_char('y');
        e.undo();
        e.insert_char('z');
        assert!(!e.redo());
        assert_eq!(e.text(), "xz");
    }

    #[test]
    fn set_text_is_undoable() {
        let mut e = editor("old");
        e.set_text("new");
        e.undo();
        assert_eq!(e.text(), "old");
    }

    #[test]
    fn shift_selection_and_replace() {
        let mut e = editor("hello world");
        e.prepare_move(true);
        e.move_word_left();
        assert_eq!(e.selected_text().as_deref(), Some("world"));
        e.insert_char('X');
        assert_eq!(e.text(), "hello X");
        e.undo();
        assert_eq!(e.text(), "hello world");
    }

    #[test]
    fn multiline_selection_cut_and_select_all() {
        let mut e = editor("ab\ncd\nef");
        e.move_doc_start();
        e.move_right();
        e.prepare_move(true);
        e.move_down();
        e.move_down();
        assert_eq!(e.selected_text().as_deref(), Some("b\ncd\ne"));
        assert_eq!(e.cut().as_deref(), Some("b\ncd\ne"));
        assert_eq!(e.text(), "af");
        e.select_all();
        assert_eq!(e.selected_text().as_deref(), Some("af"));
        e.backspace();
        assert!(e.is_empty());
    }

    #[test]
    fn plain_move_clears_selection() {
        let mut e = editor("abc");
        e.prepare_move(true);
        e.move_left();
        assert!(e.selection().is_some());
        e.prepare_move(false);
        e.move_left();
        assert!(e.selection().is_none());
    }

    #[test]
    fn word_movement() {
        let mut e = editor("select a.b,  c\nfrom");
        e.move_doc_start();
        e.move_word_right();
        assert_eq!(e.cursor_col, 6);
        e.move_word_right();
        assert_eq!(e.cursor_col, 8);
        e.move_word_right();
        assert_eq!(e.cursor_col, 10);
        e.move_word_right();
        e.move_word_right();
        assert_eq!((e.cursor_row, e.cursor_col), (1, 4));
        e.move_word_left();
        assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
        e.move_word_left();
        assert_eq!((e.cursor_row, e.cursor_col), (0, 13));
    }

    #[test]
    fn replace_before_cursor() {
        let mut e = editor("select us");
        e.replace_before_cursor(2, "users");
        assert_eq!(e.text(), "select users");
        assert_eq!(e.cursor_col, 12);
    }

    #[test]
    fn toggles_line_comments() {
        let mut ed = TextEditor::with_text("select 1\n  from t\n\nwhere x");
        ed.select_all();
        ed.toggle_comment();
        assert_eq!(ed.text(), "-- select 1\n--   from t\n\n-- where x");
        ed.toggle_comment();
        assert_eq!(ed.text(), "select 1\n  from t\n\nwhere x");
        ed.clear_selection();
        ed.set_cursor((1, 4));
        ed.toggle_comment();
        assert_eq!((ed.text().lines().nth(1).unwrap(), ed.cursor_col), ("  -- from t", 7));
        assert!(ed.undo());
        assert_eq!(ed.text(), "select 1\n  from t\n\nwhere x");
    }

    #[test]
    fn finds_matching_brackets() {
        let mut ed = TextEditor::with_text("f(a, (b), [c])");
        ed.set_cursor((0, 1));
        assert_eq!(ed.matching_bracket(), Some(((0, 1), (0, 13))));
        ed.set_cursor((0, 8));
        assert_eq!(ed.matching_bracket(), Some(((0, 7), (0, 5))));
        ed.set_cursor((0, 3));
        assert_eq!(ed.matching_bracket(), None);
    }
}
