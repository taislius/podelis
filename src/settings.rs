use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::db::DEFAULT_ROW_LIMIT;
use crate::keys::KeySpec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    #[default]
    Standard,
    /// Modal editing with vim keys.
    Vim,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hints {
    /// Only the run key and how to see the others.
    #[default]
    Minimal,
    /// As many key hints as fit in the status bar.
    Full,
}

/// User preferences from `config.toml`. Every field is optional; the file
/// is only read, never written.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Rows kept in memory per result set.
    pub row_limit: usize,
    pub theme: ThemeName,
    /// Check SQL with the server (prepare without running) after typing
    /// pauses, instead of only the built-in parser.
    pub server_syntax_check: bool,
    /// Warn about SQL that runs but likely isn't what was meant (`= NULL`,
    /// `UPDATE` without `WHERE`...).
    pub lint: bool,
    /// List the database's own catalog (`pg_catalog`, `information_schema`,
    /// `sqlite_schema`…) in the navigator; `.` there toggles it.
    pub system_tables: bool,
    pub editor_mode: EditorMode,
    /// How many key hints the status bar shows.
    pub hints: Hints,
    /// Where exports are written; the current directory when unset. A
    /// leading `~/` means the home directory.
    pub export_dir: Option<String>,
    /// Reopen the last project's tabs and connections on startup.
    pub restore_session: bool,
    /// Clicks and the wheel work in the app (the terminal's own text
    /// selection then needs Shift, or Option on macOS).
    pub mouse: bool,
    /// Opens result rows (`o`), e.g. `vd`; `{}` is the file.
    /// Default: `$PAGER`, else `less -S`.
    pub viewer: Option<String>,
    /// Seconds between checks that an idle connection is still alive
    /// (and its latency); 0 turns them off.
    pub keepalive: u64,
    /// Space twice quickly in the editor opens the command menu (Ctrl+K).
    pub leader_double_space: bool,
    /// Seconds a query must run before its end is announced (a bell and a
    /// desktop notification) while you're in another window; 0 turns it off.
    pub notify_after: u64,
    /// Key bindings by action name (see `keys.rs`).
    pub keys: HashMap<String, KeySpec>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            row_limit: DEFAULT_ROW_LIMIT,
            theme: ThemeName::Dark,
            server_syntax_check: true,
            lint: true,
            system_tables: false,
            editor_mode: EditorMode::Standard,
            hints: Hints::Minimal,
            export_dir: None,
            restore_session: true,
            mouse: true,
            viewer: None,
            keepalive: 0,
            leader_double_space: false,
            notify_after: 10,
            keys: HashMap::new(),
        }
    }
}

impl Settings {
    /// Loads settings, falling back to defaults (with a warning) if the file
    /// is invalid.
    pub fn load(dir: &Path) -> (Self, Option<String>) {
        let path = dir.join("config.toml");
        let Ok(text) = fs::read_to_string(&path) else {
            return (Self::default(), None);
        };
        match toml::from_str::<Settings>(&text) {
            Ok(mut s) => {
                s.row_limit = s.row_limit.max(1);
                (s, None)
            }
            Err(e) => {
                let first = e.to_string().lines().next().unwrap_or_default().to_string();
                (Self::default(), Some(format!("Ignoring invalid {}: {first}", path.display())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing_or_partial() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()), (Settings::default(), None));
        fs::write(dir.path().join("config.toml"), "theme = \"light\"\n").unwrap();
        let (s, warning) = Settings::load(dir.path());
        assert!(warning.is_none());
        assert_eq!((s.theme, s.row_limit), (ThemeName::Light, DEFAULT_ROW_LIMIT));
    }

    #[test]
    fn invalid_file_warns_and_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "row_limt = 5\n").unwrap();
        let (s, warning) = Settings::load(dir.path());
        assert_eq!(s, Settings::default());
        assert!(warning.unwrap().contains("Ignoring invalid"));
    }
}
