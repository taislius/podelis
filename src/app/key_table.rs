//! Keys as data: a screen or window lists its keys once, in a table of
//! `(key, what it does)`, and its handler, its hints and its help all read
//! that table, so a key is written in one place. The query screen's panes
//! have theirs in `commands.rs`; moving (arrows and vim's `hjkl`) is
//! `MOVE_KEYS`, for every list and text view.

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::commands::{is_plain, key_label};

/// Keys and what they do. A command's first key is the one its hint shows.
pub type Table<C> = &'static [(KeyCode, C)];

/// What `key` does in `table`: a plain key only (Ctrl+D isn't `d`).
pub fn lookup<C: Copy>(table: Table<C>, key: KeyEvent) -> Option<C> {
    if !is_plain(key) {
        return None;
    }
    table.iter().find(|(code, _)| *code == key.code).map(|&(_, c)| c)
}

/// The first key of `command`, for its hint.
pub fn first_key<C: PartialEq + Copy>(table: Table<C>, command: C) -> String {
    table.iter().find(|(_, c)| *c == command).map(|&(code, _)| key_label(code)).unwrap_or_default()
}

/// Every key of each of `commands`, for help: `e · → · l`.
pub fn all_keys<C: PartialEq + Copy>(table: Table<C>, commands: &[C]) -> String {
    let labels: Vec<String> =
        table.iter().filter(|(_, c)| commands.contains(c)).map(|&(code, _)| key_label(code)).collect();
    labels.join(" · ")
}

/// A hint: `command`'s first key, then what it does (`y copy`).
pub fn hint<C: PartialEq + Copy>(table: Table<C>, command: C, what: &str) -> String {
    format!("{} {what}", first_key(table, command))
}

/// Hints joined as a window's key line: `y copy · Esc close`.
pub fn hint_line<C: PartialEq + Copy>(table: Table<C>, hints: &[(C, &str)]) -> String {
    let hints: Vec<String> = hints.iter().map(|&(c, what)| hint(table, c, what)).collect();
    hints.join(" · ")
}

/// A key with Ctrl (`^D`), for a window whose letters go to typing.
pub fn ctrl_label(c: char) -> String {
    format!("^{}", c.to_ascii_uppercase())
}

/// Moving in a list or a text: one step, a page, or to an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The arrows and vim's letters for them, the page keys, and Home/End
/// (vim's `g` and `G`).
pub const MOVE_KEYS: Table<Move> = &[
    (KeyCode::Up, Move::Up),
    (KeyCode::Char('k'), Move::Up),
    (KeyCode::Down, Move::Down),
    (KeyCode::Char('j'), Move::Down),
    (KeyCode::Left, Move::Left),
    (KeyCode::Char('h'), Move::Left),
    (KeyCode::Right, Move::Right),
    (KeyCode::Char('l'), Move::Right),
    (KeyCode::PageUp, Move::PageUp),
    (KeyCode::PageDown, Move::PageDown),
    (KeyCode::Home, Move::Home),
    (KeyCode::Char('g'), Move::Home),
    (KeyCode::End, Move::End),
    (KeyCode::Char('G'), Move::End),
];

/// Where `key` moves, if it's a moving key.
pub fn movement(key: KeyEvent) -> Option<Move> {
    lookup(MOVE_KEYS, key)
}

/// ↑ and ↓ (and `k`, `j`) together, as a hint names them.
pub const UP_DOWN: &str = "↑↓";
/// ← and → (and `h`, `l`) together.
pub const LEFT_RIGHT: &str = "←→";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    /// No key in `table` twice: each does one thing.
    pub fn no_key_twice<C>(table: Table<C>) {
        for (i, (code, _)) in table.iter().enumerate() {
            assert!(!table[..i].iter().any(|(c, _)| c == code), "{code:?} is in the table twice");
        }
    }

    #[test]
    fn moving_keys_are_the_arrows_and_hjkl_plain() {
        no_key_twice(MOVE_KEYS);
        let key = |code, m| KeyEvent::new(code, m);
        assert_eq!(movement(key(KeyCode::Char('j'), KeyModifiers::NONE)), Some(Move::Down));
        assert_eq!(movement(key(KeyCode::Left, KeyModifiers::NONE)), Some(Move::Left));
        assert_eq!(movement(key(KeyCode::Char('j'), KeyModifiers::CONTROL)), None);
    }

    #[test]
    fn each_windows_key_does_one_thing() {
        use crate::app::windows::{activity, agents, cell, memory, sandbox};
        no_key_twice(cell::CELL_KEYS);
        no_key_twice(activity::ACTIVITY_KEYS);
        no_key_twice(agents::AGENTS_KEYS);
        no_key_twice(sandbox::SANDBOX_KEYS);
        no_key_twice(memory::MEMORY_KEYS);
        no_key_twice(memory::EDITS_KEYS);
    }

    /// A window's own keys never are moving keys: a list moves first
    /// (`list::step`), so one would be taken before its window saw it.
    #[test]
    fn no_windows_key_is_a_moving_one() {
        use crate::app::manager::keys::{CONNECTIONS_KEYS, PROJECTS_KEYS};
        use crate::app::windows::{activity, agents, cell, memory, sandbox};
        let moving = [Move::Up, Move::Down, Move::PageUp, Move::PageDown, Move::Home, Move::End];
        let taken = |code: KeyCode| MOVE_KEYS.iter().any(|&(c, m)| c == code && moving.contains(&m));
        fn all<C: Copy>(t: Table<C>) -> Vec<KeyCode> {
            t.iter().map(|&(c, _)| c).collect()
        }
        for code in [
            all(cell::CELL_KEYS),
            all(activity::ACTIVITY_KEYS),
            all(agents::AGENTS_KEYS),
            all(memory::MEMORY_KEYS),
            all(memory::EDITS_KEYS),
            all(sandbox::SANDBOX_KEYS),
            all(PROJECTS_KEYS),
            all(CONNECTIONS_KEYS),
        ]
        .concat()
        {
            assert!(!taken(code), "{code:?} moves in a list: a window's key can't be it");
        }
    }

    #[test]
    fn a_hint_line_names_each_commands_first_key() {
        const T: Table<u8> = &[(KeyCode::Char('y'), 1), (KeyCode::Esc, 2), (KeyCode::Char('q'), 2)];
        assert_eq!(hint_line(T, &[(1, "copy"), (2, "close")]), "y copy · Esc close");
        assert_eq!(all_keys(T, &[2]), "Esc · q");
    }
}
