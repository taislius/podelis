//! A connection's settings: one row per field, the focused row
//! highlighted while they're being edited.

use super::*;
use crate::app::{ConnectForm, fields_for};
use crate::db::{AgentAccess, Engine, SslMode};
use crate::store::ConnectionProfile;

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
    let file_based = form.engine.is_file_based();
    fields_for(form.engine)
        .iter()
        // Shown read-only only when set: most connections have none.
        .filter(|&&field| {
            field != ConnectField::PasswordCommand || focus.is_some() || !form.password_command.trim().is_empty()
        })
        .map(|&field| {
            let (label, value) = match field {
                ConnectField::Name => ("Name", form.name),
                ConnectField::Engine => ("Engine", form.engine.label()),
                ConnectField::Host if file_based => ("File", form.host),
                ConnectField::Host => ("Host", form.host),
                ConnectField::Port => ("Port", form.port),
                ConnectField::User => ("User", form.user),
                // The command supplies it: whatever is typed there isn't used.
                ConnectField::Password if !form.password_command.trim().is_empty() => ("Password", "from Passwd cmd"),
                ConnectField::Password => ("Password", password),
                ConnectField::PasswordCommand => ("Passwd cmd", &*form.password_command),
                ConnectField::Database => ("Database", form.database),
                ConnectField::Ssl => ("SSL mode", form.ssl_mode.label()),
                ConnectField::Mode => ("Mode", form.mode.label()),
                // Read-only connections don't write, so there's nothing to commit.
                ConnectField::Transaction if form.mode == ConnectionMode::ReadOnly => {
                    ("Transaction", "none: read-only")
                }
                ConnectField::Transaction => ("Transaction", form.tx_mode.label()),
                ConnectField::Ssh => ("SSH via", form.ssh),
                ConnectField::Timeout => ("Timeout", form.timeout),
                ConnectField::Agents => ("AI agents", form.agents.label()),
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
                Span::styled(format!("{label:>11}  "), if focused { bold(t.accent) } else { muted() }),
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
        ConnectField::Host if form.engine.is_file_based() => "Path to the database file, or :memory:",
        ConnectField::Host => "Hostname, IP address, or a Unix socket path starting with /",
        ConnectField::Mode => match form.mode {
            ConnectionMode::Normal => "Asks before UPDATE/DELETE without WHERE, DROP and TRUNCATE",
            ConnectionMode::Production => "Shown in red; asks before running anything that changes data",
            ConnectionMode::ReadOnly => "Refuses statements that change data; the server enforces it too",
        },
        ConnectField::Transaction if form.mode == ConnectionMode::ReadOnly => {
            "Read-only connections don't change data, so there's nothing to commit"
        }
        ConnectField::Agents => match form.agents {
            AgentAccess::None => "Not shared with AI agents (podelis mcp); ←/→ to share it",
            AgentAccess::Read => "AI agents can read, in a session the server keeps read-only",
            AgentAccess::Write => "AI agents can write too: never DROP or TRUNCATE, and on production only a dry run",
            AgentAccess::Approve => {
                "AI agents can write once you approve each write here (with its dry run): never DROP or TRUNCATE"
            }
        },
        ConnectField::Transaction => match form.tx_mode() {
            TxMode::AutoCommit => "Each statement is committed as it runs (switch any time: Ctrl+K x)",
            TxMode::Manual => "Changes wait for COMMIT (F8) or ROLLBACK (Shift+F8) (switch any time: Ctrl+K x)",
            TxMode::RollbackOnly => "Statements run, then are always rolled back (switch any time: Ctrl+K x)",
        },
        ConnectField::Password if !form.password_command.trim().is_empty() => {
            "Not used: the password command supplies the password (clear Passwd cmd to type one)"
        }
        ConnectField::PasswordCommand => {
            "Optional: a command that prints the password, e.g. pass show db/prod or op read op://vault/db/password. Runs without a shell"
        }
        ConnectField::Timeout => "Optional: seconds a query may run before it's cancelled, e.g. 30",
        ConnectField::Ssh => {
            "Optional: user@host[:port] or an ~/.ssh/config alias. Uses your ssh keys/agent; no password prompts"
        }
        _ => "Tip: paste a connection URL to fill the form · Ctrl+T tests the connection step by step",
    };
    if form.connecting {
        ("Connecting… (Esc to cancel)".to_string(), bold(t.warn))
    } else if let Some(err) = &form.error {
        (err.clone(), fg(t.error))
    } else if let Some(info) = &form.info {
        (info.clone(), fg(t.ok))
    } else {
        (field_help.to_string(), dim())
    }
}
