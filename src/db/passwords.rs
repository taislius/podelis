//! A password the connection doesn't give (none typed, saved or printed
//! by a password command), found where each database's own clients look:
//! each engine's `passwords.rs` knows where (PostgreSQL's `PGPASSWORD`
//! and `~/.pgpass`, MySQL's `MYSQL_PWD` and `~/.my.cnf`).

use std::path::Path;

use super::ConnectParams;

/// A password found for a connection.
pub struct Found {
    pub password: String,
    /// Where, for the connection test: `PGPASSWORD`, `~/.pgpass`, ….
    pub from: String,
    /// The user it belongs to, when the connection gives none (`~/.my.cnf`
    /// names one): logging in as anyone else with it would fail.
    pub user: Option<String>,
}

pub fn from_environment(params: &ConnectParams) -> Option<Found> {
    let host = params.host.trim().trim_start_matches('[').trim_end_matches(']');
    on_engine!(params.engine, engine::passwords::from_environment(params, host))
}

/// A password found in `from`.
pub(super) fn found(password: String, from: &str) -> Found {
    Found { password, from: from.to_string(), user: None }
}

/// An environment variable's value, if it's set and not empty.
pub(super) fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// A path under the home folder as `~/…`.
pub(super) fn shown(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
