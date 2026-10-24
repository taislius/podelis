//! A connection's settings: one row per field, the focused row
//! highlighted while they're being edited.

use super::*;
use crate::app::{ConnectForm, fields_for};
use crate::config::store::ConnectionProfile;
use crate::db::{AgentAccess, Engine, SslMode};

/// A connection's settings to show, borrowed from the form being edited or
/// from a saved connection.
pub(super) struct Settings<'a> {
    name: &'a str,
    engine: Engine,
    host: &'a str,
    port: &'a str,
    user: &'a str,
    /// As typed (the form) or as one line (a saved connection's words).
    password_command: std::borrow::Cow<'a, str>,
    database: &'a str,
    ssl_mode: SslMode,
    mode: ConnectionMode,
    tx_mode: TxMode,
    ssh: &'a str,
    timeout: &'a str,
    agents: AgentAccess,
}

impl<'a> From<&'a ConnectForm> for Settings<'a> {
    fn from(form: &'a ConnectForm) -> Self {
        Settings {
            name: &form.name,
            engine: form.engine,
            host: &form.host,
            port: &form.port,
            user: &form.user,
            password_command: (&*form.password_command).into(),
            database: &form.database,
            ssl_mode: form.ssl_mode,
            mode: form.mode,
            tx_mode: form.tx_mode(),
            ssh: &form.ssh,
            timeout: &form.timeout,
            agents: form.agents,
        }
    }
}

impl<'a> From<&'a ConnectionProfile> for Settings<'a> {
    fn from(p: &'a ConnectionProfile) -> Self {
        Settings {
            name: &p.name,
            engine: p.engine,
            host: &p.host,
            port: &p.port,
            user: &p.user,
            password_command: crate::db::password_command::join(&p.password_command).into(),
            database: &p.database,
            ssl_mode: p.ssl_mode,
            mode: p.mode,
            tx_mode: p.transaction.unwrap_or_else(|| TxMode::for_connection(p.mode)),
            ssh: &p.ssh,
            timeout: &p.timeout,
            agents: p.agents,
        }
    }
}

/// The settings as rows. `focus` is the field being edited (`None` shows
/// them read-only); `password` is how to show the password.
pub(super) fn form_lines(form: &Settings, focus: Option<ConnectField>, password: &str) -> Vec<Line<'static>> {
    let t = theme();
    // The labels line up on the right, as wide as the language's longest.
    let label_w = [
        t!("form.name"),
        t!("form.engine"),
        t!("form.file"),
        t!("form.host"),
        t!("form.port"),
        t!("form.user"),
        t!("form.password"),
        t!("form.password_command"),
        t!("form.database"),
        t!("form.ssl"),
        t!("form.mode"),
        t!("form.transaction"),
        t!("form.ssh"),
        t!("form.timeout"),
        t!("form.agents"),
    ]
    .iter()
    .map(|l| l.chars().count())
    .max()
    .unwrap_or(11);
    let file_based = form.engine.is_file_based();
    fields_for(form.engine)
        .iter()
        // Shown read-only only when set: most connections have none.
        .filter(|&&field| {
            field != ConnectField::PasswordCommand || focus.is_some() || !form.password_command.trim().is_empty()
        })
        .map(|&field| {
            let (label, value) = match field {
                ConnectField::Name => (t!("form.name"), form.name),
                ConnectField::Engine => (t!("form.engine"), form.engine.label()),
                ConnectField::Host if file_based => (t!("form.file"), form.host),
                ConnectField::Host => (t!("form.host"), form.host),
                ConnectField::Port => (t!("form.port"), form.port),
                ConnectField::User => (t!("form.user"), form.user),
                // The command supplies it: whatever is typed there isn't used.
                ConnectField::Password if !form.password_command.trim().is_empty() => {
                    (t!("form.password"), t!("form.from_command"))
                }
                ConnectField::Password => (t!("form.password"), password),
                ConnectField::PasswordCommand => (t!("form.password_command"), &*form.password_command),
                ConnectField::Database => (t!("form.database"), form.database),
                ConnectField::Ssl => (t!("form.ssl"), form.ssl_mode.label()),
                ConnectField::Mode => (t!("form.mode"), form.mode.label()),
                // Read-only connections don't write, so there's nothing to commit.
                ConnectField::Transaction if form.mode == ConnectionMode::ReadOnly => {
                    (t!("form.transaction"), t!("form.no_transaction"))
                }
                ConnectField::Transaction => (t!("form.transaction"), form.tx_mode.label()),
                ConnectField::Ssh => (t!("form.ssh"), form.ssh),
                ConnectField::Timeout => (t!("form.timeout"), form.timeout),
                ConnectField::Agents => (t!("form.agents"), form.agents.label()),
            };
            let toggle =
                matches!(field, ConnectField::Engine | ConnectField::Ssl | ConnectField::Mode | ConnectField::Agents)
                    || (field == ConnectField::Transaction && form.mode != ConnectionMode::ReadOnly);
            let focused = focus == Some(field);
            let value_style = match (field, form.mode) {
                (ConnectField::Mode, ConnectionMode::Production) => bold(t.error),
                (ConnectField::Mode, ConnectionMode::ReadOnly) => bold(t.ok),
                (ConnectField::Password, _) if focus.is_none() => dim(),
                _ if focused => bold(t.text_bright),
                _ => fg(t.text),
            };
            let value = if toggle && focused { format!("‹ {value} ›") } else { value.to_string() };
            let line = Line::from(vec![
                Span::styled(if focused { "▸" } else { " " }, bold(t.accent)),
                Span::styled(format!("{label:>label_w$}  "), if focused { bold(t.accent) } else { muted() }),
                Span::styled(value, value_style),
            ]);
            if focused { line.style(Style::default().bg(t.active)) } else { line }
        })
        .collect()
}

/// Below the form being edited: progress, the last error or outcome, or
/// help for the focused field.
pub(super) fn form_message(form: &ConnectForm) -> (String, Style) {
    let t = theme();
    let field_help = match form.focus {
        ConnectField::Host if form.engine.is_file_based() => t!("form.help_file"),
        ConnectField::Host => t!("form.help_host"),
        ConnectField::Mode => match form.mode {
            ConnectionMode::Normal => t!("form.help_normal"),
            ConnectionMode::Production => t!("form.help_production"),
            ConnectionMode::ReadOnly => t!("form.help_read_only"),
        },
        ConnectField::Transaction if form.mode == ConnectionMode::ReadOnly => {
            t!("form.help_no_transaction")
        }
        ConnectField::Agents => match form.agents {
            AgentAccess::None => t!("form.help_agents_none"),
            AgentAccess::Read => t!("form.help_agents_read"),
            AgentAccess::Write => t!("form.help_agents_write"),
            AgentAccess::Approve => {
                t!("form.help_agents_approve")
            }
            AgentAccess::Sandbox => t!("form.help_agents_sandbox"),
        },
        ConnectField::Transaction => match form.tx_mode() {
            TxMode::AutoCommit => t!("form.help_autocommit"),
            TxMode::Manual => t!("form.help_manual"),
            TxMode::RollbackOnly => t!("form.help_rollback_only"),
            TxMode::Sandbox => t!("form.help_sandbox"),
        },
        ConnectField::Password if !form.password_command.trim().is_empty() => {
            t!("form.help_password_unused")
        }
        ConnectField::PasswordCommand => {
            t!("form.help_password_command")
        }
        ConnectField::Timeout => t!("form.help_timeout"),
        ConnectField::Ssh => {
            t!("form.help_ssh")
        }
        _ => t!("form.help_tip"),
    };
    if form.connecting {
        (t!("form.connecting").to_string(), bold(t.warn))
    } else if let Some(err) = &form.error {
        (err.clone(), fg(t.error))
    } else if let Some(info) = &form.info {
        (info.clone(), fg(t.ok))
    } else {
        (field_help.to_string(), dim())
    }
}
