//! The MESSAGES tab as a list: ↑/↓ select a message, `Enter` shows all
//! of it, `y` (or Ctrl+C) copies it and `Y` copies them all. A click
//! selects one too, a double click opens it.

use super::*;

impl App {
    /// A key in MESSAGES; false leaves it to scrolling (←/→ across).
    pub(super) fn handle_messages_key(&mut self, key: KeyEvent) -> bool {
        let n = self.messages.len();
        if n == 0 {
            return false;
        }
        let page = usize::from(self.hits.messages.height.max(2) - 1);
        // From the newest, where the list starts.
        let at = self.message_selected.unwrap_or(n);
        let select = match key.code {
            KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (at + 1).min(n - 1),
            KeyCode::PageUp => at.saturating_sub(page),
            KeyCode::PageDown => (at + page).min(n - 1),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => n - 1,
            KeyCode::Enter | KeyCode::Char('v') => {
                self.open_message();
                return true;
            }
            KeyCode::Char('y') => {
                self.copy_message();
                return true;
            }
            KeyCode::Char('Y') => {
                let all: Vec<String> = self.messages.iter().map(|m| format!("{}  {}", m.time, m.text)).collect();
                self.copy_text(all.join("\n"), &format!("Copied {n} messages"));
                return true;
            }
            KeyCode::Esc if self.message_selected.is_some() => {
                self.message_selected = None;
                return true;
            }
            _ => return false,
        };
        self.message_selected = Some(select);
        self.reveal_message = true;
        true
    }

    /// The selected message (else the newest), whole.
    fn chosen_message(&self) -> Option<&Message> {
        self.message_selected.and_then(|i| self.messages.get(i)).or(self.messages.last())
    }

    pub(super) fn copy_message(&mut self) {
        let Some(m) = self.chosen_message() else { return };
        let text = m.text.clone();
        self.copy_text(text, "Copied the message");
    }

    fn open_message(&mut self) {
        let Some(m) = self.chosen_message() else { return };
        let title = format!("MESSAGE · {}{}", m.time, if m.is_error { " · error" } else { "" });
        let view = CellView { copies_text: Some("Copied the message"), ..CellView::new(title, m.text.clone()) };
        self.cell_view = Some(view);
    }

    /// A click on MESSAGES line `y`: selects its message; twice opens it.
    pub(super) fn click_message(&mut self, y: u16, double: bool) {
        let i = self.hits.messages_offset + usize::from(y - self.hits.messages.y);
        if i >= self.messages.len() {
            return;
        }
        self.message_selected = Some(i);
        if double {
            self.open_message();
        }
    }
}
