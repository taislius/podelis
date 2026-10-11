//! A password the connection doesn't give (none typed, saved or printed
//! by a password command), found where each database's own clients look:
//! PostgreSQL's `PGPASSWORD` and `~/.pgpass`, MySQL's `MYSQL_PWD` and
//! `~/.my.cnf`.

use std::path::Path;

use sqlx::postgres::PgConnectOptions;

use super::{ConnectParams, Engine};

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
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let found = |password, from: &str| Found { password, from: from.to_string(), user: None };
    let host = params.host.trim().trim_start_matches('[').trim_end_matches(']');
    match params.engine {
        Engine::Postgres => {
            if let Some(password) = env("PGPASSWORD") {
                return Some(found(password, "PGPASSWORD"));
            }
            // Unset fields are the driver's defaults (PGUSER, the OS user;
            // PGDATABASE, else the user's name), as libpq matches them.
            let defaults = PgConnectOptions::new_without_pgpass();
            let user = if params.user.is_empty() { defaults.get_username() } else { &params.user };
            let database =
                if params.database.is_empty() { defaults.get_database().unwrap_or(user) } else { &params.database };
            let (password, path) = super::postgres::pgpass::password(host, params.port, user, database)?;
            Some(found(password, &shown(&path)))
        }
        Engine::MySql => {
            if let Some(password) = env("MYSQL_PWD") {
                return Some(found(password, "MYSQL_PWD"));
            }
            let path = dirs::home_dir()?.join(".my.cnf");
            let (password, user) = my_cnf_password(&path, host, &params.user)?;
            let from = match &user {
                Some(user) => format!("{} (as {user})", shown(&path)),
                None => shown(&path),
            };
            Some(Found { user, ..found(password, &from) })
        }
        Engine::Sqlite => None,
    }
}

/// A path under the home folder as `~/…`.
fn shown(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The password in a MySQL option file's `[client]` or `[mysql]` section,
/// and the user to log in as if the connection names none. Stricter than
/// the mysql client, so a password isn't sent to a server the file wasn't
/// written for: a file that names a `host` is for that server only, one
/// that doesn't is for this machine only, and its `user` must match (or,
/// on the server it names, is the one logged in as). Like the mysql
/// client, a file anyone can write to is ignored. (Not read: `!include`
/// files and the obfuscated `~/.mylogin.cnf`.)
fn my_cnf_password(path: &Path, host: &str, user: &str) -> Option<(String, Option<String>)> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.permissions().mode() & 0o002 != 0 {
        tracing::warn!(path = %path.display(), "option file ignored: anyone can write to it");
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let (mut in_client, mut password, mut file_host, mut file_user) = (false, None, None, None);
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let section = section.trim();
            in_client = section.eq_ignore_ascii_case("client") || section.eq_ignore_ascii_case("mysql");
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_client) else { continue };
        let value = option_value(value.trim());
        match key.trim().replace('_', "-").to_ascii_lowercase().as_str() {
            "password" => password = Some(value),
            "host" => file_host = Some(value),
            "user" => file_user = Some(value),
            _ => {}
        }
    }
    let password = password.filter(|p| !p.is_empty())?;
    // A file that names no host is for this machine: a server elsewhere
    // (perhaps not one you trust) could ask for the password itself.
    let named_host = match file_host {
        Some(h) if same_host(&h, host) => true,
        None if is_local(host) => false,
        Some(_) | None => return None,
    };
    match file_user {
        // Logging in as the file's user only on the server it names.
        Some(named) if user.is_empty() => named_host.then_some((password, Some(named))),
        Some(named) if named != user => None,
        _ => Some((password, None)),
    }
}

/// This machine: `localhost`, a loopback address or a Unix socket.
fn is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1" || host.starts_with('/')
}

/// Host names that reach the same server: equal ignoring case, or both
/// this machine (`localhost`, a loopback address, a Unix socket).
fn same_host(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || (is_local(a) && is_local(b))
}

/// An option's value: quoted ('…' or "…", with backslash escapes) or bare,
/// up to a `#` comment as MySQL reads it.
fn option_value(raw: &str) -> String {
    let Some(quote) = raw.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
        return raw.split('#').next().unwrap_or_default().trim().to_string();
    };
    let mut out = String::new();
    let mut chars = raw[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            c if c == quote => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn file(dir: &tempfile::TempDir, text: &str, mode: u32) -> PathBuf {
        let path = dir.path().join("my.cnf");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn reads_the_client_section() {
        let dir = tempfile::tempdir().unwrap();
        let text = "[mysqld]\npassword = server\n\n[client]\n# comment\nuser = app\npassword = \"s3 \\\"cret\"\n";
        let path = file(&dir, text, 0o600);
        let pw = "s3 \"cret".to_string();
        assert_eq!(my_cnf_password(&path, "localhost", "app"), Some((pw.clone(), None)));
        // Another user than the file names: not its password.
        assert_eq!(my_cnf_password(&path, "localhost", "root"), None);
        // No host named: this machine only.
        assert_eq!(my_cnf_password(&path, "db.example", "app"), None);
        // No user given and no host named: not logging in as the file's user.
        assert_eq!(my_cnf_password(&path, "localhost", ""), None);
        // [mysql] counts too.
        let path = file(&dir, "[mysql]\npassword=pw\n", 0o600);
        assert_eq!(my_cnf_password(&path, "/tmp/mysql.sock", "app"), Some(("pw".to_string(), None)));
    }

    #[test]
    fn a_named_host_limits_it_and_anyone_writable_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "[client]\nhost=db.prod\npassword=pw # note\n", 0o600);
        assert_eq!(my_cnf_password(&path, "DB.prod", "app"), Some(("pw".to_string(), None)));
        assert_eq!(my_cnf_password(&path, "localhost", "app"), None);
        // On the server it names, its user is the one logged in as.
        let path = file(&dir, "[client]\nhost=db.prod\nuser=app\npassword=pw\n", 0o600);
        assert_eq!(my_cnf_password(&path, "db.prod", ""), Some(("pw".to_string(), Some("app".to_string()))));
        // This machine, however it's named.
        let path = file(&dir, "[client]\nhost=localhost\npassword=pw\n", 0o600);
        assert!(my_cnf_password(&path, "127.0.0.1", "app").is_some());
        assert!(my_cnf_password(&path, "/tmp/mysql.sock", "app").is_some());
        let path = file(&dir, "[client]\npassword=pw\n", 0o666);
        assert_eq!(my_cnf_password(&path, "localhost", "app"), None);
    }

    #[test]
    fn values() {
        assert_eq!(option_value("plain # comment"), "plain");
        assert_eq!(option_value("'it''s'"), "it");
        assert_eq!(option_value(r#""a\tb\\c""#), "a\tb\\c");
        assert_eq!(option_value("pw\t# prod"), "pw");
        assert_eq!(option_value("pw#old"), "pw");
        assert_eq!(option_value("'p#ss'"), "p#ss");
    }
}
