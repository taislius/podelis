//! Key bindings for the query screen, configurable in config.toml:
//!
//! ```toml
//! [keys]
//! run = "Ctrl+Enter"
//! new_tab = ["Ctrl+T", "Ctrl+N"]
//! zoom = ["Ctrl+K z", "F12"]
//! ```
//!
//! A configured action replaces all of its default keys. `Ctrl+K x` is a
//! leader key: Ctrl+K opens a menu of them, then `x` runs the action.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Run,
    RunStatement,
    Explain,
    Complete,
    HistoryPrev,
    HistoryNext,
    Undo,
    Redo,
    ToggleSchema,
    OpenConnection,
    Disconnect,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    SaveTab,
    RenameTab,
    Library,
    Commit,
    Rollback,
    Find,
    Comment,
    Format,
    Palette,
    ConnectionInfo,
    Activity,
    ExplainAnalyze,
    InsertSnippet,
    Menu,
    Zoom,
    Heartbeat,
    Projects,
    TxMode,
    NextWarning,
    Ask,
    Memory,
    SelectStatement,
    ZoomStatement,
    TabList,
    Settings,
    SaveStatement,
    AiExplain,
}

impl Action {
    /// What the action does, for the command palette.
    pub fn describe(self) -> &'static str {
        match self {
            Action::Run => t!("action.run"),
            Action::RunStatement => t!("action.run_statement"),
            Action::Explain => t!("action.explain"),
            Action::Complete => t!("action.complete"),
            Action::HistoryPrev => t!("action.history_prev"),
            Action::HistoryNext => t!("action.history_next"),
            Action::Undo => t!("action.undo"),
            Action::Redo => t!("action.redo"),
            Action::ToggleSchema => t!("action.toggle_schema"),
            Action::OpenConnection => t!("action.open_connection"),
            Action::Disconnect => t!("action.disconnect"),
            Action::NewTab => t!("action.new_tab"),
            Action::CloseTab => t!("action.close_tab"),
            Action::NextTab => t!("action.next_tab"),
            Action::PrevTab => t!("action.prev_tab"),
            Action::SaveTab => t!("action.save_tab"),
            Action::RenameTab => t!("action.rename_tab"),
            Action::Library => t!("action.library"),
            Action::Commit => t!("action.commit"),
            Action::Rollback => t!("action.rollback"),
            Action::Find => t!("action.find"),
            Action::Comment => t!("action.comment"),
            Action::Format => t!("action.format"),
            Action::Palette => t!("action.palette"),
            Action::ConnectionInfo => t!("action.connection_info"),
            Action::Activity => t!("action.activity"),
            Action::ExplainAnalyze => t!("action.explain_analyze"),
            Action::InsertSnippet => t!("action.insert_snippet"),
            Action::Menu => t!("action.menu"),
            Action::Zoom => t!("action.zoom"),
            Action::Heartbeat => t!("action.heartbeat"),
            Action::Projects => t!("action.projects"),
            Action::TxMode => t!("action.tx_mode"),
            Action::NextWarning => t!("action.next_warning"),
            Action::Ask => t!("action.ask"),
            Action::Memory => t!("action.memory"),
            Action::SelectStatement => t!("action.select_statement"),
            Action::ZoomStatement => t!("action.zoom_statement"),
            Action::TabList => t!("action.tab_list"),
            Action::Settings => t!("action.settings"),
            Action::SaveStatement => t!("action.save_statement"),
            Action::AiExplain => t!("action.ai_explain"),
        }
    }

    /// A few words, for the Ctrl+K menu.
    pub fn brief(self) -> &'static str {
        match self {
            Action::Explain => t!("action_brief.explain"),
            Action::ExplainAnalyze => t!("action_brief.explain_analyze"),
            Action::ToggleSchema => t!("action_brief.toggle_schema"),
            Action::OpenConnection => t!("action_brief.open_connection"),
            Action::Disconnect => t!("action_brief.disconnect"),
            Action::RenameTab => t!("action_brief.rename_tab"),
            Action::Library => t!("action_brief.library"),
            Action::Commit => t!("action_brief.commit"),
            Action::Rollback => t!("action_brief.rollback"),
            Action::Comment => t!("action_brief.comment"),
            Action::Format => t!("action_brief.format"),
            Action::ConnectionInfo => t!("action_brief.connection_info"),
            Action::Activity => t!("action_brief.activity"),
            Action::InsertSnippet => t!("action_brief.insert_snippet"),
            Action::Menu => t!("action_brief.menu"),
            Action::Zoom => t!("action_brief.zoom"),
            Action::Heartbeat => t!("action_brief.heartbeat"),
            Action::Projects => t!("action_brief.projects"),
            Action::TxMode => t!("action_brief.tx_mode"),
            Action::NextWarning => t!("action_brief.next_warning"),
            Action::Ask => t!("action_brief.ask"),
            Action::Memory => t!("action_brief.memory"),
            Action::SelectStatement => t!("action_brief.select_statement"),
            Action::ZoomStatement => t!("action_brief.zoom_statement"),
            Action::TabList => t!("action_brief.tab_list"),
            Action::Settings => t!("action_brief.settings"),
            Action::AiExplain => t!("action_brief.ai_explain"),
            _ => self.describe(),
        }
    }

    /// Every action, in a sensible order.
    pub fn all() -> impl Iterator<Item = Action> {
        ACTIONS.iter().map(|(a, _, _)| *a)
    }

    /// About the editor's text (its statement, lines or cursor) or its tab:
    /// the Ctrl+K menu lists them only while the editor has the keys.
    pub fn is_editor_action(self) -> bool {
        matches!(
            self,
            Action::RenameTab
                | Action::Explain
                | Action::ExplainAnalyze
                | Action::Comment
                | Action::Format
                | Action::InsertSnippet
                | Action::NextWarning
                | Action::SelectStatement
                | Action::ZoomStatement
                | Action::SaveStatement
                | Action::AiExplain
        )
    }

    /// Tab and library actions work from every pane and over popups.
    pub fn is_tab_action(self) -> bool {
        matches!(
            self,
            Action::NewTab
                | Action::CloseTab
                | Action::NextTab
                | Action::PrevTab
                | Action::SaveTab
                | Action::RenameTab
                | Action::Library
        )
    }
}

/// Every action: its name in config.toml and default keys.
const ACTIONS: &[(Action, &str, &[&str])] = &[
    (Action::Run, "run", &["F5", "Ctrl+R", "Ctrl+Enter"]),
    (Action::RunStatement, "run_statement", &["F6"]),
    (Action::Explain, "explain", &["F7", "Ctrl+K e"]),
    (Action::Complete, "complete", &["Ctrl+Space"]),
    (Action::HistoryPrev, "history_prev", &["Ctrl+P"]),
    (Action::HistoryNext, "history_next", &["Ctrl+N"]),
    (Action::Undo, "undo", &["Ctrl+Z"]),
    (Action::Redo, "redo", &["Ctrl+Y"]),
    (Action::ToggleSchema, "toggle_schema", &["Ctrl+K n", "F2"]),
    (Action::OpenConnection, "open_connection", &["Ctrl+K o", "Ctrl+O"]),
    (Action::Disconnect, "disconnect", &["Ctrl+K d", "Ctrl+D"]),
    (Action::NewTab, "new_tab", &["Ctrl+T"]),
    (Action::CloseTab, "close_tab", &["Ctrl+W"]),
    (Action::NextTab, "next_tab", &["Ctrl+PageDown"]),
    (Action::PrevTab, "prev_tab", &["Ctrl+PageUp"]),
    (Action::SaveTab, "save_tab", &["Ctrl+S"]),
    (Action::RenameTab, "rename_tab", &["Ctrl+K t", "F4"]),
    (Action::Library, "library", &["Ctrl+K l", "F3"]),
    (Action::Commit, "commit", &["F8", "Ctrl+K c"]),
    (Action::Rollback, "rollback", &["Shift+F8", "Ctrl+K r"]),
    (Action::Find, "find", &["Ctrl+F"]),
    // Most terminals send Ctrl+/ as Ctrl+7.
    (Action::Comment, "comment", &["Ctrl+K -", "Ctrl+/", "Ctrl+7"]),
    (Action::Format, "format", &["Ctrl+K f", "F9"]),
    (Action::Palette, "palette", &["Ctrl+K"]),
    (Action::ConnectionInfo, "connection_info", &["Ctrl+K u"]),
    (Action::Activity, "activity", &["Ctrl+K s"]),
    (Action::ExplainAnalyze, "explain_analyze", &["Shift+F7", "Ctrl+K a"]),
    (Action::InsertSnippet, "insert_snippet", &["Ctrl+K i", "Ctrl+E"]),
    (Action::Menu, "menu", &["Ctrl+K m", "Shift+F10", "F10"]),
    (Action::Zoom, "zoom", &["Ctrl+K z", "F12"]),
    (Action::ZoomStatement, "zoom_statement", &["Ctrl+K b", "Shift+F12"]),
    (Action::TabList, "tab_list", &["Ctrl+K g"]),
    (Action::Settings, "settings", &["Ctrl+K ,"]),
    (Action::SelectStatement, "select_statement", &["Ctrl+K v"]),
    (Action::Heartbeat, "heartbeat", &["Ctrl+K h"]),
    (Action::Projects, "projects", &["Ctrl+K p"]),
    (Action::TxMode, "transaction_mode", &["Ctrl+K x"]),
    (Action::NextWarning, "next_warning", &["Ctrl+K w"]),
    (Action::Ask, "ask", &["Ctrl+K q"]),
    (Action::AiExplain, "ai_explain", &["Ctrl+K y"]),
    (Action::SaveStatement, "save_statement", &[]),
    (Action::Memory, "memory", &["Ctrl+K k"]),
];

/// One key or several, as written in config.toml.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeySpec {
    One(String),
    Many(Vec<String>),
}

type Binding = (KeyCode, KeyModifiers);

#[derive(Clone, Debug)]
pub struct KeyMap {
    bindings: Vec<(Binding, Action)>,
    /// Letters after Ctrl+K.
    leader: Vec<(char, Action)>,
    labels: HashMap<Action, String>,
}

impl Default for KeyMap {
    fn default() -> Self {
        KeyMap::new(&HashMap::new()).0
    }
}

impl KeyMap {
    /// The defaults with `overrides` applied, and warnings about anything
    /// that couldn't be used.
    pub fn new(overrides: &HashMap<String, KeySpec>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut names: Vec<&String> = overrides.keys().collect();
        names.sort();
        for name in names {
            if !ACTIONS.iter().any(|(_, n, _)| n == name) {
                let known: Vec<&str> = ACTIONS.iter().map(|(_, n, _)| *n).collect();
                warnings.push(format!("[keys]: unknown action '{name}' (known: {})", known.join(", ")));
            }
        }
        let mut map = KeyMap { bindings: Vec::new(), leader: Vec::new(), labels: HashMap::new() };
        let specs_of = |name: &str, defaults: &[&str]| -> Vec<String> {
            match overrides.get(name) {
                Some(KeySpec::One(s)) => vec![s.clone()],
                Some(KeySpec::Many(v)) => v.clone(),
                None => defaults.iter().map(|s| (*s).to_string()).collect(),
            }
        };
        // Leader letters are shown after the palette's first key.
        let leader_key = ACTIONS
            .iter()
            .find(|(a, _, _)| *a == Action::Palette)
            .and_then(|(_, name, defaults)| specs_of(name, defaults).into_iter().find(|s| leader_spec(s).is_none()))
            .unwrap_or_else(|| "Ctrl+K".to_string());
        for &(action, name, defaults) in ACTIONS {
            let specs = specs_of(name, defaults);
            let mut labels = Vec::new();
            for spec in specs {
                if let Some(rest) = leader_spec(&spec) {
                    match rest {
                        Ok(c) => {
                            map.leader.push((c, action));
                            labels.push(format!("{leader_key} {c}"));
                        }
                        Err(e) => warnings.push(format!("[keys] {name}: {e}")),
                    }
                    continue;
                }
                match parse_key(&spec) {
                    Ok(binding) => {
                        map.bindings.push((binding, action));
                        labels.push(spec);
                    }
                    Err(e) => warnings.push(format!("[keys] {name}: {e}")),
                }
            }
            map.labels.insert(action, labels.join(" · "));
        }
        // Configured keys come first, so they win over a default elsewhere.
        let configured = |action: &Action| {
            let name = ACTIONS.iter().find(|(a, _, _)| a == action).map_or("", |(_, n, _)| *n);
            !overrides.contains_key(name)
        };
        map.bindings.sort_by_key(|(_, action)| configured(action));
        map.leader.sort_by_key(|(_, action)| configured(action));
        (map, warnings)
    }

    /// The action of `c` after Ctrl+K.
    pub fn leader(&self, c: char) -> Option<Action> {
        self.leader.iter().find(|(k, _)| *k == c).map(|(_, a)| *a)
    }

    /// The letters after Ctrl+K, in the order of the actions, each once.
    pub fn leader_keys(&self) -> Vec<(char, Action)> {
        let mut keys: Vec<(char, Action)> = Vec::new();
        for action in Action::all() {
            for &(c, a) in &self.leader {
                if a == action && !keys.iter().any(|(k, _)| *k == c) {
                    keys.push((c, a));
                }
            }
        }
        keys
    }

    pub fn action(&self, key: KeyEvent) -> Option<Action> {
        let (code, mods) = normalize(key.code, key.modifiers);
        self.bindings.iter().find(|((c, m), _)| *c == code && *m == mods).map(|(_, a)| *a)
    }

    /// The keys for `action`, for help and hints ("F5 · Ctrl+R").
    pub fn label(&self, action: Action) -> &str {
        self.labels.get(&action).map_or("", String::as_str)
    }

    /// The first key for `action`, short ("^P" for Ctrl+P).
    pub fn short_label(&self, action: Action) -> String {
        let first = self.label(action).split(" · ").next().unwrap_or_default();
        first.strip_prefix("Ctrl+").map_or_else(|| first.to_string(), |rest| format!("^{rest}"))
    }
}

/// Letters compare case-insensitively, and Shift only counts for keys
/// that aren't characters (`Shift+Tab` is its own key anyway).
fn normalize(code: KeyCode, mods: KeyModifiers) -> Binding {
    let mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    match code {
        KeyCode::Char(c) => (KeyCode::Char(c.to_ascii_lowercase()), mods - KeyModifiers::SHIFT),
        _ => (code, mods),
    }
}

/// For `Ctrl+K x` (or `Leader x`): the letter, or why it can't be one.
fn leader_spec(spec: &str) -> Option<Result<char, String>> {
    let (prefix, rest) = spec.trim().split_once(' ')?;
    if !prefix.eq_ignore_ascii_case("ctrl+k") && !prefix.eq_ignore_ascii_case("leader") {
        return None;
    }
    let mut chars = rest.trim().chars();
    Some(match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_ascii_digit() && c != '?' && c != '/' => Ok(c),
        _ => Err(format!("'{spec}': after Ctrl+K use one character (not a digit, ? or /)")),
    })
}

/// Parses "Ctrl+Shift+PageUp", "Alt+Enter", "F5", "Ctrl+Space", "g".
pub fn parse_key(spec: &str) -> Result<Binding, String> {
    let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    let (key, modifiers) = match parts.split_last() {
        // "Ctrl++" means Ctrl and the plus key.
        Some((&"", rest)) if spec.ends_with("++") => ("+", &rest[..rest.len() - 1]),
        Some((key, rest)) => (*key, rest),
        None => return Err("empty key".into()),
    };
    let mut mods = KeyModifiers::NONE;
    for m in modifiers {
        mods |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" | "option" | "meta" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return Err(format!("unknown modifier '{m}' in '{spec}' (use Ctrl, Alt, Shift)")),
        };
    }
    let lower = key.to_ascii_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            KeyCode::F(f[1..].parse().unwrap_or(1))
        }
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return Err(format!("unknown key '{key}' in '{spec}'")),
            }
        }
    };
    if mods.is_empty() && matches!(code, KeyCode::Char(_)) {
        return Err(format!("'{spec}' would stop you typing it; add Ctrl or Alt"));
    }
    Ok(normalize(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_key_specs() {
        assert_eq!(parse_key("F5"), Ok((KeyCode::F(5), KeyModifiers::NONE)));
        assert_eq!(parse_key("ctrl+enter"), Ok((KeyCode::Enter, KeyModifiers::CONTROL)));
        assert_eq!(parse_key("Ctrl+Shift+PageUp"), Ok((KeyCode::PageUp, KeyModifiers::CONTROL | KeyModifiers::SHIFT)));
        assert_eq!(parse_key("Alt+X"), Ok((KeyCode::Char('x'), KeyModifiers::ALT)));
        assert_eq!(parse_key("Ctrl++"), Ok((KeyCode::Char('+'), KeyModifiers::CONTROL)));
        assert!(parse_key("x").unwrap_err().contains("add Ctrl"));
        assert!(parse_key("Hyper+X").unwrap_err().contains("unknown modifier"));
        assert!(parse_key("Ctrl+Foo").unwrap_err().contains("unknown key"));
        assert!(parse_key("F99").is_err());
    }

    #[test]
    fn defaults_and_overrides() {
        let map = KeyMap::default();
        assert_eq!(map.action(event(KeyCode::F(5), KeyModifiers::NONE)), Some(Action::Run));
        assert_eq!(
            map.action(event(KeyCode::Char('T'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            Some(Action::NewTab)
        );
        assert_eq!(map.short_label(Action::HistoryPrev), "^P");

        let overrides = HashMap::from([
            ("run".to_string(), KeySpec::One("F9".into())),
            ("new_tab".to_string(), KeySpec::Many(vec!["Ctrl+T".into(), "Ctrl+N".into()])),
            ("frobnicate".to_string(), KeySpec::One("F1".into())),
            ("explain".to_string(), KeySpec::One("q".into())),
        ]);
        let (map, warnings) = KeyMap::new(&overrides);
        assert_eq!(map.action(event(KeyCode::F(9), KeyModifiers::NONE)), Some(Action::Run));
        assert_eq!(map.action(event(KeyCode::F(5), KeyModifiers::NONE)), None);
        // Ctrl+N is now new_tab, taking precedence over history_next's default.
        assert_eq!(map.action(event(KeyCode::Char('n'), KeyModifiers::CONTROL)), Some(Action::NewTab));
        assert_eq!(map.label(Action::NewTab), "Ctrl+T · Ctrl+N");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("frobnicate") && warnings[1].contains("explain"));
    }

    #[test]
    fn leader_keys_and_their_overrides() {
        let map = KeyMap::default();
        assert_eq!(map.leader('x'), Some(Action::TxMode));
        assert_eq!(map.label(Action::Zoom), "Ctrl+K z · F12");
        assert_eq!(map.short_label(Action::Zoom), "^K z");
        assert_eq!(map.short_label(Action::Commit), "F8");

        let overrides = HashMap::from([
            ("zoom".to_string(), KeySpec::Many(vec!["Leader Z".into(), "F12".into()])),
            ("format".to_string(), KeySpec::One("Ctrl+K 5".into())),
        ]);
        let (map, warnings) = KeyMap::new(&overrides);
        assert_eq!((map.leader('Z'), map.leader('z')), (Some(Action::Zoom), None));
        assert_eq!(map.label(Action::Zoom), "Ctrl+K Z · F12");
        assert!(warnings.len() == 1 && warnings[0].contains("one character"), "{warnings:?}");

        // Rebinding the palette renames the leader in every label.
        let overrides = HashMap::from([("palette".to_string(), KeySpec::One("Ctrl+B".into()))]);
        let map = KeyMap::new(&overrides).0;
        assert_eq!(map.label(Action::Zoom), "Ctrl+B z · F12");
    }
}
