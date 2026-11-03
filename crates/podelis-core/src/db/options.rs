use super::*;

// `Engine` and `Engine::ALL` are build.rs's (`db/mod.rs`).
impl Engine {
    pub fn label(self) -> &'static str {
        self.backend().label()
    }

    pub fn default_port(self) -> &'static str {
        self.backend().default_port()
    }

    pub fn cycle(self, forward: bool) -> Engine {
        let idx = Engine::BUILT.iter().position(|e| *e == self).unwrap_or(0);
        let n = Engine::BUILT.len();
        Engine::BUILT[if forward { (idx + 1) % n } else { (idx + n - 1) % n }]
    }

    /// A local file: no server, port, user or TLS.
    pub fn is_file_based(self) -> bool {
        self.capabilities().file_based
    }

    /// Quotes an identifier if it isn't a plain lowercase/underscore name,
    /// in SQL; as it is in a language without SQL's quoting.
    pub fn quote_ident(self, ident: &str) -> String {
        self.dialect().map_or_else(|| ident.to_string(), |d| d.quote_ident(ident))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

const SSL_MODES: [SslMode; 5] =
    [SslMode::Disable, SslMode::Prefer, SslMode::Require, SslMode::VerifyCa, SslMode::VerifyFull];

impl SslMode {
    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Prefer => "prefer",
            SslMode::Require => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyFull => "verify-full",
        }
    }

    pub fn cycle(self, forward: bool) -> SslMode {
        let idx = SSL_MODES.iter().position(|m| *m == self).expect("SSL_MODES lists every mode");
        let n = SSL_MODES.len();
        SSL_MODES[if forward { (idx + 1) % n } else { (idx + n - 1) % n }]
    }
}

/// How carefully the app treats a connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionMode {
    #[default]
    Normal,
    /// Highlighted in red; asks before running anything that modifies data.
    Production,
    /// Refuses statements that modify data, and asks the server to enforce it.
    ReadOnly,
}

const MODES: [ConnectionMode; 3] = [ConnectionMode::Normal, ConnectionMode::Production, ConnectionMode::ReadOnly];

impl ConnectionMode {
    pub fn label(self) -> &'static str {
        match self {
            ConnectionMode::Normal => t!("mode.normal"),
            ConnectionMode::Production => t!("mode.production"),
            ConnectionMode::ReadOnly => t!("mode.read_only"),
        }
    }

    pub fn cycle(self, forward: bool) -> ConnectionMode {
        let idx = MODES.iter().position(|m| *m == self).expect("MODES lists every mode");
        let n = MODES.len();
        MODES[if forward { (idx + 1) % n } else { (idx + n - 1) % n }]
    }
}

/// What AI agents may do on a connection, through `podelis mcp`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentAccess {
    /// Not shared with agents (they don't see it).
    #[default]
    None,
    /// Reads, in a session the server keeps read-only.
    Read,
    /// Writes too: never anything destructive, and on a production
    /// connection only as a dry run.
    Write,
    /// Writes once the user approves each one in Podėlis (shown with its
    /// dry run), production included; never anything destructive.
    Approve,
    /// Writes in a sandbox: one transaction kept open between the agent's
    /// calls, a savepoint before each, never committed; rolled back when
    /// the agent goes (or after a while idle). The user keeps what it did
    /// as a script, from the agent log. Never anything destructive.
    Sandbox,
}

const AGENT_ACCESS: [AgentAccess; 5] =
    [AgentAccess::None, AgentAccess::Read, AgentAccess::Write, AgentAccess::Approve, AgentAccess::Sandbox];

impl AgentAccess {
    pub fn label(self) -> &'static str {
        match self {
            AgentAccess::None => t!("agent_access.none"),
            AgentAccess::Read => t!("agent_access.read"),
            AgentAccess::Write => t!("agent_access.write"),
            AgentAccess::Approve => t!("agent_access.approve"),
            AgentAccess::Sandbox => t!("agent_access.sandbox"),
        }
    }

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "by reference: serde's `skip_serializing_if` passes one")]
    pub fn is_none(&self) -> bool {
        *self == AgentAccess::None
    }

    pub fn cycle(self, forward: bool) -> AgentAccess {
        let idx = AGENT_ACCESS.iter().position(|m| *m == self).unwrap_or(0);
        let n = AGENT_ACCESS.len();
        AGENT_ACCESS[if forward { (idx + 1) % n } else { (idx + n - 1) % n }]
    }
}

/// The connection's transaction mode, chosen with Ctrl+K x.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxMode {
    /// Each statement is committed as it runs.
    #[default]
    AutoCommit,
    /// Writes wait in a transaction for COMMIT or ROLLBACK.
    Manual,
    /// Writes run in a transaction that is always rolled back, to see what
    /// they would change (a dry run).
    RollbackOnly,
    /// Every write runs in one transaction kept open between runs, a
    /// savepoint before each: steps build on each other, the last can be
    /// undone, and nothing is kept until the sandbox is ended (discarded,
    /// kept as a script, or run for real).
    Sandbox,
}

impl TxMode {
    pub const ALL: [TxMode; 4] = [TxMode::AutoCommit, TxMode::Manual, TxMode::RollbackOnly, TxMode::Sandbox];

    pub fn next(self) -> Self {
        match self {
            TxMode::AutoCommit => TxMode::Manual,
            TxMode::Manual => TxMode::RollbackOnly,
            TxMode::RollbackOnly => TxMode::Sandbox,
            TxMode::Sandbox => TxMode::AutoCommit,
        }
    }

    /// Its key in the picker.
    pub fn letter(self) -> char {
        match self {
            TxMode::AutoCommit => 'a',
            TxMode::Manual => 'm',
            TxMode::RollbackOnly => 'r',
            TxMode::Sandbox => 's',
        }
    }

    /// What it does, for the picker.
    pub fn explain(self) -> &'static str {
        match self {
            TxMode::AutoCommit => t!("tx_mode.autocommit_about"),
            TxMode::Manual => t!("tx_mode.manual_about"),
            TxMode::RollbackOnly => t!("tx_mode.rollback_only_about"),
            TxMode::Sandbox => t!("tx_mode.sandbox_about"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TxMode::AutoCommit => t!("tx_mode.autocommit"),
            TxMode::Manual => t!("tx_mode.manual"),
            TxMode::RollbackOnly => t!("tx_mode.rollback_only"),
            TxMode::Sandbox => t!("tx_mode.sandbox"),
        }
    }

    /// Shorter, for the editor's footer.
    pub fn short(self) -> &'static str {
        match self {
            TxMode::Manual => t!("tx_mode.manual_short"),
            _ => self.label(),
        }
    }

    /// The default for a connection's mode: production connections start
    /// in manual commit.
    pub fn for_connection(mode: ConnectionMode) -> Self {
        if mode == ConnectionMode::Production { TxMode::Manual } else { TxMode::AutoCommit }
    }
}

/// Where a connection's password came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordSource {
    /// Typed, saved in the keychain, or in the URL.
    Given,
    /// Printed by the password command (this program).
    Command(String),
    /// The database's own conventions: `PGPASSWORD`, `~/.pgpass`, ….
    Found(String),
    None,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectParams {
    pub engine: Engine,
    /// Hostname, IP address (IPv6 without brackets), or a Unix socket path
    /// starting with `/`.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub ssl_mode: SslMode,
    /// Ask the server to reject writes (best effort; see `ConnectionMode`).
    pub read_only: bool,
    /// Reach the database through `ssh -L` via this host (`user@host`,
    /// `user@host:port`, or an alias from `~/.ssh/config`).
    pub ssh: Option<String>,
    /// Program and arguments that print the password, run when connecting
    /// if `password` is empty (see [`super::password_command`]).
    pub password_command: Vec<String>,
}

/// Everything but the password: a `{:?}` in a log line or a panic mustn't
/// write it out.
impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectParams")
            .field("engine", &self.engine)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &if self.password.is_empty() { "" } else { "<hidden>" })
            .field("database", &self.database)
            .field("ssl_mode", &self.ssl_mode)
            .field("read_only", &self.read_only)
            .field("ssh", &self.ssh)
            .field("password_command", &self.password_command)
            .finish()
    }
}

impl ConnectParams {
    /// With the password to log in with, when none was given: the password
    /// command's, or else from the database's own conventions (environment
    /// variables, password files). Through an SSH tunnel, files are matched
    /// against the server as configured.
    pub async fn with_password(self) -> anyhow::Result<Self> {
        Ok(self.resolve_password().await?.0)
    }

    /// [`Self::with_password`], and where the password came from (for the
    /// connection test).
    pub async fn resolve_password(mut self) -> anyhow::Result<(Self, PasswordSource)> {
        if self.engine.is_file_based() {
            return Ok((self, PasswordSource::None));
        }
        if !self.password.is_empty() {
            return Ok((self, PasswordSource::Given));
        }
        let params = self.clone();
        if let Some(program) = self.password_command.first().cloned() {
            self.password =
                tokio::task::spawn_blocking(move || super::password_command::run(&params.password_command, &params))
                    .await??;
            return Ok((self, PasswordSource::Command(program)));
        }
        match tokio::task::spawn_blocking(move || super::passwords::from_environment(&params)).await? {
            Some(found) => {
                self.password = found.password;
                if self.user.is_empty()
                    && let Some(user) = found.user
                {
                    self.user = user;
                }
                Ok((self, PasswordSource::Found(found.from)))
            }
            None => Ok((self, PasswordSource::None)),
        }
    }

    /// The SQLite file that connecting would create because it doesn't
    /// exist yet: worth asking about, since it's usually a typo. (Read-only
    /// connections fail instead.)
    pub fn file_to_create(&self) -> Option<&str> {
        // The path `Database::connect` opens.
        let path = self.host.trim();
        let missing = self.engine.is_file_based()
            && !self.read_only
            && !path.is_empty()
            && path != ":memory:"
            && !std::path::Path::new(path).exists();
        missing.then_some(path)
    }
}

#[cfg(test)]
mod debug_tests {
    #[test]
    fn debug_output_hides_the_password() {
        let mut params = super::ConnectParams {
            engine: super::Engine::Postgres,
            host: "db".into(),
            port: 5432,
            user: "bob".into(),
            password: "hunter2".into(),
            database: "shop".into(),
            ssl_mode: super::SslMode::Prefer,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
        };
        let shown = format!("{params:?}");
        assert!(!shown.contains("hunter2") && shown.contains("<hidden>") && shown.contains("bob"), "{shown}");
        params.password.clear();
        assert!(!format!("{params:?}").contains("<hidden>"));
    }
}
