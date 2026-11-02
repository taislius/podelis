//! Asking on the terminal, outside the TUI: a password or key typed
//! without being shown.

use std::io::Write as _;

/// Reads a password from the terminal without showing it; None if
/// cancelled (Esc, Ctrl+C) or the terminal can't be put in raw mode.
pub fn prompt_password(label: &str) -> Option<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    let mut err = std::io::stderr();
    let _ = write!(err, "{label}");
    let _ = err.flush();
    /// Leaves raw mode however the prompt ends, a panic included.
    struct Raw;
    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = writeln!(std::io::stderr());
        }
    }
    enable_raw_mode().ok()?;
    let raw = Raw;
    let mut password = String::new();
    let entered = loop {
        // The terminal went away (a dropped SSH session): give up.
        let Ok(event) = read() else { break false };
        let Event::Key(key) = event else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => break true,
            KeyCode::Esc => break false,
            KeyCode::Char('c' | 'd') if ctrl => break false,
            KeyCode::Char('u' | 'w') if ctrl => password.clear(),
            // Other Ctrl/Alt keys aren't typing: never part of the password.
            KeyCode::Char(_) if ctrl || key.modifiers.contains(KeyModifiers::ALT) => {}
            KeyCode::Char(c) => password.push(c),
            KeyCode::Backspace => {
                password.pop();
            }
            _ => {}
        }
    };
    drop(raw);
    (entered && !password.is_empty()).then_some(password)
}
