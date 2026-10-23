use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::config::keys::KeySpec;
use crate::db::DEFAULT_ROW_LIMIT;

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

/// A path as typed in settings or on the command line: a leading `~/` is
/// the home folder.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => std::path::PathBuf::from(path),
    }
}

/// User preferences from `config.toml`. Every field is optional. The file
/// is written only by the settings screen, one value at a time
/// ([`save_value`]), keeping everything else in it as it was.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Rows kept in memory per result set.
    pub row_limit: usize,
    pub theme: ThemeName,
    /// The language of the app's words: `auto` (the system's, from
    /// `LC_ALL`, `LC_MESSAGES` or `LANG`), `en` or `lt`.
    pub language: String,
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
    /// A program that shows result rows as TSV in this terminal (`o`, then
    /// TSV), e.g. `vd`; `{}` is the file. Kept from before `viewers`.
    pub viewer: Option<String>,
    /// What opens result rows in each format (`o`): `csv`, `tsv`, `json`,
    /// `sql`, `txt`. Default: the system's app for the file's type.
    pub viewers: HashMap<String, Viewer>,
    /// Seconds between checks that an idle connection is still alive
    /// (and its latency); 0 turns them off.
    pub keepalive: u64,
    /// Space twice quickly in the editor opens the command menu (Ctrl+K).
    pub leader_double_space: bool,
    /// Ctrl+K shows its menu when no letter follows within half a second;
    /// off, it waits quietly (`?`, or Ctrl+Shift+K where the terminal can
    /// tell it apart, shows the menu).
    pub leader_menu: bool,
    /// Seconds a query runs before Podėlis says how to cancel it; 0 never
    /// says.
    pub long_query: u64,
    /// Seconds after which any query is cancelled, on connections without
    /// a Timeout of their own; 0 lets queries run.
    pub query_timeout: u64,
    /// Seconds a query must run before its end is announced (a bell and a
    /// desktop notification) while you're in another window; 0 turns it off.
    pub notify_after: u64,
    /// On production connections, a query that would read a whole table of
    /// at least this many rows asks first (agents' are refused); 0 turns
    /// the check off.
    pub guard_rows: u64,
    /// Minutes the gateway waits with no client connected before it
    /// stops (it's started again when needed); 0 keeps it running.
    pub gateway_idle: u64,
    /// The models the gateway may use, the default first (`[[models]]`).
    pub models: Vec<crate::gateway::models::ModelConfig>,
    /// Key bindings by action name (see `keys.rs`).
    pub keys: HashMap<String, KeySpec>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            row_limit: DEFAULT_ROW_LIMIT,
            theme: ThemeName::Dark,
            language: "auto".to_string(),
            server_syntax_check: true,
            lint: true,
            system_tables: false,
            editor_mode: EditorMode::Standard,
            hints: Hints::Minimal,
            export_dir: None,
            restore_session: true,
            mouse: true,
            viewer: None,
            viewers: HashMap::new(),
            keepalive: 0,
            leader_double_space: false,
            leader_menu: true,
            long_query: 600,
            query_timeout: 0,
            notify_after: 10,
            guard_rows: 1_000_000,
            gateway_idle: 30,
            models: Vec::new(),
            keys: HashMap::new(),
        }
    }
}

/// A program that opens result rows: `"libreoffice --calc"` (`{}` is the
/// file, else it goes last), started on its own while Podėlis runs on, or
/// `{ command = "vd", terminal = true }`, run in Podėlis's terminal until
/// it exits.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Viewer {
    Command(String),
    Full {
        command: String,
        #[serde(default)]
        terminal: bool,
    },
}

/// The formats result rows open in (`o`), by their key in `[viewers]`
/// (and their files' extension, but an ASCII table's file is a `.txt`).
pub const VIEWER_FORMATS: [&str; 6] = ["csv", "json", "sql", "tsv", "txt", "ascii"];

/// The system's command that opens a file in the app for its type.
pub const SYSTEM_OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// Whether there's a desktop to show an app on: not over SSH on macOS
/// (`open` would show it on the Mac's own screen), and on Linux a display
/// (X, Wayland; one forwarded over SSH counts).
pub fn has_desktop() -> bool {
    let set = |v: &str| std::env::var_os(v).is_some_and(|v| !v.is_empty());
    if cfg!(target_os = "macos") {
        !set("SSH_CONNECTION") && !set("SSH_TTY")
    } else {
        set("DISPLAY") || set("WAYLAND_DISPLAY")
    }
}

impl Settings {
    /// What opens result rows in `format` (a `VIEWER_FORMATS` key):
    /// `[viewers]`, the old `viewer` for TSV (in the terminal), else the
    /// system's app, or without a desktop (`has_desktop`) `$PAGER` (else
    /// `less -S`) in the terminal.
    pub fn viewer_for(&self, format: &str) -> Viewer {
        self.viewer_with(format, has_desktop(), std::env::var("PAGER").ok())
    }

    pub(crate) fn viewer_with(&self, format: &str, desktop: bool, pager: Option<String>) -> Viewer {
        let given = |v: &&String| !v.trim().is_empty();
        if let Some(viewer) = self.viewers.get(format).filter(|v| !v.command().trim().is_empty()) {
            return viewer.clone();
        }
        // An ASCII table is text: without a viewer of its own, text's.
        if format == "ascii" {
            return self.viewer_with("txt", desktop, pager);
        }
        if let Some(old) = self.viewer.as_ref().filter(given).filter(|_| format == "tsv") {
            return Viewer::Full { command: old.clone(), terminal: true };
        }
        if desktop {
            return Viewer::Command(SYSTEM_OPENER.to_string());
        }
        let pager = pager.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "less -S".to_string());
        Viewer::Full { command: pager, terminal: true }
    }
}

impl Viewer {
    pub fn command(&self) -> &str {
        match self {
            Viewer::Command(c) | Viewer::Full { command: c, .. } => c,
        }
    }

    pub fn in_terminal(&self) -> bool {
        matches!(self, Viewer::Full { terminal: true, .. })
    }
}

impl Settings {
    /// Loads settings, falling back to defaults (with a warning) if the file
    /// is invalid.
    pub fn load(dir: &Path) -> (Self, Option<String>) {
        match Self::load_strict(dir) {
            Ok(s) => (s, None),
            Err(e) => (Self::default(), Some(format!("Ignoring invalid {e}"))),
        }
    }

    /// Loads settings; an invalid file is an error (`<path>: <what's
    /// wrong>`), for whoever can't do without what it says (the gateway's
    /// models). No file is the defaults.
    pub fn load_strict(dir: &Path) -> Result<Self, String> {
        let path = dir.join("config.toml");
        let Ok(text) = fs::read_to_string(&path) else {
            return Ok(Self::default());
        };
        match toml::from_str::<Settings>(&text) {
            Ok(mut s) => {
                s.row_limit = s.row_limit.max(1);
                Ok(s)
            }
            Err(e) => Err(format!("{}: {}", path.display(), toml_problem(&e))),
        }
    }
}

/// Sets `key` to `value` in `dir`'s `config.toml` (made if there's none),
/// or removes it (`None`: back to its default), keeping the file's
/// comments, layout and other settings. A file that's there but can't be
/// read is left alone: writing it back would lose what's in it.
pub fn save_value(dir: &Path, key: &str, value: Option<toml_edit::Value>) -> Result<(), String> {
    let path = dir.join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| format!("{}: {}", path.display(), one_line_toml_error(&e)))?;
    // A new key goes before the first table ([keys], [[models]]…), where
    // top-level settings belong.
    match (doc.get_mut(key).and_then(toml_edit::Item::as_value_mut), value) {
        (Some(old), Some(value)) => {
            let decor = old.decor().clone();
            *old = value;
            *old.decor_mut() = decor;
        }
        (None, Some(value)) => {
            doc.insert(key, toml_edit::Item::Value(value));
        }
        (_, None) => {
            doc.remove(key);
        }
    }
    crate::config::store::write_atomic(&path, &doc.to_string()).map_err(|e| format!("{}: {e:#}", path.display()))
}

fn one_line_toml_error(e: &toml_edit::TomlError) -> String {
    e.to_string().lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default().to_string()
}

/// Where a TOML file is wrong and what's wrong, on one line: its first
/// line (the place) and its last (what), without the excerpt between.
fn toml_problem(e: &toml::de::Error) -> String {
    let text = e.to_string();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default();
    match lines.next_back() {
        Some(last) if !last.starts_with('|') && !last.starts_with('^') => format!("{first}: {last}"),
        _ => first.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_opens_rows_by_default_and_without_a_desktop() {
        let mut s = Settings::default();
        let pager = || Some("most".to_string());
        for format in VIEWER_FORMATS {
            // A desktop: the system's app for the type; none: the pager, here.
            assert_eq!(s.viewer_with(format, true, pager()), Viewer::Command(SYSTEM_OPENER.into()));
            assert_eq!(s.viewer_with(format, false, pager()), Viewer::Full { command: "most".into(), terminal: true });
        }
        assert_eq!(s.viewer_with("csv", false, None), Viewer::Full { command: "less -S".into(), terminal: true });
        // The old `viewer` is TSV's, in this terminal; `[viewers]` comes first.
        s.viewer = Some("vd".into());
        assert_eq!(s.viewer_with("tsv", true, None), Viewer::Full { command: "vd".into(), terminal: true });
        assert_eq!(s.viewer_with("csv", true, None), Viewer::Command(SYSTEM_OPENER.into()));
        // An ASCII table: its own viewer, else text's.
        s.viewers.insert("txt".into(), Viewer::Command("gedit".into()));
        assert_eq!(s.viewer_with("ascii", true, None), Viewer::Command("gedit".into()));
        s.viewers.insert("ascii".into(), Viewer::Command("kate".into()));
        assert_eq!(s.viewer_with("ascii", true, None), Viewer::Command("kate".into()));
        s.viewers.insert("tsv".into(), Viewer::Command("libreoffice --calc".into()));
        assert_eq!(s.viewer_with("tsv", false, None), Viewer::Command("libreoffice --calc".into()));
    }

    #[test]
    fn viewers_by_format() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "[viewers]\ncsv = \"libreoffice --calc\"\ntsv = { command = \"vd\", terminal = true }\n",
        )
        .unwrap();
        let (s, warning) = Settings::load(dir.path());
        assert!(warning.is_none(), "{warning:?}");
        assert_eq!(s.viewers["csv"], Viewer::Command("libreoffice --calc".into()));
        assert!(s.viewers["tsv"].in_terminal() && s.viewers["tsv"].command() == "vd");
    }

    #[test]
    fn a_saved_value_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // None yet: made.
        save_value(dir.path(), "theme", Some("light".into())).unwrap();
        assert_eq!(Settings::load(dir.path()).0.theme, ThemeName::Light);
        fs::write(&path, "# mine\ntheme = \"dark\"   # for the evenings\nrow_limit = 500\n\n[keys]\nrun = \"F5\"\n")
            .unwrap();
        save_value(dir.path(), "theme", Some("light".into())).unwrap();
        save_value(dir.path(), "lint", Some(false.into())).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\ntheme = \"light\"   # for the evenings\nrow_limit = 500\n"), "{text}");
        assert!(text.contains("lint = false") && text.contains("[keys]\nrun = \"F5\""), "{text}");
        assert!(text.find("lint").unwrap() < text.find("[keys]").unwrap(), "top-level, before the tables: {text}");
        let s = Settings::load(dir.path()).0;
        assert_eq!((s.theme, s.lint, s.row_limit), (ThemeName::Light, false, 500));
        // Removed: back to the default, the rest kept.
        save_value(dir.path(), "row_limit", None).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("row_limit") && text.contains("# mine"), "{text}");
        // A file that can't be read isn't written over.
        fs::write(&path, b"theme = \"dark\" # caf\xe9\n").unwrap();
        assert!(save_value(dir.path(), "lint", Some(true.into())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"theme = \"dark\" # caf\xe9\n");
    }

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
