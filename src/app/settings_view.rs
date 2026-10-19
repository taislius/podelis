//! The settings screen (Ctrl+K ,): the general settings of `config.toml`,
//! each changed in place and saved at once, the rest of the file kept as
//! it was. Key bindings, viewers and models stay in the file.

use super::input::{InputResult, LineInput};
use super::list::{self, Nav};
use super::*;
use crate::config::settings::{EditorMode, Hints, ThemeName, save_value};

/// A setting the screen shows, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Language,
    Theme,
    EditorMode,
    Hints,
    Mouse,
    RestoreSession,
    ServerSyntaxCheck,
    Lint,
    SystemTables,
    LeaderDoubleSpace,
    RowLimit,
    Keepalive,
    NotifyAfter,
    GuardRows,
    GatewayIdle,
    ExportDir,
}

/// What kind of value a setting has: how it's changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One of a few: ←/→ go round them.
    Choice,
    /// On or off.
    Toggle,
    /// A whole number, typed (0 turns some off).
    Number,
    /// Text, typed (empty: unset).
    Text,
}

impl Field {
    pub const ALL: [Field; 16] = [
        Field::Language,
        Field::Theme,
        Field::EditorMode,
        Field::Hints,
        Field::Mouse,
        Field::RestoreSession,
        Field::ServerSyntaxCheck,
        Field::Lint,
        Field::SystemTables,
        Field::LeaderDoubleSpace,
        Field::RowLimit,
        Field::Keepalive,
        Field::NotifyAfter,
        Field::GuardRows,
        Field::GatewayIdle,
        Field::ExportDir,
    ];

    /// Its name in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Field::Language => "language",
            Field::Theme => "theme",
            Field::EditorMode => "editor_mode",
            Field::Hints => "hints",
            Field::Mouse => "mouse",
            Field::RestoreSession => "restore_session",
            Field::ServerSyntaxCheck => "server_syntax_check",
            Field::Lint => "lint",
            Field::SystemTables => "system_tables",
            Field::LeaderDoubleSpace => "leader_double_space",
            Field::RowLimit => "row_limit",
            Field::Keepalive => "keepalive",
            Field::NotifyAfter => "notify_after",
            Field::GuardRows => "guard_rows",
            Field::GatewayIdle => "gateway_idle",
            Field::ExportDir => "export_dir",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Field::Language | Field::Theme | Field::EditorMode | Field::Hints => Kind::Choice,
            Field::RowLimit | Field::Keepalive | Field::NotifyAfter | Field::GuardRows | Field::GatewayIdle => {
                Kind::Number
            }
            Field::ExportDir => Kind::Text,
            _ => Kind::Toggle,
        }
    }

    /// Its name on the screen.
    pub fn label(self) -> &'static str {
        match self {
            Field::Language => t!("settings.language"),
            Field::Theme => t!("settings.theme"),
            Field::EditorMode => t!("settings.editor_mode"),
            Field::Hints => t!("settings.hints"),
            Field::Mouse => t!("settings.mouse"),
            Field::RestoreSession => t!("settings.restore_session"),
            Field::ServerSyntaxCheck => t!("settings.server_syntax_check"),
            Field::Lint => t!("settings.lint"),
            Field::SystemTables => t!("settings.system_tables"),
            Field::LeaderDoubleSpace => t!("settings.leader_double_space"),
            Field::RowLimit => t!("settings.row_limit"),
            Field::Keepalive => t!("settings.keepalive"),
            Field::NotifyAfter => t!("settings.notify_after"),
            Field::GuardRows => t!("settings.guard_rows"),
            Field::GatewayIdle => t!("settings.gateway_idle"),
            Field::ExportDir => t!("settings.export_dir"),
        }
    }

    /// What it does, below the list while it's selected.
    pub fn about(self) -> &'static str {
        match self {
            Field::Language => t!("settings.language_about"),
            Field::Theme => t!("settings.theme_about"),
            Field::EditorMode => t!("settings.editor_mode_about"),
            Field::Hints => t!("settings.hints_about"),
            Field::Mouse => t!("settings.mouse_about"),
            Field::RestoreSession => t!("settings.restore_session_about"),
            Field::ServerSyntaxCheck => t!("settings.server_syntax_check_about"),
            Field::Lint => t!("settings.lint_about"),
            Field::SystemTables => t!("settings.system_tables_about"),
            Field::LeaderDoubleSpace => t!("settings.leader_double_space_about"),
            Field::RowLimit => t!("settings.row_limit_about"),
            Field::Keepalive => t!("settings.keepalive_about"),
            Field::NotifyAfter => t!("settings.notify_after_about"),
            Field::GuardRows => t!("settings.guard_rows_about"),
            Field::GatewayIdle => t!("settings.gateway_idle_about"),
            Field::ExportDir => t!("settings.export_dir_about"),
        }
    }
}

/// The settings screen's state.
pub struct SettingsView {
    pub selected: usize,
    /// A number or text being typed in for the selected setting.
    pub editing: Option<LineInput>,
    /// What the last change did (saved, or why not), and whether it failed.
    pub said: Option<(String, bool)>,
}

fn on_off(on: bool) -> String {
    if on { t!("settings.on") } else { t!("settings.off") }.to_string()
}

/// A number, or "off" for 0 where 0 turns it off.
fn number_or_off(n: u64) -> String {
    if n == 0 { t!("settings.off").to_string() } else { n.to_string() }
}

impl App {
    /// Ctrl+K ,: the settings, the first selected.
    pub(super) fn open_settings(&mut self) {
        self.settings_view = Some(SettingsView { selected: 0, editing: None, said: None });
    }

    /// A setting's value as the screen shows it.
    pub fn setting_shown(&self, field: Field) -> String {
        let s = &self.settings;
        match field {
            Field::Language if s.language.eq_ignore_ascii_case("auto") => {
                let now = crate::lang::LANGUAGES.iter().find(|(c, _, _)| *c == crate::lang::code());
                format!("{} ({})", t!("settings.auto"), now.map_or("English", |(_, name, _)| *name))
            }
            Field::Language => crate::lang::LANGUAGES
                .iter()
                .find(|(c, _, _)| c.eq_ignore_ascii_case(&s.language))
                .map_or_else(|| s.language.clone(), |(_, name, _)| (*name).to_string()),
            Field::Theme => match s.theme {
                ThemeName::Dark => t!("settings.dark"),
                ThemeName::Light => t!("settings.light"),
            }
            .to_string(),
            Field::EditorMode => match s.editor_mode {
                EditorMode::Standard => t!("settings.standard"),
                EditorMode::Vim => t!("settings.vim"),
            }
            .to_string(),
            Field::Hints => match s.hints {
                Hints::Minimal => t!("settings.minimal"),
                Hints::Full => t!("settings.full"),
            }
            .to_string(),
            Field::Mouse => on_off(s.mouse),
            Field::RestoreSession => on_off(s.restore_session),
            Field::ServerSyntaxCheck => on_off(s.server_syntax_check),
            Field::Lint => on_off(s.lint),
            Field::SystemTables => on_off(s.system_tables),
            Field::LeaderDoubleSpace => on_off(s.leader_double_space),
            Field::RowLimit => s.row_limit.to_string(),
            Field::Keepalive => number_or_off(s.keepalive),
            Field::NotifyAfter => number_or_off(s.notify_after),
            Field::GuardRows => number_or_off(s.guard_rows),
            Field::GatewayIdle => number_or_off(s.gateway_idle),
            Field::ExportDir => s.export_dir.clone().unwrap_or_else(|| t!("settings.current_dir").to_string()),
        }
    }

    pub(super) fn handle_settings_key(&mut self, key: KeyEvent) {
        let Some(selected) = self.settings_view.as_ref().map(|v| v.selected) else { return };
        let field = Field::ALL[selected.min(Field::ALL.len() - 1)];
        let typed = match field {
            Field::ExportDir => self.settings.export_dir.clone().unwrap_or_default(),
            _ => self.setting_number(field).to_string(),
        };
        let Some(view) = &mut self.settings_view else { return };
        if let Some(input) = &mut view.editing {
            match input.handle(key) {
                InputResult::Cancel => view.editing = None,
                InputResult::Submit => {
                    let text = input.text().trim().to_string();
                    view.editing = None;
                    self.set_typed(field, &text);
                }
                _ => {}
            }
            return;
        }
        if let Some(i) = list::step(view.selected, Field::ALL.len(), key, Nav::FIELDS) {
            view.selected = i;
            view.said = None;
            return;
        }
        match key.code {
            KeyCode::Esc => self.settings_view = None,
            KeyCode::Left | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                let forward = key.code != KeyCode::Left;
                match field.kind() {
                    Kind::Choice | Kind::Toggle => self.change_setting(field, forward),
                    Kind::Number | Kind::Text if key.code == KeyCode::Enter => {
                        view.editing = Some(LineInput::new(typed));
                    }
                    Kind::Number | Kind::Text => {}
                }
            }
            _ => {}
        }
    }

    /// Pasted text goes into the value being typed.
    pub(super) fn paste_setting(&mut self, text: &str) {
        if let Some(input) = self.settings_view.as_mut().and_then(|v| v.editing.as_mut()) {
            input.insert_str(text);
        }
    }

    fn setting_number(&self, field: Field) -> u64 {
        let s = &self.settings;
        match field {
            Field::RowLimit => u64::try_from(s.row_limit).unwrap_or(u64::MAX),
            Field::Keepalive => s.keepalive,
            Field::NotifyAfter => s.notify_after,
            Field::GuardRows => s.guard_rows,
            Field::GatewayIdle => s.gateway_idle,
            _ => 0,
        }
    }

    /// The next (or previous) choice, or the other of on/off.
    fn change_setting(&mut self, field: Field, forward: bool) {
        let s = &mut self.settings;
        let value: Option<toml_edit::Value> = Some(match field {
            Field::Language => {
                // auto, then each language, round.
                let mut codes: Vec<&str> = vec!["auto"];
                codes.extend(crate::lang::LANGUAGES.iter().map(|(c, _, _)| *c));
                let at = codes.iter().position(|c| c.eq_ignore_ascii_case(&s.language)).unwrap_or(0);
                let n = codes.len();
                s.language = codes[if forward { (at + 1) % n } else { (at + n - 1) % n }].to_string();
                crate::lang::choose(&s.language, crate::lang::system().as_deref());
                s.language.as_str().into()
            }
            Field::Theme => {
                s.theme = if s.theme == ThemeName::Dark { ThemeName::Light } else { ThemeName::Dark };
                crate::config::theme::set_theme(s.theme);
                if s.theme == ThemeName::Dark { "dark" } else { "light" }.into()
            }
            Field::EditorMode => {
                s.editor_mode = if s.editor_mode == EditorMode::Vim { EditorMode::Standard } else { EditorMode::Vim };
                if s.editor_mode == EditorMode::Vim { "vim" } else { "standard" }.into()
            }
            Field::Hints => {
                s.hints = if s.hints == Hints::Full { Hints::Minimal } else { Hints::Full };
                if s.hints == Hints::Full { "full" } else { "minimal" }.into()
            }
            Field::Mouse => flip(&mut s.mouse),
            Field::RestoreSession => flip(&mut s.restore_session),
            Field::ServerSyntaxCheck => flip(&mut s.server_syntax_check),
            Field::Lint => flip(&mut s.lint),
            Field::SystemTables => {
                // Toggled the way `.` in the navigator does, the list reloaded.
                self.toggle_system_tables();
                self.settings.system_tables.into()
            }
            Field::LeaderDoubleSpace => flip(&mut s.leader_double_space),
            _ => return,
        });
        self.save_setting(field, value);
    }

    /// A number or text typed in for `field`.
    fn set_typed(&mut self, field: Field, text: &str) {
        let value: Option<toml_edit::Value> = if field == Field::ExportDir {
            // Empty: unset, the setting removed from the file.
            self.settings.export_dir = (!text.is_empty()).then(|| text.to_string());
            (!text.is_empty()).then(|| text.into())
        } else {
            let Some(n) = text.parse::<u64>().ok().filter(|n| field != Field::RowLimit || *n > 0) else {
                if let Some(view) = &mut self.settings_view {
                    view.said = Some((t!("settings.need_number").to_string(), true));
                }
                return;
            };
            let s = &mut self.settings;
            match field {
                Field::RowLimit => s.row_limit = usize::try_from(n).unwrap_or(usize::MAX),
                Field::Keepalive => s.keepalive = n,
                Field::NotifyAfter => s.notify_after = n,
                Field::GuardRows => s.guard_rows = n,
                Field::GatewayIdle => s.gateway_idle = n,
                _ => return,
            }
            Some(i64::try_from(n).unwrap_or(i64::MAX).into())
        };
        self.save_setting(field, value);
    }

    /// Writes `field`'s new value into `config.toml` (nowhere in tests
    /// without a data folder), saying so on the screen.
    fn save_setting(&mut self, field: Field, value: Option<toml_edit::Value>) {
        let said = match &self.data_dir {
            Some(dir) => match save_value(dir, field.key(), value) {
                Ok(()) => (t!("settings.saved").to_string(), false),
                Err(e) => (t!("settings.save_failed", e = e), true),
            },
            None => (t!("settings.saved").to_string(), false),
        };
        if let Some(view) = &mut self.settings_view {
            view.said = Some(said);
        }
    }
}

/// Turns `setting` over; its new value.
fn flip(setting: &mut bool) -> toml_edit::Value {
    *setting = !*setting;
    (*setting).into()
}
