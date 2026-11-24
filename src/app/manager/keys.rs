//! The projects and connections screens' keys (see `key_table`): each
//! screen's handler looks a key up here, and its footer and help name the
//! keys from here. Moving in the list is `list::step`'s.

use crossterm::event::KeyCode;

pub use crate::app::key_table::{Table, all_keys, first_key, lookup};

/// What a key does on the projects screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectsKey {
    /// The selected project (the open one: its connections).
    Open,
    New,
    Rename,
    Delete,
    /// The selected project's connections, opening it first.
    Connections,
    /// Round to the next pane (the connections).
    NextPane,
    /// Round to the previous pane (the settings).
    PrevPane,
    Back,
}

/// What a key does on the connections screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionsKey {
    /// The selected connection becomes this tab's ("+ New" makes one).
    Connect,
    NewTab,
    /// Into the settings pane, to change them.
    Edit,
    New,
    Move,
    Delete,
    Containers,
    Projects,
    Back,
}

pub const PROJECTS_KEYS: Table<ProjectsKey> = &[
    (KeyCode::Enter, ProjectsKey::Open),
    (KeyCode::Char('n'), ProjectsKey::New),
    (KeyCode::Char('r'), ProjectsKey::Rename),
    (KeyCode::Char('d'), ProjectsKey::Delete),
    (KeyCode::Right, ProjectsKey::Connections),
    (KeyCode::Char('l'), ProjectsKey::Connections),
    (KeyCode::Tab, ProjectsKey::NextPane),
    (KeyCode::BackTab, ProjectsKey::PrevPane),
    (KeyCode::Esc, ProjectsKey::Back),
];

pub const CONNECTIONS_KEYS: Table<ConnectionsKey> = &[
    (KeyCode::Enter, ConnectionsKey::Connect),
    (KeyCode::Char('t'), ConnectionsKey::NewTab),
    (KeyCode::Char('e'), ConnectionsKey::Edit),
    (KeyCode::Right, ConnectionsKey::Edit),
    (KeyCode::Char('l'), ConnectionsKey::Edit),
    (KeyCode::Tab, ConnectionsKey::Edit),
    (KeyCode::Char('n'), ConnectionsKey::New),
    (KeyCode::Char('m'), ConnectionsKey::Move),
    (KeyCode::Char('d'), ConnectionsKey::Delete),
    (KeyCode::Char('c'), ConnectionsKey::Containers),
    (KeyCode::Left, ConnectionsKey::Projects),
    (KeyCode::Char('h'), ConnectionsKey::Projects),
    (KeyCode::BackTab, ConnectionsKey::Projects),
    (KeyCode::Esc, ConnectionsKey::Back),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::key_table::tests::no_key_twice;
    use crossterm::event::{KeyEvent, KeyModifiers};

    #[test]
    fn each_key_does_one_thing() {
        no_key_twice(PROJECTS_KEYS);
        no_key_twice(CONNECTIONS_KEYS);
    }

    #[test]
    fn a_key_with_ctrl_or_alt_isnt_the_letter() {
        let d = |m| KeyEvent::new(KeyCode::Char('d'), m);
        assert_eq!(lookup(PROJECTS_KEYS, d(KeyModifiers::NONE)), Some(ProjectsKey::Delete));
        assert_eq!(lookup(PROJECTS_KEYS, d(KeyModifiers::CONTROL)), None);
        assert_eq!(lookup(PROJECTS_KEYS, d(KeyModifiers::ALT)), None);
    }

    #[test]
    fn the_hints_name_the_tables_keys() {
        assert_eq!(first_key(CONNECTIONS_KEYS, ConnectionsKey::Edit), "e");
        assert_eq!(all_keys(CONNECTIONS_KEYS, &[ConnectionsKey::Edit]), "e · → · l · Tab");
        assert_eq!(all_keys(PROJECTS_KEYS, &[ProjectsKey::New, ProjectsKey::Rename, ProjectsKey::Delete]), "n · r · d");
    }
}
