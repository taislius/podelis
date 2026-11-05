//! The MESSAGES tab as a list: ↑/↓ select a message, `Enter` shows all
//! of it, `y` (or Ctrl+C) copies it and `Y` copies them all. A click
//! selects one too, a double click opens it.

use super::*;

/// What's said to the user: the status line, the editor's and the
/// results' footer notes, and the MESSAGES list.
#[derive(Default)]
pub struct Notes {
    /// The status line's message, and whether it's an error.
    pub status: String,
    pub status_is_error: bool,
    /// The status is about a run (its outcome or progress): the results
    /// pane shows it, not the status bar.
    pub status_in_results: bool,
    /// A note about typing (e.g. "No completions") in the editor's footer,
    /// next to where you type; gone with the next key.
    pub editor_note: Option<String>,
    pub editor_note_is_error: bool,
    /// The same for the results pane, with whether it's an error.
    pub results_note: Option<(String, bool)>,
    /// Outcomes and errors of recent runs, oldest first.
    pub log: Vec<Message>,
    /// The message selected in MESSAGES (`messages.rs`), and whether the
    /// next drawing scrolls it into view.
    pub selected: Option<usize>,
    pub reveal: bool,
    /// What following a foreign key said it waits for ("Looking up…"),
    /// cleared from the results pane when the row comes.
    pub(super) following: Option<String>,
}

impl App {
    /// A key in MESSAGES; false leaves it to scrolling (←/→ across).
    pub(super) fn handle_messages_key(&mut self, key: KeyEvent) -> bool {
        let n = self.notes.log.len();
        if n == 0 {
            return false;
        }
        let page = usize::from(self.hits.messages.height.max(2) - 1);
        // From the newest, where the list starts.
        let at = self.notes.selected.unwrap_or(n);
        let select = match key.code {
            KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (at + 1).min(n - 1),
            KeyCode::PageUp => at.saturating_sub(page),
            KeyCode::PageDown => (at + page).min(n - 1),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => n - 1,
            KeyCode::Esc if self.notes.selected.is_some() => {
                self.notes.selected = None;
                return true;
            }
            _ => return false,
        };
        self.notes.selected = Some(select);
        self.notes.reveal = true;
        true
    }

    /// The selected message (else the newest), whole.
    fn chosen_message(&self) -> Option<&Message> {
        self.notes.selected.and_then(|i| self.notes.log.get(i)).or(self.notes.log.last())
    }

    /// Every message, each with its time.
    pub(super) fn copy_messages(&mut self) {
        let all: Vec<String> = self.notes.log.iter().map(|m| format!("{}  {}", m.time, m.text)).collect();
        let n = all.len();
        self.copy_text(all.join("\n"), &tn!("copy.messages", n));
    }

    pub(super) fn copy_message(&mut self) {
        let Some(m) = self.chosen_message() else { return };
        let text = m.text.clone();
        self.copy_text(text, t!("copy.message"));
    }

    pub(super) fn open_message(&mut self) {
        let Some(m) = self.chosen_message() else { return };
        let error = if m.is_error { format!(" · {}", t!("messages.error")) } else { String::new() };
        let title = format!("{} · {}{error}", t!("messages.title"), m.time);
        let view = CellView { copies_text: Some(t!("copy.message")), ..CellView::new(title, m.text.clone()) };
        self.popups.cell_view = Some(view);
    }

    /// What a pane's footer says now.
    fn footer_message(&self) -> Option<&str> {
        let run = (self.notes.status_in_results && !self.notes.status.is_empty()).then_some(self.notes.status.as_str());
        self.notes.editor_note.as_deref().or(self.notes.results_note.as_ref().map(|(m, _)| m.as_str())).or(run)
    }

    /// Every message whole, the newest first: a footer's may be cut off.
    /// What a footer says now comes first if the list hasn't got it (a run's
    /// progress, say).
    pub(super) fn open_messages(&mut self) {
        let logged = |text: &str| self.notes.log.iter().any(|m| m.text == text);
        let shown = self.footer_message().filter(|m| !logged(m)).map(str::to_string);
        let all: Vec<String> =
            shown.into_iter().chain(self.notes.log.iter().rev().map(|m| format!("{}  {}", m.time, m.text))).collect();
        let text = if all.is_empty() { t!("results.no_messages").to_string() } else { all.join("\n\n") };
        let title = t!("results.messages").to_string();
        self.popups.cell_view =
            Some(CellView { copies_text: Some(t!("messages.copied")), ..CellView::new(title, text) });
    }

    /// A click on MESSAGES line `y`: selects its message; twice opens it.
    pub(super) fn click_message(&mut self, y: u16, double: bool) {
        let i = self.hits.messages_offset + usize::from(y - self.hits.messages.y);
        if i >= self.notes.log.len() {
            return;
        }
        self.notes.selected = Some(i);
        if double {
            self.open_message();
        }
    }
}
