//! What every engine has, SQL or not: its name, how it's reached (URL
//! schemes, port, socket), what the app may offer on it, how its own
//! connection errors are explained, where its clients keep passwords, and
//! its containers. Each engine implements [`Backend`] in its own module; an
//! SQL engine also implements `SqlDialect` (`dialect.rs`).

use super::Engine;

/// What an engine can do, for the app to decide what to offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "independent yes-or-no capabilities")]
pub struct Capabilities {
    /// A local file: no host, port, user, TLS or SSH.
    pub file_based: bool,
    /// A running query can be cancelled (and so time out).
    pub cancel: bool,
    /// Sessions on the server can be listed and stopped.
    pub activity: bool,
    /// Several schemas, shown as `schema.table`.
    pub schemas: bool,
    /// Stored functions and procedures.
    pub routines: bool,
    /// The server says whether a transaction is open.
    pub server_txn_state: bool,
    /// DDL commits an open transaction (MySQL).
    pub ddl_commits: bool,
    /// The catalog keeps each table's CREATE statement (SQLite).
    pub stores_create_sql: bool,
    /// The catalog knows each table's size on disk (not SQLite).
    pub table_sizes: bool,
    /// A plan with several columns is a table worth a grid (MySQL's classic
    /// EXPLAIN; not SQLite's EXPLAIN QUERY PLAN, a tree).
    pub plan_as_grid: bool,
    /// Rows load fastest with `COPY … FROM STDIN` (PostgreSQL).
    pub copy_in: bool,
}

/// The login an engine's container was given (see
/// `Backend::container_login`).
#[derive(Debug, PartialEq, Eq)]
pub struct ContainerLogin {
    pub user: String,
    pub database: String,
    pub password: Option<ContainerSecret>,
}

/// Where a container's password is: an environment variable, or a file
/// inside it (`*_PASSWORD_FILE`, as with Docker secrets).
#[derive(Debug, PartialEq, Eq)]
pub enum ContainerSecret {
    Var(&'static str),
    File(String),
}

/// What any engine has, apart from talking to the server and from SQL.
pub trait Backend: Sync {
    fn label(&self) -> &'static str;
    /// URL schemes that select this engine.
    fn url_schemes(&self) -> &'static [&'static str];
    fn default_port(&self) -> &'static str {
        ""
    }
    fn capabilities(&self) -> Capabilities;

    /// The socket file to connect to for a Unix socket `path` given in the
    /// form (Postgres takes the folder that holds it).
    fn socket_file(&self, path: &str, _port: u16) -> String {
        path.to_string()
    }
    /// A plain-language hint for one of this engine's own connection errors
    /// (`message` in lower case), as it words them; the network's, SSH's and
    /// TLS's are `diagnose::hint`'s.
    fn connect_hint(&self, _message: &str) -> Option<&'static str> {
        None
    }
    /// Where its own clients find a password, as a note: " (or ~/.pgpass)".
    fn password_files_note(&self) -> &'static str {
        ""
    }
    /// The official container images that run it (`postgres`, `mariadb`).
    fn container_images(&self) -> &'static [&'static str] {
        &[]
    }
    /// The login a container of it was given, from its environment
    /// (`var`: a variable's value); `None` for an engine without containers.
    fn container_login(&self, _var: &dyn Fn(&str) -> Option<String>) -> Option<ContainerLogin> {
        None
    }
}

impl Engine {
    /// Its engine module's backend (`DIALECT`).
    pub fn backend(self) -> &'static dyn Backend {
        on_engine!(self, &engine::DIALECT)
    }

    pub fn capabilities(self) -> Capabilities {
        self.backend().capabilities()
    }

    /// The engine a URL scheme selects.
    pub fn from_scheme(scheme: &str) -> Option<Engine> {
        Engine::ALL.into_iter().find(|e| e.backend().url_schemes().iter().any(|s| s.eq_ignore_ascii_case(scheme)))
    }
}
