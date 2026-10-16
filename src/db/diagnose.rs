//! Connection diagnosis: hints for common connection errors, and a test
//! that connects step by step (SSH, DNS, TCP, login, TLS, session,
//! latency) to show where a connection breaks.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use super::{ConnectParams, Database, Engine, PasswordSource, SslMode, error_message, open_tunnel};

/// Whether a connect failed because of the password (wrong, or none).
/// MySQL says "(using password: …)" only then; its other "access denied"
/// errors (no rights on the database) a password can't fix.
pub fn is_auth_failure(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("password authentication failed")
        || (m.contains("access denied for user") && m.contains("using password:"))
}

/// A plain-language hint for a connection error message, if there is one.
pub fn hint(message: &str) -> Option<&'static str> {
    let m = message.to_lowercase();
    let has = |s: &str| m.contains(s);
    Some(if has("connection refused") {
        "nothing is listening there: is the server running, and are the host and port right?"
    } else if has("host key verification failed") {
        "ssh doesn't know this host yet: connect once with `ssh` from a shell to accept its key"
    } else if has("could not resolve hostname") {
        "ssh can't find that host: check the SSH destination or your ~/.ssh/config"
    } else if has("permission denied (publickey") || (has("ssh") && has("permission denied")) {
        "the SSH key wasn't accepted: check ssh-agent and ~/.ssh/config (Podėlis can't answer password prompts)"
    } else if has("no route to") || has("network is unreachable") {
        "the network can't get there: a VPN that's off, or a host on a network you're not on?"
    } else if has("timed out") || has("timeout") || has("time out") {
        "no answer: a firewall, a VPN that's off, or the wrong host? Through SSH, the host is as seen from the SSH server"
    } else if has("failed to lookup address")
        || has("nodename nor servname")
        || has("name or service not known")
        || has("no such host")
        || has("temporary failure in name resolution")
    {
        "the host name doesn't resolve: check its spelling, your DNS or VPN"
    } else if has("password authentication failed") {
        "wrong or missing password for this user: from the form (or the keychain), the password command, ~/.pgpass or PGPASSWORD"
    } else if has("no pg_hba.conf entry") {
        "the server doesn't let this user/database in from your address: it needs a pg_hba.conf entry (or SSL, if only hostssl is allowed)"
    } else if has("role") && has("does not exist") {
        "there's no such user on the server"
    } else if (has("database") && has("does not exist")) || has("unknown database") {
        "no database by that name: check it, or leave Database empty for the default"
    } else if has("access denied for user") && has("using password: no") {
        "the server wants a password: enter it, give the connection a password command, or put it in ~/.my.cnf (with host = this server, unless it's local)"
    } else if has("access denied for user") {
        "wrong user or password, or this user isn't allowed to connect from your host"
    } else if has("certificate") || has("unknownissuer") || has("invalid peer") {
        "the server's TLS certificate isn't trusted: SSL mode `require` encrypts without verifying it, or trust its CA"
    } else if has("ssl") && (has("required") || has("not support") || has("rejected")) {
        "the server and the SSL mode disagree: try another SSL mode (`require` to insist on TLS, `disable` for none)"
    } else if has("too many connections") || has("remaining connection slots") {
        "the server has no free connections: close some, or raise its connection limit"
    } else if has("starting up") || has("shutting down") || has("in recovery mode") {
        "the server is starting up or shutting down: try again in a moment"
    } else if has("unable to open database file") {
        "can't open that file: check the path and that you may write to its folder"
    } else if has("failed to start ssh") {
        "install OpenSSH (the `ssh` command) to use tunnels"
    } else {
        return None;
    })
}

/// An error message with its hint, if any.
pub fn with_hint(message: &str) -> String {
    match hint(message) {
        Some(h) => format!("{message}\nhint: {h}"),
        None => message.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Ok,
    Warn,
    Failed,
    Skipped,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: &'static str,
    pub state: StepState,
    pub detail: String,
    pub elapsed: Option<Duration>,
    pub hint: Option<&'static str>,
}

impl Step {
    fn new(name: &'static str, state: StepState, detail: impl Into<String>, elapsed: Option<Duration>) -> Self {
        let detail = detail.into();
        let hint = if state == StepState::Failed { hint(&detail) } else { None };
        Step { name, state, detail, elapsed, hint }
    }
}

const STEP_TIMEOUT: Duration = Duration::from_secs(8);

/// Connects step by step, stopping at the first failure.
pub async fn diagnose(mut params: ConnectParams) -> Vec<Step> {
    let mut steps = Vec::new();
    if params.engine.is_file_based() {
        steps.push(file_step(&params.host, params.read_only));
        // Connecting would create a missing file: a test shouldn't.
        if steps.last().is_some_and(|s| s.state != StepState::Ok) {
            return steps;
        }
        login_and_session(&params, &mut steps).await;
        return steps;
    }

    // Where the password comes from: a wrong guess here is the usual
    // reason a login fails.
    let started = Instant::now();
    match params.resolve_password().await {
        Ok((resolved, source)) => {
            params = resolved;
            let (state, detail) = match source {
                PasswordSource::Given => (StepState::Ok, "given (typed, saved or in the URL)".to_string()),
                PasswordSource::Command(program) => (StepState::Ok, format!("from the password command ({program})")),
                PasswordSource::Found(from) => (StepState::Ok, format!("from {from}")),
                PasswordSource::None => (StepState::Skipped, "none (fine if the server doesn't ask)".to_string()),
            };
            steps.push(Step::new("Password", state, detail, Some(started.elapsed())));
        }
        Err(e) => {
            steps.push(Step::new("Password", StepState::Failed, format!("{e:#}"), Some(started.elapsed())));
            return steps;
        }
    }

    // Through SSH, everything after the tunnel goes to its local end.
    let _tunnel = match params.ssh.take().filter(|s| !s.trim().is_empty()) {
        Some(dest) => {
            let started = Instant::now();
            match open_tunnel(&dest, &params.host, params.port).await {
                Ok((tunnel, local)) => {
                    let detail = format!("{dest} → {}:{} (local port {local})", params.host, params.port);
                    steps.push(Step::new("SSH", StepState::Ok, detail, Some(started.elapsed())));
                    params.host = "127.0.0.1".into();
                    params.port = local;
                    Some(tunnel)
                }
                Err(e) => {
                    steps.push(Step::new("SSH", StepState::Failed, format!("{e:#}"), Some(started.elapsed())));
                    return steps;
                }
            }
        }
        None => None,
    };

    let host = params.host.trim().trim_start_matches('[').trim_end_matches(']').to_string();
    if host.starts_with('/') {
        steps.push(Step::new("DNS", StepState::Skipped, "Unix socket", None));
        steps.push(socket_step(&host, params.engine, params.port).await);
    } else {
        let started = Instant::now();
        let lookup = tokio::time::timeout(STEP_TIMEOUT, tokio::net::lookup_host((host.as_str(), params.port))).await;
        let addrs: Vec<std::net::SocketAddr> = match lookup {
            Ok(Ok(addrs)) => addrs.collect(),
            Ok(Err(e)) => {
                steps.push(Step::new("DNS", StepState::Failed, format!("{host}: {e}"), Some(started.elapsed())));
                return steps;
            }
            Err(_) => {
                steps.push(Step::new("DNS", StepState::Failed, format!("{host}: timed out"), Some(started.elapsed())));
                return steps;
            }
        };
        let shown: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).take(3).collect();
        let is_ip = host.parse::<std::net::IpAddr>().is_ok();
        let detail = if is_ip { format!("{host} (an IP address)") } else { format!("{host} → {}", shown.join(", ")) };
        steps.push(Step::new("DNS", StepState::Ok, detail, Some(started.elapsed())));

        let started = Instant::now();
        let mut last_err = String::from("no address");
        let mut connected = None;
        for addr in &addrs {
            match tokio::time::timeout(STEP_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
                Ok(Ok(_)) => {
                    connected = Some(*addr);
                    break;
                }
                Ok(Err(e)) => last_err = format!("{addr}: {e}"),
                Err(_) => last_err = format!("{addr}: timed out after {} s", STEP_TIMEOUT.as_secs()),
            }
        }
        let Some(addr) = connected else {
            steps.push(Step::new("TCP", StepState::Failed, last_err, Some(started.elapsed())));
            return steps;
        };
        steps.push(Step::new("TCP", StepState::Ok, addr.to_string(), Some(started.elapsed())));
    }
    login_and_session(&params, &mut steps).await;
    steps
}

fn file_step(path: &str, read_only: bool) -> Step {
    let path = path.trim();
    if path.is_empty() {
        return Step::new("File", StepState::Failed, "no file given", None);
    }
    if path == ":memory:" {
        return Step::new("File", StepState::Ok, "in-memory database", None);
    }
    let p = std::path::Path::new(path);
    if let Ok(m) = std::fs::metadata(p) {
        return Step::new("File", StepState::Ok, format!("{path} ({} bytes)", m.len()), None);
    }
    let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    if read_only {
        Step::new(
            "File",
            StepState::Failed,
            format!("{path} doesn't exist (read-only connections don't create it)"),
            None,
        )
    } else if dir.is_dir() {
        Step::new("File", StepState::Warn, format!("{path} doesn't exist yet; connecting offers to create it"), None)
    } else {
        Step::new("File", StepState::Failed, format!("folder {} doesn't exist", dir.display()), None)
    }
}

async fn socket_step(path: &str, engine: Engine, port: u16) -> Step {
    let target = engine.dialect().socket_file(path, port);
    let started = Instant::now();
    match tokio::net::UnixStream::connect(&target).await {
        Ok(_) => Step::new("Socket", StepState::Ok, target, Some(started.elapsed())),
        Err(e) => Step::new("Socket", StepState::Failed, format!("{target}: {e}"), Some(started.elapsed())),
    }
}

async fn login_and_session(params: &ConnectParams, steps: &mut Vec<Step>) {
    let started = Instant::now();
    let db = match Database::connect(params.clone()).await {
        Ok(db) => db,
        Err(e) => {
            steps.push(Step::new("Login", StepState::Failed, error_message(&e), Some(started.elapsed())));
            return;
        }
    };
    let who = if params.user.is_empty() { "default user".to_string() } else { params.user.clone() };
    let what = if params.engine.is_file_based() { "opened".to_string() } else { format!("as {who}") };
    steps.push(Step::new("Login", StepState::Ok, what, Some(started.elapsed())));

    if !params.engine.is_file_based() {
        steps.push(match db.tls_info().await {
            Some(tls) => Step::new("TLS", StepState::Ok, tls, None),
            None if params.ssl_mode == SslMode::Disable => {
                Step::new("TLS", StepState::Skipped, "off (SSL mode disable)", None)
            }
            None if params.ssh.is_some() => Step::new("TLS", StepState::Ok, "not used; the SSH tunnel encrypts", None),
            None if db.tls_failed().is_some() => Step::new(
                "TLS",
                StepState::Warn,
                format!(
                    "not encrypted: TLS isn't possible with this server ({}), so `prefer` connected without it. \
                     Set SSL mode to `disable` to skip trying, or connect over SSH",
                    db.tls_failed().unwrap_or_default()
                ),
                None,
            ),
            None => Step::new(
                "TLS",
                StepState::Warn,
                "not encrypted: the server didn't offer TLS (SSL mode `require` refuses that)",
                None,
            ),
        });
    }
    let info = db.session_info().await;
    let pick = |key: &str| info.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let mut session = vec![format!("{} {}", params.engine.label(), db.version())];
    if let Some(u) = pick("User") {
        session.push(format!("user {u}"));
    }
    if let Some(d) = pick("Database").filter(|d| !d.is_empty()) {
        session.push(format!("database {d}"));
    }
    if pick("Read-only").as_deref() == Some("yes") {
        session.push("read-only".into());
    }
    steps.push(Step::new("Session", StepState::Ok, session.join(", "), None));

    let mut times = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        if db.run("SELECT 1", 1).await.is_ok() {
            times.push(t.elapsed());
        }
    }
    if let Some(best) = times.iter().min() {
        let avg = times.iter().sum::<Duration>() / times.len() as u32;
        let detail = format!("{:.1} ms (best {:.1} ms)", avg.as_secs_f64() * 1000.0, best.as_secs_f64() * 1000.0);
        steps.push(Step::new("Latency", StepState::Ok, detail, None));
    }
    db.close().await;
}

/// The steps as text, one per line (for the CLI and the app's view).
pub fn report(steps: &[Step]) -> String {
    let mut out = String::new();
    for s in steps {
        let mark = match s.state {
            StepState::Ok => "✓",
            StepState::Warn => "!",
            StepState::Failed => "✗",
            StepState::Skipped => "-",
        };
        let time = s.elapsed.map_or_else(String::new, |t| format!("  {} ms", t.as_millis()));
        let _ = writeln!(out, "{mark} {:<8} {}{time}", s.name, s.detail);
        if let Some(h) = s.hint {
            let _ = writeln!(out, "  {:<8} hint: {h}", "");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_for_common_errors() {
        assert!(hint("Connection refused (os error 61)").unwrap().contains("listening"));
        assert!(hint("password authentication failed for user \"app\"").unwrap().contains("password"));
        assert!(
            hint("Access denied for user 'root'@'localhost' (using password: NO)")
                .unwrap()
                .contains("wants a password")
        );
        assert!(hint("database \"nope\" does not exist").unwrap().contains("database"));
        assert!(hint("something else").is_none());
        assert!(is_auth_failure("password authentication failed for user \"app\""));
        assert!(is_auth_failure("Access denied for user 'root'@'localhost' (using password: YES)"));
        assert!(!is_auth_failure("Connection refused (os error 61)"));
        assert!(!is_auth_failure("Access denied for user 'app'@'%' to database 'prod'"));
        assert_eq!(with_hint("weird"), "weird");
    }

    #[tokio::test]
    async fn diagnoses_a_refused_port_and_a_sqlite_file() {
        // Port 1 (tcpmux) is practically never served; a freed random
        // port could be taken by a test running in parallel.
        let port = 1;
        let params = ConnectParams {
            engine: Engine::Postgres,
            host: "127.0.0.1".into(),
            port,
            user: String::new(),
            password: String::new(),
            database: String::new(),
            ssl_mode: SslMode::Prefer,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
        };
        let steps = diagnose(params).await;
        let last = steps.last().unwrap();
        assert_eq!((last.name, last.state), ("TCP", StepState::Failed));
        assert!(last.hint.is_some_and(|h| h.contains("listening")), "{steps:?}");

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("d.db");
        let params = ConnectParams {
            engine: Engine::Sqlite,
            host: file.to_string_lossy().into_owned(),
            port: 0,
            user: String::new(),
            password: String::new(),
            database: String::new(),
            ssl_mode: SslMode::Disable,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
        };
        // A missing file: said so, and not created by the test.
        let steps = diagnose(params.clone()).await;
        let names: Vec<_> = steps.iter().map(|s| (s.name, s.state)).collect();
        assert_eq!(names, [("File", StepState::Warn)]);
        assert!(!file.exists());
        // Read-only connections can't create it.
        let read_only = ConnectParams { read_only: true, ..params.clone() };
        let steps = diagnose(read_only).await;
        assert_eq!(steps.iter().map(|s| (s.name, s.state)).collect::<Vec<_>>(), [("File", StepState::Failed)]);
        // An existing (empty) file is a database.
        std::fs::write(&file, "").unwrap();
        let steps = diagnose(params).await;
        let names: Vec<_> = steps.iter().map(|s| (s.name, s.state)).collect();
        assert_eq!(&names[..3], [("File", StepState::Ok), ("Login", StepState::Ok), ("Session", StepState::Ok)]);
        assert!(report(&steps).contains("✓ Latency"));
    }
}
