//! A connection's password command: a program that prints the password,
//! such as `pass show db/prod` or `op read op://vault/db/password`.
//!
//! It runs without a shell (the words are the program and its arguments),
//! with no input, and gets the connection's details in `PODELIS_*`
//! environment variables rather than in its arguments. The first line it
//! prints is the password; it is never logged.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::ConnectParams;

/// Long enough for a password manager to ask for its unlock (in its own
/// window), short enough not to hang a connect forever.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Splits a command as typed into words: spaces separate them, quotes
/// ('…' literally, "…" with \" and \\) and backslashes keep spaces in one.
pub fn split(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("The password command has an unclosed ' quote".to_string()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("The password command has an unclosed \" quote".to_string()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("The password command has an unclosed \" quote".to_string()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next().unwrap_or('\\'));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// The words as one line that [`split`] turns back into them.
pub fn join(words: &[String]) -> String {
    let quote = |w: &String| {
        let plain = !w.is_empty() && !w.chars().any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'));
        if plain { w.clone() } else { format!("'{}'", w.replace('\'', r"'\''")) }
    };
    words.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// Runs the command and returns the password it printed. Blocks: call it
/// off the async runtime.
pub fn run(words: &[String], params: &ConnectParams) -> Result<String> {
    let env = [
        ("PODELIS_ENGINE", params.engine.label().to_string()),
        ("PODELIS_HOST", params.host.clone()),
        ("PODELIS_PORT", params.port.to_string()),
        ("PODELIS_USER", params.user.clone()),
        ("PODELIS_DATABASE", params.database.clone()),
    ];
    run_with(words, &env)
}

/// Runs a command that prints a secret (a password, a model's API key)
/// with `env` set, and returns its first line. Blocks.
pub fn run_with(words: &[String], env: &[(&str, String)]) -> Result<String> {
    let (program, args) = words.split_first().context("the password command is empty")?;
    let mut child = Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        // Without it, gpg (under `pass`) doesn't point a terminal pinentry
        // at the TUI's terminal; a graphical one still works.
        .env_remove("GPG_TTY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group: in the background, so a prompt on the
        // terminal stops it instead of fighting the TUI for keys; and the
        // whole group can be killed, helpers it started included.
        .process_group(0)
        .spawn()
        .with_context(|| format!("couldn't run the password command '{program}'"))?;
    let stdout = Pipe::read(child.stdout.take());
    let stderr = Pipe::read(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        let waited = child.try_wait();
        let timed_out = started.elapsed() > TIMEOUT;
        if let Ok(Some(status)) = waited {
            break status;
        }
        if waited.is_err() || timed_out {
            kill_group(&mut child);
            if let Err(e) = waited {
                return Err(e).context(format!("couldn't wait for the password command '{program}'"));
            }
            bail!(
                "the password command '{program}' didn't finish within {} seconds \
                 (it can't ask for anything in the terminal: use a password manager's own window or agent)",
                TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A helper it left running may keep the pipes open: what was printed
    // by now is what counts.
    let grace = Instant::now() + Duration::from_millis(500);
    let stdout = stdout.finish(grace);
    let stderr = stderr.finish(grace);
    if !status.success() {
        let said = String::from_utf8_lossy(&stderr);
        let said = said.trim().lines().last().unwrap_or_default();
        let code = status.code().map_or_else(|| "was stopped".to_string(), |c| format!("exited with {c}"));
        if said.is_empty() {
            bail!("the password command '{program}' {code}");
        }
        bail!("the password command '{program}' {code}: {said}");
    }
    let text = String::from_utf8(stdout).ok().context("the password command printed something that isn't text")?;
    let password = text.lines().next().unwrap_or_default();
    if password.is_empty() {
        bail!("the password command '{program}' printed no password");
    }
    Ok(password.to_string())
}

/// A password manager with a command line that can serve as a password
/// command, and how a connection named `name` would use it.
pub struct Tool {
    pub program: &'static str,
    pub label: &'static str,
    example: fn(&str) -> Vec<String>,
}

impl Tool {
    /// A password command for a connection named `name`, as typed.
    pub fn example(&self, name: &str) -> String {
        join(&(self.example)(name))
    }
}

/// In the order they're suggested. Missing on purpose: keepassxc-cli reads
/// the database password from its input, which the command doesn't get,
/// and secret-tool needs the Secret Service, the keychain itself.
pub const TOOLS: &[Tool] = &[
    Tool { program: "pass", label: "pass", example: |n| words(["pass", "show", &format!("podelis/{n}")]) },
    Tool { program: "gopass", label: "gopass", example: |n| words(["gopass", "show", "-o", &format!("podelis/{n}")]) },
    Tool {
        program: "op",
        label: "1Password",
        example: |n| words(["op", "read", &format!("op://Private/{n}/password")]),
    },
    Tool { program: "bw", label: "Bitwarden", example: |n| words(["bw", "get", "password", n]) },
    Tool {
        program: "vault",
        label: "Vault",
        example: |n| words(["vault", "kv", "get", "-field=password", &format!("secret/podelis/{n}")]),
    },
];

fn words<const N: usize>(w: [&str; N]) -> Vec<String> {
    w.iter().map(|s| (*s).to_string()).collect()
}

/// The tools that `found` says are installed (see `doctor::on_path`).
pub fn installed(found: impl Fn(&str) -> bool) -> Vec<&'static Tool> {
    TOOLS.iter().filter(|t| found(t.program)).collect()
}

/// Kills the command's process group, then reaps the command.
fn kill_group(child: &mut std::process::Child) {
    // No signal API without unsafe code: kill(1) does it.
    let _ = Command::new("kill")
        .args(["-KILL", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// One of the command's outputs, read on a thread as it comes.
struct Pipe {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

impl Pipe {
    fn read(pipe: Option<impl Read + Send + 'static>) -> Self {
        let pipe_buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let (buf, finished) = (pipe_buf.clone(), done.clone());
        std::thread::spawn(move || {
            if let Some(mut pipe) = pipe {
                let mut chunk = [0u8; 4096];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .extend_from_slice(&chunk[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            }
            finished.store(true, Ordering::Release);
        });
        Self { buf: pipe_buf, done }
    }

    /// What was read, waiting until `deadline` at most for the end.
    fn finish(self, deadline: Instant) -> Vec<u8> {
        while !self.done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::mem::take(&mut *self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Engine, SslMode};

    fn words(text: &str) -> Vec<String> {
        split(text).unwrap()
    }

    #[test]
    fn splits_like_a_shell_without_running_one() {
        assert_eq!(words("pass show db/prod"), ["pass", "show", "db/prod"]);
        assert_eq!(words("  op  read 'op://My Vault/db/password' "), ["op", "read", "op://My Vault/db/password"]);
        assert_eq!(words(r#"echo "a \"b\" c" d\ e ''"#), ["echo", r#"a "b" c"#, "d e", ""]);
        assert_eq!(words("$HOME;rm"), ["$HOME;rm"]);
        assert!(words("").is_empty());
        assert!(split("echo 'open").is_err());
        assert!(split("echo \"open").is_err());
    }

    #[test]
    fn suggests_installed_tools_in_order() {
        let found = installed(|p| matches!(p, "op" | "pass"));
        assert_eq!(found.iter().map(|t| t.program).collect::<Vec<_>>(), ["pass", "op"]);
        assert_eq!(found[0].example("prod"), "pass show podelis/prod");
        // Names with spaces stay one word.
        assert_eq!(found[1].example("my db"), "op read 'op://Private/my db/password'");
        assert!(installed(|_| false).is_empty());
    }

    #[test]
    fn join_round_trips() {
        for text in ["pass show db/prod", "op read 'op://My Vault/x'", r#"echo "it's" '' a\b"#] {
            let w = words(text);
            assert_eq!(split(&join(&w)).unwrap(), w, "{text}");
        }
        assert_eq!(join(&words("pass show db/prod")), "pass show db/prod");
    }

    fn params() -> ConnectParams {
        ConnectParams {
            engine: Engine::Postgres,
            host: "db.example".into(),
            port: 5432,
            user: "app".into(),
            password: String::new(),
            database: "shop".into(),
            ssl_mode: SslMode::Prefer,
            read_only: false,
            ssh: None,
            password_command: Vec::new(),
        }
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn uses_the_first_line_and_passes_the_connection_in_the_environment() {
        let got = run(&sh(r#"printf '%s %s\r\nsecond\n' "$PODELIS_USER" "$PODELIS_HOST:$PODELIS_PORT""#), &params());
        assert_eq!(got.unwrap(), "app db.example:5432");
        // Spaces inside the password are kept.
        assert_eq!(run(&sh("printf ' s3 cret '"), &params()).unwrap(), " s3 cret ");
    }

    #[tokio::test]
    async fn connecting_runs_it_only_without_a_password() {
        let with_command = ConnectParams { password_command: sh("echo from-command"), ..params() };
        assert_eq!(with_command.clone().with_password().await.unwrap().password, "from-command");
        let typed = ConnectParams { password: "typed".into(), ..with_command };
        assert_eq!(typed.with_password().await.unwrap().password, "typed");
        let failing = ConnectParams { password_command: sh("exit 1"), ..params() };
        assert!(failing.with_password().await.is_err());
    }

    #[test]
    fn failures_say_what_happened() {
        let err = |words: Vec<String>| format!("{:#}", run(&words, &params()).unwrap_err());
        assert!(err(sh("echo locked >&2; exit 3")).contains("exited with 3: locked"));
        assert!(err(sh("true")).contains("printed no password"));
        assert!(err(vec!["podelis-no-such-program".into()]).contains("couldn't run"));
        assert!(err(Vec::new()).contains("empty"));
        // A helper left holding the output doesn't keep it waiting.
        let started = Instant::now();
        assert_eq!(run(&sh("(sleep 30 &); echo pw"), &params()).unwrap(), "pw");
        assert!(started.elapsed() < Duration::from_secs(5));
        // No terminal to read from: a prompt sees end of input.
        assert!(err(sh("read -r pw && echo \"$pw\"")).contains("exited with 1"));
    }
}
