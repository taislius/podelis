//! PostgreSQL's password file (`~/.pgpass`, or `$PGPASSFILE`), read the
//! way libpq does (see `db::passwords`). sqlx reads it too, but before the
//! connection's host, user and database are set, so it would match the
//! defaults instead.

use std::path::{Path, PathBuf};

/// The password for this server, user and database, if the file has one,
/// and the file.
pub(in crate::db) fn password(host: &str, port: u16, user: &str, database: &str) -> Option<(String, PathBuf)> {
    let path = std::env::var_os("PGPASSFILE")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".pgpass")))?;
    lookup(&path, host, port, user, database).map(|password| (password, path))
}

fn lookup(path: &Path, host: &str, port: u16, user: &str, database: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    // Like libpq: a file others can read is ignored.
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!(path = %path.display(), "password file ignored: readable by others (chmod 600 it)");
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    // A Unix socket counts as localhost.
    let host = if host.starts_with('/') || host.is_empty() { "localhost" } else { host };
    let port = port.to_string();
    let wanted = [host, port.as_str(), database, user];
    text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).find_map(|line| {
        let fields = fields(line);
        let [h, p, d, u, password] = fields.as_slice() else { return None };
        let matches = [h, p, d, u].iter().zip(wanted).all(|(field, want)| *field == "*" || *field == want);
        matches.then(|| password.clone())
    })
}

/// A line's `:`-separated fields; `\:` and `\\` stand for `:` and `\`.
fn fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => fields.last_mut().unwrap().extend(chars.next()),
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn matches_like_libpq() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pgpass");
        let text = "# comment\n\
                    localhost:5432:*:me:local\n\
                    db.prod:5432:shop:app:pro\\:d\\\\pw\n\
                    *:*:*:app:any\n";
        std::fs::write(&file, text).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let find = |host, port, user, db| lookup(&file, host, port, user, db);
        assert_eq!(find("db.prod", 5432, "app", "shop").as_deref(), Some(r"pro:d\pw"));
        // The first match wins; wildcards match anything.
        assert_eq!(find("db.prod", 5433, "app", "shop").as_deref(), Some("any"));
        assert_eq!(find("/var/run/postgresql", 5432, "me", "x").as_deref(), Some("local"));
        assert_eq!(find("db.prod", 5432, "me", "shop"), None);
        // Readable by others: ignored, as libpq does.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find("db.prod", 5432, "app", "shop"), None);
    }
}
