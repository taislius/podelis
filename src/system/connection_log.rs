//! What happened to the app's connections, kept in `connections.log` in
//! the config folder, always (not only with `PODELIS_LOG`): opening,
//! reopening, failing (with the libraries' own words, which the screen
//! leaves out), the tables loading, checks finding one lost or back. So a
//! connection that misbehaved yesterday can be looked into today
//! (`podelis doctor` shows the last lines). Never a password or a query.
//!
//! Written on a thread of its own: the screen never waits for the disk.
//!
//! It's the user's alone (mode 600, in the config folder, which is 700),
//! and what could be a secret is taken out before it's written
//! ([`redacted`]): a password in a URL or a `password=` setting, and what
//! a failing password command printed. `connection_log = false` in
//! config.toml keeps none.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::Sender;

/// The log grows to this, then starts again (the one before kept as
/// `connections.log.old`).
const MAX_BYTES: u64 = 512 * 1024;

static LOG: OnceLock<Sender<String>> = OnceLock::new();

pub fn path() -> PathBuf {
    crate::config::store::config_dir().join("connections.log")
}

/// Starts keeping the log (the app's start; tests don't, so they never
/// write to a real config folder).
pub fn init() {
    let path = path();
    let (send, receive) = std::sync::mpsc::channel::<String>();
    if LOG.set(send).is_err() {
        return;
    }
    std::thread::spawn(move || {
        for line in receive {
            write(&path, &line);
        }
    });
}

/// Notes `what` happened to connection `name`: `details` after it.
pub fn record(what: &str, name: &str, details: &str) {
    let Some(log) = LOG.get() else { return };
    let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let details = redacted(&details.replace('\n', " · "));
    let _ = log.send(format!("{at}  {what:<14} {name}  {details}\n"));
}

fn write(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if let Ok(mut file) = options.open(path) {
        // Made readable by others since (a copy, a chmod): the user's again.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if file.metadata().is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
        }
        let _ = file.write_all(line.as_bytes());
    }
}

/// `text` without what could be a secret: the password in a URL
/// (`mysql://user:secret@host` → `mysql://user:***@host`), a
/// `password=…` or `password: …` setting, and what a failing password
/// command printed (its own words could hold anything).
pub fn redacted(text: &str) -> String {
    let mut out = text.to_string();
    // A URL's password.
    let mut from = 0;
    while let Some(at) = out[from..].find("://").map(|i| from + i + 3) {
        // To the URL's end, not its first `/`: a password typed without
        // encoding may hold one (`app:ab/cd@db`), and `@` too, so the last
        // `@` ends it. A user name has no `/`.
        let end = out[at..].find(char::is_whitespace).map_or(out.len(), |i| at + i);
        if let Some(amp) = out[at..end].rfind('@').map(|i| at + i)
            && let Some(colon) = out[at..amp].find(':').map(|i| at + i)
            && !out[at..colon].contains('/')
        {
            out.replace_range(colon + 1..amp, "***");
        }
        from = at.min(out.len());
    }
    // `password=…`, `password: …` (any case), to the next space, `;` or `&`.
    let mut from = 0;
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(at) = lower[from..].find("password").map(|i| from + i + "password".len()) else { break };
        let rest = &out[at..];
        let sep = rest.len() - rest.trim_start_matches([' ', '=', ':']).len();
        // MySQL's `(using password: YES)` says whether one was sent, no more.
        let mysqls = lower[..at - "password".len()].ends_with("using ");
        let is_setting = !mysqls && (rest.starts_with('=') || rest.starts_with(": ") || rest.starts_with(" = "));
        if is_setting {
            let start = at + sep;
            let end = out[start..].find([' ', ';', '&', ',']).map_or(out.len(), |i| start + i);
            if end > start && &out[start..end] != "***" {
                out.replace_range(start..end, "***");
            }
        }
        from = at;
    }
    // A password command's own words: `the password command 'x' exited with 1: …`.
    if let Some(at) = out.find("the password command '")
        && let Some(close) =
            out[at + "the password command '".len()..].find('\'').map(|i| at + "the password command '".len() + i)
        && let Some(colon) = out[close..].find(": ").map(|i| close + i)
    {
        out.truncate(colon);
        out.push_str(": (what it printed is left out)");
    }
    out
}

/// The log's last `n` lines, oldest first (empty if there's none).
pub fn last_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| (*l).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_could_be_a_secret_is_left_out() {
        assert_eq!(
            redacted("can't parse mysql://app:s3cret@db.internal:3306/shop as a URL"),
            "can't parse mysql://app:***@db.internal:3306/shop as a URL"
        );
        assert_eq!(redacted("postgres://db.internal/shop"), "postgres://db.internal/shop", "no password: as it is");
        assert_eq!(redacted("mysql://app:ab/c@d@db:3306/shop failed"), "mysql://app:***@db:3306/shop failed");
        assert_eq!(
            redacted("see http://host/a:b@c"),
            "see http://host/a:b@c",
            "a path's `:` and `@` aren't a password"
        );
        assert_eq!(redacted("host=db password=hunter2 user=app"), "host=db password=*** user=app");
        assert_eq!(redacted("Password: hunter2; port 5432"), "Password: ***; port 5432");
        assert_eq!(
            redacted("the password command 'op' exited with 1: token hunter2 expired"),
            "the password command 'op' exited with 1: (what it printed is left out)"
        );
        // What says nothing secret stays.
        let plain = "couldn't connect: Access denied for user 'root'@'10.0.0.1' (using password: YES)";
        assert_eq!(redacted(plain), plain);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_the_users_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.log");
        write(&path, "one\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Opened to others since: closed again at the next line.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, "two\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn grows_then_starts_again_keeping_the_one_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.log");
        std::fs::write(&path, "x".repeat(usize::try_from(MAX_BYTES).unwrap() + 1)).unwrap();
        write(&path, "one\n");
        write(&path, "two\n");
        assert_eq!(last_lines(&path, 10), ["one", "two"]);
        assert_eq!(last_lines(&path, 1), ["two"]);
        assert!(path.with_extension("log.old").exists());
        assert!(last_lines(&dir.path().join("none.log"), 5).is_empty());
    }
}
