//! A connection's settings as typed: what the connect form edits, what a
//! saved profile opens as, and what a URL or file argument fills in. Turns
//! a profile into `ConnectParams` for everyone who connects (the app, the
//! command line, the gateway, agents), so none of them goes through the UI.

use std::time::Duration;

use crate::config::store::ConnectionProfile;
use crate::db::{AgentAccess, ConnectParams, ConnectionMode, Engine, SslMode, TxMode};

/// A connection's fields, as text where the user types them. No `Debug`:
/// it holds the password (`ConnectParams` hides it in its own).
#[derive(Clone)]
pub struct ConnectionDraft {
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    /// The password command as typed (see [`crate::db::password_command`]);
    /// when set, it supplies the password instead of the Password field.
    pub password_command: String,
    pub database: String,
    pub ssl_mode: SslMode,
    pub mode: ConnectionMode,
    /// The transaction mode chosen for the connection; None follows the
    /// mode's default (see [`ConnectionDraft::tx_mode`]).
    pub transaction: Option<TxMode>,
    /// SSH destination to tunnel through; empty for a direct connection.
    pub ssh: String,
    /// Query timeout in seconds, as typed; empty for none.
    pub timeout: String,
    /// What AI agents may do on it (`podelis mcp`).
    pub agents: AgentAccess,
}

impl Default for ConnectionDraft {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionDraft {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            // The first engine (by its `order`): PostgreSQL.
            engine: Engine::BUILT[0],
            host: "localhost".to_string(),
            port: Engine::BUILT[0].default_port().to_string(),
            user: String::new(),
            password: String::new(),
            password_command: String::new(),
            database: String::new(),
            ssl_mode: SslMode::default(),
            mode: ConnectionMode::default(),
            transaction: None,
            ssh: String::new(),
            timeout: String::new(),
            agents: AgentAccess::None,
        }
    }

    pub fn from_profile(profile: &ConnectionProfile, password: String) -> Self {
        Self {
            name: profile.name.clone(),
            engine: profile.engine,
            host: profile.host.clone(),
            port: profile.port.clone(),
            user: profile.user.clone(),
            password,
            password_command: crate::db::password_command::join(&profile.password_command),
            database: profile.database.clone(),
            ssl_mode: profile.ssl_mode,
            mode: profile.mode,
            transaction: profile.transaction,
            ssh: profile.ssh.clone(),
            timeout: profile.timeout.clone(),
            agents: profile.agents,
        }
    }

    /// The draft for a URL or file argument; a saved connection's name
    /// isn't one.
    pub fn from_address(target: &Target) -> Result<Self, String> {
        let mut draft = Self::new();
        match target {
            Target::Url(url) => draft.apply_url(url)?,
            Target::File(path) => {
                let engine = if is_duckdb_file(path) { "duckdb" } else { "sqlite" };
                draft.apply_url(&format!("{engine}:{path}"))?;
            }
            Target::Saved(_) => return Err("not an address".into()),
        }
        Ok(draft)
    }

    pub fn to_profile(&self) -> ConnectionProfile {
        ConnectionProfile {
            unknown: crate::config::store::Unknown::default(),
            name: self.name.trim().to_string(),
            engine: self.engine,
            host: self.host.clone(),
            port: self.port.clone(),
            user: self.user.clone(),
            database: self.database.clone(),
            ssl_mode: self.ssl_mode,
            mode: self.mode,
            // Only a choice: the mode's default follows the mode.
            transaction: self.transaction.filter(|t| *t != TxMode::for_connection(self.mode)),
            ssh: self.ssh.trim().to_string(),
            timeout: self.timeout.trim().to_string(),
            agents: self.agents,
            password_command: self.password_command_words().unwrap_or_default(),
            legacy_keychain: false,
        }
    }

    /// The password command's program and arguments (none for a file).
    pub fn password_command_words(&self) -> Result<Vec<String>, String> {
        if self.engine.is_file_based() {
            return Ok(Vec::new());
        }
        let words = crate::db::password_command::split(&self.password_command)?;
        if words.first().is_some_and(String::is_empty) {
            return Err(t!("connections.empty_command").to_string());
        }
        Ok(words)
    }

    /// The transaction mode the connection starts in: the one chosen, or
    /// the mode's default (manual commit for production).
    pub fn tx_mode(&self) -> TxMode {
        self.transaction.unwrap_or_else(|| TxMode::for_connection(self.mode))
    }

    pub fn cycle_mode(&mut self, forward: bool) {
        self.mode = self.mode.cycle(forward);
    }

    pub fn cycle_transaction(&mut self, forward: bool) {
        let current = self.tx_mode();
        self.transaction = Some(if forward { current.next() } else { current.next().next() });
    }

    pub fn cycle_engine(&mut self, forward: bool) {
        let old = self.engine;
        self.engine = self.engine.cycle(forward);
        if self.port == old.default_port() || self.port.is_empty() {
            self.port = self.engine.default_port().to_string();
        }
        // A server host makes no sense as a file name and vice versa.
        if self.engine.is_file_based() && self.host == "localhost" {
            self.host.clear();
        } else if old.is_file_based() && !self.engine.is_file_based() && self.host.is_empty() {
            self.host = "localhost".to_string();
        }
    }

    /// Fills the draft from a connection URL such as
    /// `postgres://user:pass@host:5432/db?sslmode=require`,
    /// `mysql://user@host/db` or `sqlite:path/to/file.db`.
    pub fn apply_url(&mut self, text: &str) -> Result<(), String> {
        let url = url::Url::parse(text).map_err(|e| t!("connections.bad_url", e = e))?;
        let decode = |s: &str| percent_decode(s).unwrap_or_else(|| s.to_string());
        let engine =
            Engine::from_scheme(url.scheme()).ok_or_else(|| t!("connections.bad_scheme", scheme = url.scheme()))?;
        self.engine = engine;
        if engine.is_file_based() {
            // sqlite:file.db, sqlite://relative.db, sqlite:///abs/path.db
            let rest = text.split_once(':').map_or("", |(_, r)| r);
            let path = rest.strip_prefix("//").unwrap_or(rest);
            let path = path.split('?').next().unwrap_or(path);
            self.host = decode(path);
            self.port.clear();
            return Ok(());
        }
        self.host = url.host_str().unwrap_or("localhost").trim_start_matches('[').trim_end_matches(']').to_string();
        self.port = url.port().map_or_else(|| engine.default_port().to_string(), |p| p.to_string());
        self.user = decode(url.username());
        self.password = url.password().map(decode).unwrap_or_default();
        self.database = decode(url.path().trim_start_matches('/'));
        for (key, value) in url.query_pairs() {
            if matches!(key.as_ref(), "sslmode" | "ssl-mode" | "ssl_mode") {
                self.ssl_mode = match value.to_ascii_lowercase().replace('_', "-").as_str() {
                    "disable" | "disabled" => SslMode::Disable,
                    "require" | "required" => SslMode::Require,
                    "verify-ca" => SslMode::VerifyCa,
                    "verify-full" | "verify-identity" => SslMode::VerifyFull,
                    _ => SslMode::Prefer,
                };
            }
        }
        Ok(())
    }

    /// The query timeout, if one is set.
    pub fn query_timeout(&self) -> Result<Option<Duration>, String> {
        let text = self.timeout.trim().trim_end_matches('s').trim();
        if text.is_empty() {
            return Ok(None);
        }
        match text.parse::<u64>() {
            Ok(0) => Ok(None),
            Ok(secs) => Ok(Some(Duration::from_secs(secs))),
            Err(_) => Err(t!("connections.bad_timeout").to_string()),
        }
    }

    pub fn params(&self) -> Result<ConnectParams, String> {
        self.query_timeout()?;
        let host = self.host.trim();
        let ssh = Some(self.ssh.trim().to_string()).filter(|s| !s.is_empty());
        if self.engine.is_file_based() {
            if host.is_empty() {
                return Err(t!("connections.need_file").to_string());
            }
            return Ok(ConnectParams {
                engine: self.engine,
                host: host.to_string(),
                port: 0,
                user: String::new(),
                password: String::new(),
                database: String::new(),
                ssl_mode: SslMode::Disable,
                read_only: self.mode == ConnectionMode::ReadOnly,
                ssh: None,
                password_command: Vec::new(),
                for_agents: false,
            });
        }
        if host.is_empty() {
            return Err(t!("connections.need_host").to_string());
        }
        let port = match self.port.trim().parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return Err(t!("connections.bad_port").to_string()),
        };
        // A password command replaces the typed (or saved) password.
        let password_command = self.password_command_words()?;
        let password = if password_command.is_empty() { self.password.clone() } else { String::new() };
        Ok(ConnectParams {
            engine: self.engine,
            host: host.to_string(),
            port,
            user: self.user.clone(),
            password,
            database: self.database.trim().to_string(),
            ssl_mode: self.ssl_mode,
            read_only: self.mode == ConnectionMode::ReadOnly,
            ssh,
            password_command,
            for_agents: false,
        })
    }

    /// Its name for the status bar and messages: the saved name, else
    /// the file's name or `host/database`.
    pub fn short_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        if self.engine.is_file_based() {
            return std::path::Path::new(&self.host)
                .file_name()
                .map_or_else(|| self.host.clone(), |n| n.to_string_lossy().into_owned());
        }
        if self.database.is_empty() { self.host.clone() } else { format!("{}/{}", self.host, self.database) }
    }

    pub fn summary(&self) -> String {
        if self.engine.is_file_based() {
            return format!("{} {}", self.engine.label(), self.host);
        }
        let via = if self.ssh.trim().is_empty() { String::new() } else { format!(" via {}", self.ssh.trim()) };
        format!("{} as {}@{}:{}/{}", self.engine.label(), self.user, self.host, self.port, self.database) + &via
    }
}

/// How a CONNECTION argument is meant.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    /// A database file (or `:memory:`): SQLite's, unless it's named as
    /// DuckDB's.
    File(String),
    /// A saved connection's name.
    Saved(String),
}

pub fn classify(arg: &str) -> Target {
    let lower = arg.to_ascii_lowercase();
    if lower.contains("://") || url_engine(arg).is_some() {
        return Target::Url(arg.to_string());
    }
    let looks_like_file =
        [".db", ".sqlite", ".sqlite3", ".db3", ".duckdb", ".ddb"].iter().any(|ext| lower.ends_with(ext))
            || arg == ":memory:"
            || arg.contains('/')
            || std::path::Path::new(arg).is_file();
    if looks_like_file { Target::File(arg.to_string()) } else { Target::Saved(arg.to_string()) }
}

/// A path named as a DuckDB file.
fn is_duckdb_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("duckdb") || ext.eq_ignore_ascii_case("ddb"))
}

/// The engine `text` is a URL for: `scheme://…`, or for an engine of files
/// also `scheme:path`. Every engine's schemes count, built in or not: one
/// that isn't says so when it's opened.
fn url_engine(text: &str) -> Option<Engine> {
    let (scheme, rest) = text.split_once(':')?;
    let engine = Engine::from_scheme(scheme)?;
    (!rest.is_empty() && (engine.is_file_based() || rest.starts_with("//"))).then_some(engine)
}

/// Whether pasted text is a connection URL rather than a field value.
pub fn looks_like_url(text: &str) -> bool {
    url_engine(text.trim()).is_some()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
