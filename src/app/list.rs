//! Moving the selection in a list, the same way in every list: ↑/↓,
//! PageUp/PageDown, and in lists that don't take typing j/k and Home/End.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::key_table::{Move, movement};

/// How a list moves.
#[derive(Clone, Copy, Debug)]
pub struct Nav {
    /// j/k and Home/End move too (the list doesn't take typing).
    pub letters: bool,
    /// Past either end goes round to the other.
    pub wrap: bool,
    /// Tab and Shift+Tab move like ↓ and ↑.
    pub tab: bool,
}

impl Nav {
    /// A list of things to pick: projects, connections, sessions.
    pub const LIST: Nav = Nav { letters: true, wrap: false, tab: false };
    /// A list under a search box (palette, library).
    pub const SEARCHED: Nav = Nav { letters: false, wrap: false, tab: false };
    /// A short menu that goes round.
    pub const MENU: Nav = Nav { letters: true, wrap: true, tab: true };
    /// The completion popup: goes round, and Tab is for accepting.
    pub const COMPLETION: Nav = Nav { letters: false, wrap: true, tab: false };
    /// A form's fields.
    pub const FIELDS: Nav = Nav { letters: false, wrap: true, tab: true };
}

/// How far PageUp/PageDown move.
const PAGE: usize = 10;

/// Where the selection in a list of `len` goes for `key`, or `None` if
/// `key` doesn't move it.
pub fn step(selected: usize, len: usize, key: KeyEvent, nav: Nav) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let down = |s: usize| if nav.wrap && s >= last { 0 } else { (s + 1).min(last) };
    let up = |s: usize| if nav.wrap && s == 0 { last } else { s.saturating_sub(1) };
    // A letter (`j`, `g`…) moves only a list that doesn't take typing, and
    // only plain; the arrows and page keys with Ctrl or Alt too (some
    // terminals send them so).
    let step = if matches!(key.code, KeyCode::Char(_)) {
        if !nav.letters {
            return None;
        }
        movement(key)
    } else {
        movement(KeyEvent::new(key.code, crossterm::event::KeyModifiers::NONE))
    };
    Some(match (key.code, step) {
        (KeyCode::Tab, _) if nav.tab => down(selected),
        (KeyCode::BackTab, _) if nav.tab => up(selected),
        (_, Some(Move::Down)) => down(selected),
        (_, Some(Move::Up)) => up(selected),
        (_, Some(Move::PageDown)) => (selected + PAGE).min(last),
        (_, Some(Move::PageUp)) => selected.saturating_sub(PAGE),
        (_, Some(Move::Home)) if nav.letters => 0,
        (_, Some(Move::End)) if nav.letters => last,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn lists_stop_at_the_ends_and_menus_go_round() {
        assert_eq!(step(0, 3, key(KeyCode::Up), Nav::LIST), Some(0));
        assert_eq!(step(2, 3, key(KeyCode::Char('j')), Nav::LIST), Some(2));
        assert_eq!(step(2, 3, key(KeyCode::Down), Nav::MENU), Some(0));
        assert_eq!(step(0, 3, key(KeyCode::BackTab), Nav::MENU), Some(2));
        assert_eq!(step(1, 30, key(KeyCode::PageDown), Nav::SEARCHED), Some(11));
        assert_eq!(step(5, 30, key(KeyCode::End), Nav::LIST), Some(29));
    }

    #[test]
    fn arrows_move_with_ctrl_or_alt_too_and_letters_only_plain() {
        use crossterm::event::KeyModifiers;
        let with = |code, m| KeyEvent::new(code, m);
        assert_eq!(step(0, 3, with(KeyCode::Down, KeyModifiers::CONTROL), Nav::LIST), Some(1));
        assert_eq!(step(0, 30, with(KeyCode::PageDown, KeyModifiers::ALT), Nav::LIST), Some(10));
        assert_eq!(step(0, 3, with(KeyCode::Char('j'), KeyModifiers::CONTROL), Nav::LIST), None);
    }

    #[test]
    fn typing_lists_leave_letters_and_home_end_alone() {
        assert_eq!(step(1, 3, key(KeyCode::Char('j')), Nav::SEARCHED), None);
        assert_eq!(step(1, 3, key(KeyCode::Home), Nav::SEARCHED), None);
        assert_eq!(step(1, 3, key(KeyCode::Tab), Nav::COMPLETION), None);
        assert_eq!(step(0, 0, key(KeyCode::Down), Nav::LIST), None);
    }
}
