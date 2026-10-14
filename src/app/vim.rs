//! Vim-style modal editing for the SQL editor (`editor_mode = "vim"`).
//! Covers the everyday subset: counts, motions, operators with motions,
//! `iw`, visual modes, registers for yank/paste. Redo stays on Ctrl+Y
//! because Ctrl+R runs the query.

use super::*;
use crate::editor::{Pos, is_word_char};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl VimMode {
    pub fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
        }
    }
}

#[derive(Default)]
pub struct Vim {
    pub mode: VimMode,
    /// Typed count and operator so far, e.g. "2d".
    pending: String,
    /// Last yanked/deleted text, and whether it was whole lines.
    register: (String, bool),
}

/// Character classes for word motions.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: Option<char>) -> Class {
    match c {
        None => Class::Space,
        Some(c) if c.is_whitespace() => Class::Space,
        Some(c) if is_word_char(c) => Class::Word,
        Some(_) => Class::Punct,
    }
}

/// Steps one char forward/back across line ends; `None` at the text's edge.
/// A line end counts as whitespace.
fn step(ed: &TextEditor, (r, c): Pos, forward: bool) -> Option<Pos> {
    if forward {
        if c < ed.line_len(r) {
            Some((r, c + 1))
        } else if r + 1 < ed.lines.len() {
            Some((r + 1, 0))
        } else {
            None
        }
    } else if c > 0 {
        Some((r, c - 1))
    } else if r > 0 {
        Some((r - 1, ed.line_len(r - 1)))
    } else {
        None
    }
}

/// `w`: start of the next word.
fn word_start(ed: &TextEditor, mut p: Pos) -> Pos {
    let start = class(ed.char_at_pos(p));
    while start != Class::Space && class(ed.char_at_pos(p)) == start {
        match step(ed, p, true) {
            Some(n) => p = n,
            None => return p,
        }
    }
    while class(ed.char_at_pos(p)) == Class::Space {
        match step(ed, p, true) {
            Some(n) => p = n,
            None => return p,
        }
    }
    p
}

/// `e`: end of the current/next word (on its last char).
fn word_end(ed: &TextEditor, p: Pos) -> Pos {
    let Some(mut p) = step(ed, p, true) else { return p };
    while class(ed.char_at_pos(p)) == Class::Space {
        match step(ed, p, true) {
            Some(n) => p = n,
            None => return p,
        }
    }
    let cls = class(ed.char_at_pos(p));
    while let Some(n) = step(ed, p, true) {
        if class(ed.char_at_pos(n)) != cls {
            break;
        }
        p = n;
    }
    p
}

/// `b`: start of the current/previous word.
fn word_back(ed: &TextEditor, p: Pos) -> Pos {
    let Some(mut p) = step(ed, p, false) else { return p };
    while class(ed.char_at_pos(p)) == Class::Space {
        match step(ed, p, false) {
            Some(n) => p = n,
            None => return p,
        }
    }
    let cls = class(ed.char_at_pos(p));
    while let Some(n) = step(ed, p, false) {
        if class(ed.char_at_pos(n)) != cls {
            break;
        }
        p = n;
    }
    p
}

fn first_non_blank(ed: &TextEditor, row: usize) -> Pos {
    let col = ed.lines[row].chars().take_while(|c| c.is_whitespace()).count();
    (row, col.min(ed.line_len(row)))
}

/// The range of the word under the cursor (`iw`), end exclusive.
fn inner_word(ed: &TextEditor, (r, c): Pos) -> (Pos, Pos) {
    let chars: Vec<char> = ed.lines[r].chars().collect();
    if chars.is_empty() {
        return ((r, 0), (r, 0));
    }
    let c = c.min(chars.len() - 1);
    let cls = class(Some(chars[c]));
    let mut a = c;
    while a > 0 && class(Some(chars[a - 1])) == cls {
        a -= 1;
    }
    let mut b = c + 1;
    while b < chars.len() && class(Some(chars[b])) == cls {
        b += 1;
    }
    ((r, a), (r, b))
}

/// Where a motion key takes the cursor, or `None` if it isn't a motion.
/// The bool says whether the motion's end char is included (`e`, `$`).
fn motion(ed: &TextEditor, key: char, from: Pos, count: usize) -> Option<(Pos, bool)> {
    let mut p = from;
    let mut inclusive = false;
    for _ in 0..count.max(1) {
        p = match key {
            'h' => (p.0, p.1.saturating_sub(1)),
            'l' => (p.0, (p.1 + 1).min(ed.line_len(p.0))),
            'j' => (p.0 + 1, p.1),
            'k' => (p.0.saturating_sub(1), p.1),
            'w' => word_start(ed, p),
            'b' => word_back(ed, p),
            'e' => {
                inclusive = true;
                word_end(ed, p)
            }
            '0' => (p.0, 0),
            '^' => first_non_blank(ed, p.0),
            '$' => {
                inclusive = false;
                (p.0, ed.line_len(p.0))
            }
            'G' => (ed.lines.len() - 1, 0),
            _ => return None,
        };
    }
    let p = (p.0.min(ed.lines.len() - 1), p.1.min(ed.line_len(p.0.min(ed.lines.len() - 1))));
    Some((p, inclusive))
}

impl App {
    pub fn vim_enabled(&self) -> bool {
        self.settings.editor_mode == EditorMode::Vim
    }

    /// The vim mode to show, when vim editing is on.
    pub fn vim_mode(&self) -> Option<VimMode> {
        self.vim_enabled().then_some(self.vim.mode)
    }

    /// Keys for the editor in vim mode.
    pub(super) fn handle_vim_key(&mut self, key: KeyEvent) {
        match self.vim.mode {
            VimMode::Insert => {
                if key.code == KeyCode::Esc {
                    self.vim.mode = VimMode::Normal;
                    self.tab.editor.prepare_move(false);
                    self.tab.editor.move_left();
                    self.completion = None;
                } else {
                    // Everything else edits like the standard editor.
                    self.handle_standard_editor_key(key);
                }
            }
            VimMode::Normal => self.vim_normal(key),
            VimMode::Visual | VimMode::VisualLine => self.vim_visual(key),
        }
    }

    fn take_count(&mut self) -> usize {
        let digits: String = self.vim.pending.chars().take_while(char::is_ascii_digit).collect();
        self.vim.pending.drain(..digits.len());
        digits.parse().unwrap_or(1)
    }

    fn vim_insert(&mut self) {
        self.vim.mode = VimMode::Insert;
        self.vim.pending.clear();
    }

    fn vim_normal(&mut self, key: KeyEvent) {
        let KeyCode::Char(ch) = key.code else {
            // Arrows, Home/End etc. behave as in the standard editor.
            if key.code == KeyCode::Esc {
                self.vim.pending.clear();
            } else {
                self.handle_standard_editor_key(key);
            }
            return;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            // Ctrl+V pastes the system clipboard, as everywhere (no block mode).
            if ch == 'v' {
                self.paste_clipboard();
            }
            return;
        }
        // Space is the leader, as in Neovim: Ctrl+K's menu.
        if ch == ' ' && self.vim.pending.is_empty() {
            return self.open_palette(false);
        }
        // Counts: 1-9, then any digit (a lone 0 is a motion).
        if ch.is_ascii_digit() && (ch != '0' || self.vim.pending.chars().last().is_some_and(|c| c.is_ascii_digit())) {
            self.vim.pending.push(ch);
            return;
        }
        let operator = self.vim.pending.chars().find(|c| matches!(c, 'd' | 'c' | 'y'));
        let pending_g = self.vim.pending.ends_with('g');
        let pending_r = self.vim.pending.ends_with('r');
        let pending_i = self.vim.pending.ends_with('i');
        if let Some(op) = operator {
            return self.vim_operator(op, ch, pending_i);
        }
        if pending_r {
            self.vim.pending.clear();
            let p = self.tab.editor.cursor();
            if self.tab.editor.char_at_pos(p).is_some() {
                self.tab.editor.set_anchor(Some(p));
                self.tab.editor.set_cursor((p.0, p.1 + 1));
                self.tab.editor.insert_char(ch);
                self.tab.editor.set_cursor(p);
                self.edited();
            }
            return;
        }
        // Operators and prefixes wait for the next key; the count stays
        // pending with them.
        if matches!(ch, 'd' | 'c' | 'y' | 'r') || (ch == 'g' && !pending_g) {
            self.vim.pending.push(ch);
            return;
        }
        let count = self.take_count();
        if self.vim_insert_command(ch) || self.vim_edit_command(ch, count) {
            return;
        }
        let ed = &mut self.tab.editor;
        match ch {
            'g' if pending_g => {
                self.vim.pending.clear();
                ed.prepare_move(false);
                ed.set_cursor((count.saturating_sub(1).min(ed.lines.len() - 1), 0));
            }
            'G' if count > 1 => {
                ed.prepare_move(false);
                ed.set_cursor((count - 1, 0));
            }
            'v' => {
                self.vim.mode = VimMode::Visual;
                let p = ed.cursor();
                ed.set_anchor(Some(p));
            }
            'V' => {
                self.vim.mode = VimMode::VisualLine;
                let p = ed.cursor();
                ed.set_anchor(Some(p));
            }
            _ => {
                if let Some((p, _)) = motion(ed, ch, ed.cursor(), count) {
                    ed.prepare_move(false);
                    ed.set_cursor(p);
                }
            }
        }
    }

    /// `i a I A o O`: enter Insert mode somewhere. True if `ch` was one.
    fn vim_insert_command(&mut self, ch: char) -> bool {
        let ed = &mut self.tab.editor;
        match ch {
            'i' => self.vim_insert(),
            'a' => {
                ed.prepare_move(false);
                ed.move_right();
                self.vim_insert();
            }
            'I' => {
                let p = first_non_blank(ed, ed.cursor_row);
                ed.prepare_move(false);
                ed.set_cursor(p);
                self.vim_insert();
            }
            'A' => {
                ed.prepare_move(false);
                ed.move_end();
                self.vim_insert();
            }
            'o' => {
                ed.prepare_move(false);
                ed.move_end();
                ed.insert_newline();
                self.edited();
                self.vim_insert();
            }
            'O' => {
                ed.prepare_move(false);
                ed.move_home();
                ed.insert_newline();
                ed.move_up();
                self.edited();
                self.vim_insert();
            }
            _ => return false,
        }
        true
    }

    /// `x X D C p P u J ~`: edits in Normal mode. True if `ch` was one.
    fn vim_edit_command(&mut self, ch: char, count: usize) -> bool {
        let ed = &mut self.tab.editor;
        match ch {
            'x' | 'X' => {
                let p = ed.cursor();
                let (a, b) = if ch == 'x' {
                    (p, (p.0, (p.1 + count).min(ed.line_len(p.0))))
                } else {
                    ((p.0, p.1.saturating_sub(count)), p)
                };
                self.vim_cut(a, b, false);
            }
            'D' => {
                let p = ed.cursor();
                let end = (p.0, ed.line_len(p.0));
                self.vim_cut(p, end, false);
            }
            'C' => {
                let p = ed.cursor();
                let end = (p.0, ed.line_len(p.0));
                self.vim_cut(p, end, false);
                self.vim_insert();
            }
            'p' | 'P' => self.vim_paste(ch == 'p', count),
            'u' => {
                for _ in 0..count {
                    self.tab.editor.undo();
                }
                self.edited();
            }
            'J' => {
                for _ in 0..count {
                    let row = self.tab.editor.cursor_row;
                    if row + 1 >= self.tab.editor.lines.len() {
                        break;
                    }
                    let next_blank = self.tab.editor.lines[row + 1].chars().take_while(|c| c.is_whitespace()).count();
                    let end = self.tab.editor.line_len(row);
                    self.tab.editor.set_anchor(Some((row, end)));
                    self.tab.editor.set_cursor((row + 1, next_blank));
                    self.tab.editor.insert_char(' ');
                    self.tab.editor.set_cursor((row, end));
                }
                self.edited();
            }
            '~' => {
                let p = ed.cursor();
                if let Some(c) = ed.char_at_pos(p) {
                    let toggled: String =
                        if c.is_uppercase() { c.to_lowercase().collect() } else { c.to_uppercase().collect() };
                    ed.set_anchor(Some(p));
                    ed.set_cursor((p.0, p.1 + 1));
                    ed.insert_str(&toggled);
                    self.edited();
                }
            }
            _ => return false,
        }
        true
    }

    /// `d`/`c`/`y` followed by a motion, the same key again (whole lines)
    /// or `iw`.
    fn vim_operator(&mut self, op: char, ch: char, pending_i: bool) {
        let count = {
            let digits: String = self.vim.pending.chars().filter(char::is_ascii_digit).collect();
            digits.parse::<usize>().unwrap_or(1)
        };
        let ed = &self.tab.editor;
        let from = ed.cursor();
        let range = if pending_i {
            (ch == 'w').then(|| inner_word(ed, from)).map(|(a, b)| (a, b, false))
        } else if ch == 'i' {
            self.vim.pending.push('i');
            return;
        } else if ch == op {
            // dd / cc / yy: `count` whole lines.
            let last = (from.0 + count - 1).min(ed.lines.len() - 1);
            Some(((from.0, 0), (last, ed.line_len(last)), true))
        } else if ch == 'w' && op == 'c' {
            // `cw` changes to the end of the word, like `ce`.
            motion(ed, 'e', from, count).map(|(to, _)| (from, (to.0, to.1 + 1), false))
        } else {
            motion(ed, ch, from, count).map(|(to, inclusive)| {
                let (a, b) = if to < from { (to, from) } else { (from, to) };
                let b = if inclusive { (b.0, (b.1 + 1).min(ed.line_len(b.0))) } else { b };
                (a, b, false)
            })
        };
        self.vim.pending.clear();
        let Some((a, b, linewise)) = range else { return };
        match op {
            'y' => {
                self.vim_yank(a, b, linewise);
                self.yank_to_clipboard();
                self.tab.editor.prepare_move(false);
                self.tab.editor.set_cursor(a);
            }
            'd' => self.vim_cut(a, b, linewise),
            _ => {
                // `cc` keeps the (now empty) line to type into.
                self.vim_cut_keep_line(a, b, linewise);
                self.vim_insert();
            }
        }
    }

    fn vim_yank(&mut self, a: Pos, b: Pos, linewise: bool) {
        self.tab.editor.set_anchor(Some(a));
        self.tab.editor.set_cursor(b);
        let text = self.tab.editor.selected_text().unwrap_or_default();
        self.tab.editor.set_anchor(None);
        self.vim.register = (if linewise { text + "\n" } else { text }, linewise);
    }

    /// A yank also goes to the system clipboard (as vim's `unnamedplus`),
    /// quietly; deletes stay in the register.
    fn yank_to_clipboard(&mut self) {
        self.clipboard = Some(self.vim.register.0.clone());
    }

    /// Deletes `a..b` into the register. Whole-line deletes also remove a
    /// line break so the lines disappear.
    fn vim_cut(&mut self, a: Pos, b: Pos, linewise: bool) {
        self.vim_yank(a, b, linewise);
        let ed = &mut self.tab.editor;
        let (a, b) = if linewise {
            if b.0 + 1 < ed.lines.len() {
                (a, (b.0 + 1, 0))
            } else if a.0 > 0 {
                ((a.0 - 1, ed.line_len(a.0 - 1)), b)
            } else {
                (a, b)
            }
        } else {
            (a, b)
        };
        if a != b {
            ed.set_anchor(Some(a));
            ed.set_cursor(b);
            ed.cut();
        }
        if linewise {
            let row = ed.cursor_row.min(ed.lines.len() - 1);
            let p = first_non_blank(ed, if a.0 < ed.lines.len() && b.0 > a.0 { a.0 } else { row });
            ed.set_cursor(p);
        }
        self.edited();
    }

    fn vim_cut_keep_line(&mut self, a: Pos, b: Pos, linewise: bool) {
        self.vim_yank(a, b, linewise);
        if a != b {
            self.tab.editor.set_anchor(Some(a));
            self.tab.editor.set_cursor(b);
            self.tab.editor.cut();
        }
        self.edited();
    }

    /// `p` / `P`: whole lines go below/above; other text after/before the cursor.
    fn vim_paste(&mut self, after: bool, count: usize) {
        let (text, linewise) = self.vim.register.clone();
        if text.is_empty() {
            return;
        }
        let text = text.repeat(count);
        let ed = &mut self.tab.editor;
        ed.prepare_move(false);
        if linewise {
            let body = text.strip_suffix('\n').unwrap_or(&text);
            if after {
                ed.move_end();
                ed.insert_str(&format!("\n{body}"));
                let row = ed.cursor_row + 1 - body.matches('\n').count() - 1;
                let p = first_non_blank(ed, row);
                ed.set_cursor(p);
            } else {
                ed.move_home();
                let row = ed.cursor_row;
                ed.insert_str(&format!("{body}\n"));
                let p = first_non_blank(ed, row);
                ed.set_cursor(p);
            }
        } else {
            if after && ed.char_at_pos(ed.cursor()).is_some() {
                ed.move_right();
            }
            ed.insert_str(&text);
            ed.move_left();
        }
        self.edited();
    }

    fn vim_visual(&mut self, key: KeyEvent) {
        let KeyCode::Char(ch) = key.code else {
            if key.code == KeyCode::Esc {
                self.vim.mode = VimMode::Normal;
                self.tab.editor.set_anchor(None);
            } else {
                // Arrows extend the selection.
                let anchor = self.tab.editor.anchor();
                self.handle_standard_editor_key(key);
                self.tab.editor.set_anchor(anchor);
            }
            return;
        };
        if ch.is_ascii_digit() && ch != '0' {
            self.vim.pending.push(ch);
            return;
        }
        let anchor = self.tab.editor.anchor().unwrap_or_else(|| self.tab.editor.cursor());
        let cursor = self.tab.editor.cursor();
        let (a, b) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
        let line_mode = self.vim.mode == VimMode::VisualLine;
        // Visual selections include the char under the cursor.
        let (a, b) = if line_mode {
            ((a.0, 0), (b.0, self.tab.editor.line_len(b.0)))
        } else {
            (a, (b.0, (b.1 + 1).min(self.tab.editor.line_len(b.0))))
        };
        match ch {
            'y' | 'd' | 'x' | 'c' => {
                self.vim.mode = VimMode::Normal;
                match ch {
                    'y' => {
                        self.vim_yank(a, b, line_mode);
                        self.yank_to_clipboard();
                        self.tab.editor.set_cursor(a);
                    }
                    'c' => {
                        self.vim_cut_keep_line(a, b, line_mode);
                        self.vim_insert();
                    }
                    _ => self.vim_cut(a, b, line_mode),
                }
            }
            'v' | 'V' => {
                let target = if ch == 'v' { VimMode::Visual } else { VimMode::VisualLine };
                if self.vim.mode == target {
                    self.vim.mode = VimMode::Normal;
                    self.tab.editor.set_anchor(None);
                } else {
                    self.vim.mode = target;
                }
            }
            _ => {
                let count = self.take_count();
                if let Some((p, _)) = motion(&self.tab.editor, ch, cursor, count) {
                    self.tab.editor.set_cursor(p);
                    self.tab.editor.set_anchor(Some(anchor));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vim_app(text: &str) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
        let mut app = App::new(store, None, None);
        app.settings.editor_mode = EditorMode::Vim;
        app.screen = Screen::Query;
        app.focus = Focus::Editor;
        app.tab.editor = TextEditor::with_text(text);
        app.tab.editor.set_cursor((0, 0));
        (app, dir)
    }

    fn keys(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = if c == '\u{1b}' { KeyCode::Esc } else { KeyCode::Char(c) };
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    #[test]
    fn yanks_reach_the_system_clipboard_and_deletes_dont() {
        let (mut app, _dir) = vim_app("select 1\nfrom t");
        keys(&mut app, "yy");
        assert_eq!(app.take_clipboard().as_deref(), Some("select 1\n"));
        keys(&mut app, "vey");
        assert_eq!(app.take_clipboard().as_deref(), Some("select"));
        keys(&mut app, "dd");
        assert!(app.take_clipboard().is_none(), "a delete stays in the register");
        keys(&mut app, "p");
        assert_eq!(app.tab.editor.text(), "from t\nselect 1");
    }

    #[test]
    fn space_in_normal_mode_opens_the_ctrl_k_menu() {
        let (mut app, _dir) = vim_app("select 1");
        keys(&mut app, " ");
        assert!(app.palette.as_ref().is_some_and(|p| !p.searching));
        keys(&mut app, "\u{1b}i ");
        assert!(app.palette.is_none() && app.tab.editor.text() == " select 1", "Insert mode types it");
    }

    fn run(text: &str, input: &str) -> (String, Pos, VimMode) {
        let (mut app, _dir) = vim_app(text);
        keys(&mut app, input);
        (app.tab.editor.text(), app.tab.editor.cursor(), app.vim.mode)
    }

    #[test]
    fn motions() {
        let (mut app, _dir) = vim_app("select a.b, c\nfrom t");
        keys(&mut app, "w");
        assert_eq!(app.tab.editor.cursor(), (0, 7));
        keys(&mut app, "w");
        assert_eq!(app.tab.editor.cursor(), (0, 8)); // the "."
        keys(&mut app, "e");
        assert_eq!(app.tab.editor.cursor(), (0, 9));
        keys(&mut app, "$");
        assert_eq!(app.tab.editor.cursor(), (0, 13));
        keys(&mut app, "0j");
        assert_eq!(app.tab.editor.cursor(), (1, 0));
        keys(&mut app, "gg");
        assert_eq!(app.tab.editor.cursor(), (0, 0));
        keys(&mut app, "G");
        assert_eq!(app.tab.editor.cursor(), (1, 0));
        keys(&mut app, "b");
        assert_eq!(app.tab.editor.cursor(), (0, 12));
    }

    #[test]
    fn inserting() {
        assert_eq!(run("ab", "ix\u{1b}").0, "xab");
        assert_eq!(run("ab", "ax\u{1b}").0, "axb");
        assert_eq!(run("  ab", "AZ\u{1b}Iy\u{1b}").0, "  yabZ");
        assert_eq!(run("a\nb", "onew\u{1b}").0, "a\nnew\nb");
        assert_eq!(run("a\nb", "Onew\u{1b}").0, "new\na\nb");
        let (_, cursor, mode) = run("ab", "Ax\u{1b}");
        assert_eq!((cursor, mode), ((0, 2), VimMode::Normal));
    }

    #[test]
    fn deleting_and_changing() {
        assert_eq!(run("select from", "dw").0, "from");
        assert_eq!(run("select from", "2x").0, "lect from");
        assert_eq!(run("select from", "wD").0, "select ");
        assert_eq!(run("a\nb\nc", "jdd").0, "a\nc");
        assert_eq!(run("a\nb\nc", "2dd").0, "c");
        assert_eq!(run("a\nb\nc", "Gdd").0, "a\nb");
        assert_eq!(run("select from", "cwSELECT\u{1b}").0, "SELECT from");
        assert_eq!(run("one two three", "wciwX\u{1b}").0, "one X three");
        assert_eq!(run("one two", "d$").0, "");
        assert_eq!(run("one two", "wcc").0, "");
    }

    #[test]
    fn yank_paste_and_undo() {
        assert_eq!(run("a\nb", "yyp").0, "a\na\nb");
        assert_eq!(run("a\nb", "jyyP").0, "a\nb\nb");
        assert_eq!(run("one two", "ywP").0, "one one two");
        assert_eq!(run("ab", "xp").0, "ba");
        assert_eq!(run("one two", "dwu").0, "one two");
    }

    #[test]
    fn other_commands() {
        assert_eq!(run("a\n   b", "J").0, "a b");
        assert_eq!(run("abc", "rX").0, "Xbc");
        assert_eq!(run("abc", "~~").0, "ABc");
    }

    #[test]
    fn visual_modes() {
        assert_eq!(run("one two", "vey").0, "one two");
        assert_eq!(run("one two", "ved").0, " two");
        assert_eq!(run("a\nb\nc", "Vjd").0, "c");
        assert_eq!(run("one two", "wvecX\u{1b}").0, "one X");
        let (mut app, _dir) = vim_app("abc");
        keys(&mut app, "vl");
        assert_eq!(app.vim.mode, VimMode::Visual);
        assert_eq!(app.tab.editor.selected_text().as_deref(), Some("a"));
        keys(&mut app, "\u{1b}");
        assert!(app.tab.editor.selection().is_none() && app.vim.mode == VimMode::Normal);
    }

    #[test]
    fn standard_mode_is_unaffected() {
        let (mut app, _dir) = vim_app("");
        app.settings.editor_mode = EditorMode::Standard;
        keys(&mut app, "dd");
        assert_eq!(app.tab.editor.text(), "dd");
    }
}
