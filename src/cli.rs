//! The command line: which project or connection to open, and the
//! non-interactive `-e` mode that runs SQL and prints the result.
//!
//! Runs before (or instead of) the TUI, so printing is fine here.

use std::io::{IsTerminal as _, Read as _, Write as _};

use crate::analyze::{self, Kind};
use crate::app::ConnectForm;
use crate::db::{ConnectionMode, Database, QueryOutcome};
use crate::export::{self, Format};
use crate::store::{self, ConnectionStore};

pub const USAGE: &str = "\
Usage: podelis [OPTIONS] [CONNECTION]
       podelis doctor [-p <project>] [--connections]
       podelis mcp [-p <project>] [--name <label>]
       podelis ask [-p <project>] <connection> <question>
       podelis memory [-p <project>] [list [<words>] | add <subject> <what> |
                      forget <id> | confirm <id> | learn <folder> | export]
       podelis gateway [status | start | stop | models]
       podelis gateway key <model> | test [<model>]
       podelis gateway context [-p <project>] <connection> <question>

CONNECTION is a URL (postgres://…, mysql://…, sqlite:…), a SQLite file, or
the name of a saved connection in the project. Without one, Podėlis reopens
your last session.

Options:
  -p, --project <name>   Use this project (default: the last one opened)
  -c, --connection <c>   Same as CONNECTION
  -e, --execute <sql>    Run SQL without the UI and print the result
                         (\"-\" reads it from stdin). CONNECTION defaults
                         to $DATABASE_URL (also for --import, --test)
  -f, --format <fmt>     Output of -e: table, csv, tsv or json (default:
                         table on a terminal, csv otherwise). A script's
                         result sets: each one in table and json (one
                         JSON array per set), only the last in csv/tsv
      --limit <n>        Print at most n rows (-e)
  -y, --yes              Allow -e to change data on production connections
                         and run destructive statements
      --create           Let -e or --import create a SQLite file that
                         doesn't exist yet (a missing one is usually a typo)
      --test             Test CONNECTION step by step (DNS, TCP, login,
                         TLS…) and show where it fails
      --import <file>    Load a CSV/TSV file (or .gz) into a table (created
                         with guessed types if missing); see --table
      --table <name>     Table for --import (default: from the file name)
      --delimiter <c>    Separator for --import: one character, or tab
                         (default: from the file's first line)
      --model <name>     For ask: the model to ask, by its name in
                         config.toml (default: the first)
      --name <label>     For mcp: what to call this agent in the log
                         (default: $PODELIS_AGENT, else the agent's own name)
  -h, --help             Show this help
  -V, --version          Show the version

doctor checks the local setup (config, keychain, ssh, terminal);
--connections also tests the project's saved connections.

ask has an AI model write SQL for a question over a saved connection's
schema and prints it, never running it (the first model in config.toml,
see gateway models). Only the tables that matter are shown to the model,
never rows, and its SQL is checked against the schema. A model that isn't
on this machine is shown a project's schema only once you allow it.

memory is what the project's team has said about its data, shown to
models when they write SQL: `podelis memory add orders.status 3 =
cancelled`, `add revenue is orders.total minus refunds`. list shows the
facts (or those matching words), forget removes one by its number,
confirm makes a model's suggestion count, learn reads a folder of SQL
migrations (each table's history, and the comments above statements),
export prints them all as text to keep or review.

mcp serves the connections shared with AI agents (\"AI agents\" in a
connection's settings) over MCP on stdin and stdout: add `podelis mcp` to
your agent as an MCP server (it hands the agent to the gateway, started
if needed). Each agent call runs in a read-only transaction unless a
connection allows writes; what agents run is logged and shows in the
query library (F3).

gateway runs the gateway in the foreground until Ctrl+C or it's idle
(`gateway_idle` in config.toml): the background process Podėlis starts
by itself when a feature needs it, the one place that talks to AI
models. status says whether it runs; start starts it in the background;
stop stops it. models lists the models set up in config.toml
([[models]]) and whether each has its key; key <model> stores a model's
API key in the OS keychain; test asks a model for one word, to check it
answers (it costs a few tokens). context shows what a model would be
shown of a saved connection's database for a question: the tables that
matter, why, and their columns, keys and comments (nothing is sent).

Exit codes of -e, --import and --test: 0 done, 1 the SQL failed, 2 usage
(bad option, unknown connection, missing file), 3 refused (read-only,
production or destructive without --yes), 4 couldn't connect, 5 timed out.";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub project: Option<String>,
    pub connection: Option<String>,
    pub execute: Option<String>,
    pub format: Option<Format>,
    pub limit: Option<usize>,
    pub yes: bool,
    /// Create a SQLite file that doesn't exist (`-e`, `--import`).
    pub create: bool,
    pub test: bool,
    pub import: Option<String>,
    pub table: Option<String>,
    /// `--delimiter`: the separator for `--import`.
    pub delimiter: Option<u8>,
    pub doctor: bool,
    /// `mcp`: serve the shared connections to an AI agent.
    pub mcp: bool,
    /// `mcp --name`: what to call the agent in the log.
    pub name: Option<String>,
    /// `doctor --connections`.
    pub test_connections: bool,
    /// `gateway [status | start | stop]`.
    pub gateway: Option<GatewayCommand>,
    /// `ask <connection> <question…>`: the words after `ask`.
    pub ask: Option<Vec<String>>,
    /// `ask --model`: which model.
    pub model: Option<String>,
    /// `memory …`: the words after `memory`.
    pub memory: Option<Vec<String>>,
}

/// What `podelis gateway` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayCommand {
    /// Run it here, in the foreground.
    Run,
    Status,
    Start,
    Stop,
    Models,
    /// Store a model's key (its name; empty until given).
    Key(String),
    /// Ask a model (the default one when `None`) for one word.
    Test(Option<String>),
    /// What a model would be shown for a question: the connection, then
    /// the question's words.
    Context(Vec<String>),
}

/// What to do instead of starting normally.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    Run(Box<Args>),
    Exit(i32, String),
}

pub fn parse(args: impl IntoIterator<Item = String>) -> ParseOutcome {
    let mut out = Args::default();
    let mut args = args.into_iter();
    let usage_error = |msg: String| ParseOutcome::Exit(2, format!("{msg}\n\n{USAGE}"));
    while let Some(arg) = args.next() {
        if gateway_word(&mut out, &arg) {
            continue;
        }
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let result: Result<(), String> = match arg.as_str() {
            "-h" | "--help" => return ParseOutcome::Exit(0, USAGE.to_string()),
            "-V" | "--version" => return ParseOutcome::Exit(0, format!("podelis {}", env!("CARGO_PKG_VERSION"))),
            "-p" | "--project" => value(&arg).map(|v| out.project = Some(v)),
            "-c" | "--connection" => value(&arg).map(|v| out.connection = Some(v)),
            "-e" | "--execute" => value(&arg).map(|v| out.execute = Some(v)),
            "-y" | "--yes" => {
                out.yes = true;
                Ok(())
            }
            "--create" => {
                out.create = true;
                Ok(())
            }
            "--import" => value(&arg).map(|v| out.import = Some(v)),
            "--table" => value(&arg).map(|v| out.table = Some(v)),
            "--name" => value(&arg).map(|v| out.name = Some(v)),
            "--model" => value(&arg).map(|v| out.model = Some(v)),
            "--delimiter" => value(&arg).and_then(|v| {
                out.delimiter = Some(match v.as_str() {
                    "tab" | "\\t" | "\t" => b'\t',
                    v if v.len() == 1 && v.is_ascii() => v.as_bytes()[0],
                    _ => return Err(format!("--delimiter takes one character or tab, not '{v}'")),
                });
                Ok(())
            }),
            "--test" => {
                out.test = true;
                Ok(())
            }
            "--connections" => {
                out.test_connections = true;
                Ok(())
            }
            "doctor" if !out.doctor && out.connection.is_none() => {
                out.doctor = true;
                Ok(())
            }
            "mcp" if !out.mcp && out.connection.is_none() => {
                out.mcp = true;
                Ok(())
            }
            "-f" | "--format" => value(&arg).and_then(|v| {
                out.format = Some(match v.as_str() {
                    "table" => Format::Table,
                    "csv" => Format::Csv,
                    "tsv" => Format::Tsv,
                    "json" => Format::Json,
                    _ => return Err(format!("unknown format '{v}' (table, csv, tsv, json)")),
                });
                Ok(())
            }),
            "--limit" => value(&arg).and_then(|v| {
                out.limit = Some(v.parse().map_err(|_| format!("--limit needs a number, not '{v}'"))?);
                Ok(())
            }),
            other if other.starts_with('-') && other != "-" => Err(format!("unknown option: {other}")),
            other if out.connection.is_none() => {
                out.connection = Some(other.to_string());
                Ok(())
            }
            other => Err(format!("unexpected argument: {other}")),
        };
        if let Err(e) = result {
            return usage_error(e);
        }
    }
    if let Err(e) = check_subcommands(&out) {
        return usage_error(e);
    }
    ParseOutcome::Run(Box::new(out))
}

/// What a subcommand still needs after all the words are read.
fn check_subcommands(out: &Args) -> Result<(), String> {
    if out.gateway == Some(GatewayCommand::Key(String::new())) {
        return Err("gateway key needs the model's name (as in config.toml)".into());
    }
    if let Some(words) = &out.memory {
        let enough = match words.first().map(String::as_str) {
            None | Some("list" | "export") => true,
            Some("add") => words.len() >= 3,
            Some("forget" | "confirm") => words.get(1).is_some_and(|id| id.parse::<i64>().is_ok()),
            Some("learn") => words.len() == 2,
            Some(other) => {
                return Err(format!("memory: no '{other}' (list, add, forget, confirm, learn, export)"));
            }
        };
        if !enough {
            return Err("memory add needs a subject and what's said about it; forget and confirm, a fact's number; \
                 learn, a folder"
                .into());
        }
    }
    if out.ask.as_ref().is_some_and(|words| words.len() < 2) {
        return Err("ask needs a saved connection's name and a question".into());
    }
    if matches!(&out.gateway, Some(GatewayCommand::Context(words)) if words.len() < 2) {
        return Err("gateway context needs a saved connection's name and a question".into());
    }
    Ok(())
}

/// `gateway` or `ask` and the words after them; false if `word` isn't
/// one of them.
fn gateway_word(out: &mut Args, word: &str) -> bool {
    let positional = !word.starts_with('-');
    if word == "ask" && out.ask.is_none() && out.memory.is_none() && out.gateway.is_none() && out.connection.is_none() {
        out.ask = Some(Vec::new());
        return true;
    }
    if word == "memory"
        && out.memory.is_none()
        && out.ask.is_none()
        && out.gateway.is_none()
        && out.connection.is_none()
    {
        out.memory = Some(Vec::new());
        return true;
    }
    if let Some(words) = out.ask.as_mut().or(out.memory.as_mut()) {
        if positional {
            words.push(word.to_string());
        }
        return positional;
    }
    match (&mut out.gateway, word) {
        (None, "gateway") if out.connection.is_none() => out.gateway = Some(GatewayCommand::Run),
        (Some(GatewayCommand::Run), "status") => out.gateway = Some(GatewayCommand::Status),
        (Some(GatewayCommand::Run), "start") => out.gateway = Some(GatewayCommand::Start),
        (Some(GatewayCommand::Run), "stop") => out.gateway = Some(GatewayCommand::Stop),
        (Some(GatewayCommand::Run), "models") => out.gateway = Some(GatewayCommand::Models),
        (Some(GatewayCommand::Run), "key") => out.gateway = Some(GatewayCommand::Key(String::new())),
        (Some(GatewayCommand::Run), "test") => out.gateway = Some(GatewayCommand::Test(None)),
        (Some(GatewayCommand::Run), "context") => out.gateway = Some(GatewayCommand::Context(Vec::new())),
        (Some(GatewayCommand::Key(name)), _) if positional && name.is_empty() => *name = word.to_string(),
        (Some(GatewayCommand::Test(name @ None)), _) if positional => *name = Some(word.to_string()),
        (Some(GatewayCommand::Context(words)), _) if positional => words.push(word.to_string()),
        _ => return false,
    }
    true
}

/// How a CONNECTION argument is meant.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    /// A SQLite database file (or `:memory:`).
    File(String),
    /// A saved connection's name.
    Saved(String),
}

pub fn classify(arg: &str) -> Target {
    let lower = arg.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("sqlite:") {
        return Target::Url(arg.to_string());
    }
    let looks_like_file = [".db", ".sqlite", ".sqlite3", ".db3"].iter().any(|ext| lower.ends_with(ext))
        || arg == ":memory:"
        || arg.contains('/')
        || std::path::Path::new(arg).is_file();
    if looks_like_file { Target::File(arg.to_string()) } else { Target::Saved(arg.to_string()) }
}

/// The connect form for a URL or file target.
pub fn form_for_address(target: &Target) -> Result<ConnectForm, String> {
    let mut form = ConnectForm::new();
    match target {
        Target::Url(url) => form.apply_url(url)?,
        Target::File(path) => form.apply_url(&format!("sqlite:{path}"))?,
        Target::Saved(_) => return Err("not an address".into()),
    }
    Ok(form)
}

/// Exit codes of `-e`, `--import` and `--test`, so a script can tell what
/// went wrong (and whether trying again could help).
pub mod exit {
    /// The SQL (or the import) failed on the server.
    pub const FAILED: i32 = 1;
    /// Bad arguments, an unknown project or connection, a missing file.
    pub const USAGE: i32 = 2;
    /// Refused by the connection's mode or as destructive (see `--yes`).
    pub const REFUSED: i32 = 3;
    /// Couldn't connect (network, login, keychain, SSH).
    pub const CONNECT: i32 = 4;
    /// The connection's query timeout ran out.
    pub const TIMEOUT: i32 = 5;
}

/// Why `-e` or `--import` stopped: the exit code and the message.
#[derive(Debug)]
struct Failure(i32, String);

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self(exit::USAGE, message.into())
    }

    fn refused(message: impl Into<String>) -> Self {
        Self(exit::REFUSED, message.into())
    }

    fn failed(message: impl Into<String>) -> Self {
        Self(exit::FAILED, message.into())
    }

    fn connect(message: impl Into<String>) -> Self {
        Self(exit::CONNECT, message.into())
    }

    /// A query's error: the connection broke (retrying may help), or the
    /// server refused the SQL.
    fn from_query(err: &anyhow::Error) -> Self {
        let message = crate::db::error_message(err);
        if crate::db::is_broken(err) { Self::connect(message) } else { Self::failed(message) }
    }

    /// Prints the message (with a hint) and returns the exit code.
    fn report(self) -> i32 {
        let _ = writeln!(std::io::stderr(), "podelis: {}", crate::db::with_hint(&self.1));
        self.0
    }
}

/// `-e`: connects, runs the SQL, prints the result. Returns the exit code.
pub async fn execute(args: &Args, store: &ConnectionStore) -> i32 {
    execute_inner(args, store).await.map_or_else(Failure::report, |()| 0)
}

async fn execute_inner(args: &Args, store: &ConnectionStore) -> Result<(), Failure> {
    let sql = match args.execute.as_deref() {
        Some("-") => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| Failure::usage(format!("reading stdin: {e}")))?;
            text
        }
        Some(sql) => sql.to_string(),
        None => return Err(Failure::usage("nothing to execute")),
    };
    let form = resolve(args, store)?;
    let engine = form.engine;
    let infos = analyze::analyze(&crate::parse::parse(&sql, engine));
    if let Some(write) = infos.iter().find(|i| i.modifies())
        && form.mode == ConnectionMode::ReadOnly
    {
        return Err(Failure::refused(format!(
            "read-only connection: not running “{}” (it changes data)",
            write.summary
        )));
    }
    if !args.yes {
        if let Some(risky) = infos.iter().find(|i| i.risk.is_some()) {
            return Err(Failure::refused(format!("“{}” looks destructive; add --yes to run it", risky.summary)));
        }
        if form.mode == ConnectionMode::Production && infos.iter().any(analyze::StatementInfo::modifies) {
            return Err(Failure::refused("production connection: add --yes to change data"));
        }
    }
    let db = connect(&form, args).await?;
    let limit = args.limit.unwrap_or(usize::MAX);
    let timeout = form.query_timeout().map_err(Failure::usage)?;
    let result = db.run_within(&sql, limit, timeout, true).await;
    db.close().await;
    let outcome = result.map_err(|e| match e {
        crate::db::RunError::TimedOut(limit) => {
            Failure(exit::TIMEOUT, format!("timed out after {} s (the connection's limit)", limit.as_secs()))
        }
        crate::db::RunError::Failed(e) => Failure::from_query(&e),
    })?;
    let format = args.format.unwrap_or(if std::io::stdout().is_terminal() { Format::Table } else { Format::Csv });
    let mut out = std::io::stdout().lock();
    if outcome.columns.is_empty() {
        let affected = outcome.rows_affected.unwrap_or(0);
        let ran = infos.iter().filter(|i| i.kind != Kind::Read).count().max(1);
        let _ = writeln!(std::io::stderr(), "OK: {ran} statement(s), {affected} row(s) affected");
        return Ok(());
    }
    let text = render_all(&outcome, format);
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        // A closed pipe (`| head`) isn't a failure of the query; a full disk is.
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            return Err(Failure::failed(format!("writing the result: {e}")));
        }
        _ => {}
    }
    if matches!(format, Format::Csv | Format::Tsv) && !outcome.earlier.is_empty() {
        let n = outcome.earlier.len();
        let _ = writeln!(
            std::io::stderr(),
            "(printed the last result set only; {n} earlier one(s) need -f json or -f table)"
        );
    }
    for set in printed_sets(&outcome, format).into_iter().filter(|s| s.truncated()) {
        let which = if set.statement > 0 { format!(" of statement {}", set.statement) } else { String::new() };
        let _ = writeln!(std::io::stderr(), "(first {} of {} rows{which})", set.rows.len(), set.total_rows);
    }
    Ok(())
}

/// Every result set of a script, as far as the format can hold them: a
/// table and JSON show each one (JSON as one document per set, which `jq`
/// reads as a stream), CSV and TSV only the last, so they stay one table.
fn render_all(outcome: &QueryOutcome, format: Format) -> String {
    let sets = printed_sets(outcome, format);
    let separator = if format == Format::Table { "\n" } else { "" };
    sets.iter().map(|set| export::render_all_rows(set, format)).collect::<Vec<_>>().join(separator)
}

/// The result sets of a script that `format` prints.
fn printed_sets(outcome: &QueryOutcome, format: Format) -> Vec<&QueryOutcome> {
    match format {
        Format::Table | Format::Json => outcome.earlier.iter().chain(std::iter::once(outcome)).collect(),
        Format::Csv | Format::Tsv => vec![outcome],
    }
}

/// Connects for `-e` or `--import`. A wrong or missing password is asked
/// for once, as psql does, when someone is at the terminal.
async fn connect(form: &ConnectForm, args: &Args) -> Result<Database, Failure> {
    let params = params_creating(form, args).map_err(Failure::usage)?;
    let message = match Database::connect(params.clone()).await {
        Ok(db) => return Ok(db),
        Err(e) => crate::db::error_message(&e),
    };
    // A password command's password is its to fix, not ours to replace.
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !(crate::db::is_auth_failure(&message) && params.password_command.is_empty() && interactive) {
        return Err(Failure::connect(message));
    }
    let _ = writeln!(std::io::stderr(), "podelis: {message}");
    // No user: the driver's default (the OS user for Postgres).
    let label = if params.user.is_empty() {
        format!("Password for {}: ", params.host)
    } else {
        format!("Password for {}@{}: ", params.user, params.host)
    };
    let Some(password) = tokio::task::spawn_blocking(move || prompt_password(&label)).await.ok().flatten() else {
        return Err(Failure::connect("no password entered"));
    };
    let retry = crate::db::ConnectParams { password, ..params };
    Database::connect(retry).await.map_err(|e| Failure::connect(crate::db::error_message(&e)))
}

/// Reads a password from the terminal without showing it; None if
/// cancelled (Esc, Ctrl+C) or the terminal can't be put in raw mode.
pub(crate) fn prompt_password(label: &str) -> Option<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    let mut err = std::io::stderr();
    let _ = write!(err, "{label}");
    let _ = err.flush();
    /// Leaves raw mode however the prompt ends, a panic included.
    struct Raw;
    impl Drop for Raw {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = writeln!(std::io::stderr());
        }
    }
    enable_raw_mode().ok()?;
    let raw = Raw;
    let mut password = String::new();
    let entered = loop {
        // The terminal went away (a dropped SSH session): give up.
        let Ok(event) = read() else { break false };
        let Event::Key(key) = event else { continue };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => break true,
            KeyCode::Esc => break false,
            KeyCode::Char('c' | 'd') if ctrl => break false,
            KeyCode::Char('u' | 'w') if ctrl => password.clear(),
            // Other Ctrl/Alt keys aren't typing: never part of the password.
            KeyCode::Char(_) if ctrl || key.modifiers.contains(KeyModifiers::ALT) => {}
            KeyCode::Char(c) => password.push(c),
            KeyCode::Backspace => {
                password.pop();
            }
            _ => {}
        }
    };
    drop(raw);
    (entered && !password.is_empty()).then_some(password)
}

/// The form's connection parameters, refusing to create a missing SQLite
/// file (usually a typo) unless `--create` says so.
fn params_creating(form: &ConnectForm, args: &Args) -> Result<crate::db::ConnectParams, String> {
    let params = form.params()?;
    if let Some(path) = params.file_to_create()
        && !args.create
    {
        return Err(format!("{path} doesn't exist; add --create to make a new database there"));
    }
    Ok(params)
}

/// The connection the arguments name (or `$DATABASE_URL`).
fn resolve(args: &Args, store: &ConnectionStore) -> Result<ConnectForm, Failure> {
    let connection =
        args.connection
            .clone()
            .or_else(|| std::env::var("DATABASE_URL").ok().filter(|u| !u.is_empty()))
            .ok_or_else(|| Failure::usage("no connection: pass one (URL, file or saved name) or set DATABASE_URL"))?;
    match classify(&connection) {
        Target::Saved(name) => saved_form(store, args.project.as_deref(), &name),
        address => form_for_address(&address).map_err(Failure::usage),
    }
}

/// `--import`: loads a CSV/TSV file into a table.
pub async fn import(args: &Args, store: &ConnectionStore) -> i32 {
    import_inner(args, store).await.map_or_else(Failure::report, |()| 0)
}

async fn import_inner(args: &Args, store: &ConnectionStore) -> Result<(), Failure> {
    let file = std::path::PathBuf::from(args.import.as_deref().unwrap_or_default());
    // Before connecting, so a typo doesn't leave a new empty database behind.
    std::fs::File::open(&file).map_err(|e| Failure::usage(format!("can't read {}: {e}", file.display())))?;
    let form = resolve(args, store)?;
    if form.mode == ConnectionMode::ReadOnly {
        return Err(Failure::refused("read-only connection: not importing"));
    }
    if form.mode == ConnectionMode::Production && !args.yes {
        return Err(Failure::refused("production connection: add --yes to import"));
    }
    let table = args.table.clone().unwrap_or_else(|| crate::import::table_name_for(&file));
    let db = connect(&form, args).await?;
    // On a terminal, a line that keeps saying how far it's got.
    let terminal = std::io::stderr().is_terminal();
    let mut shown: Option<std::time::Instant> = None;
    let mut show = |p: crate::import::Progress| {
        let end = p.bytes >= p.size;
        if terminal && (end || shown.is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(100))) {
            let _ = write!(std::io::stderr(), "\r\x1b[K{p}");
            shown = Some(std::time::Instant::now());
        }
    };
    let result = crate::import::import(&db, &file, &table, args.delimiter, &mut show).await;
    if shown.is_some() {
        let _ = write!(std::io::stderr(), "\r\x1b[K");
    }
    db.close().await;
    let done = result.map_err(|e| Failure::from_query(&e))?;
    let how = if done.created { "created" } else { "existing" };
    let _ = writeln!(std::io::stderr(), "Imported {} row(s) into {table} ({how} table)", done.rows);
    Ok(())
}

/// `--test`: connects step by step and prints where it breaks.
pub async fn test(args: &Args, store: &ConnectionStore) -> i32 {
    let params = match resolve(args, store).and_then(|f| f.params().map_err(Failure::usage)) {
        Ok(p) => p,
        Err(e) => return e.report(),
    };
    let steps = crate::db::diagnose(params).await;
    let _ = std::io::stdout().write_all(crate::db::report(&steps).as_bytes());
    if steps.iter().any(|s| s.state == crate::db::StepState::Failed) { exit::CONNECT } else { 0 }
}

/// A saved connection of the project (the last one opened by default),
/// with its password from the keychain. A keychain that can't be read is
/// only a warning: the database's own sources may have the password.
fn saved_form(store: &ConnectionStore, project: Option<&str>, name: &str) -> Result<ConnectForm, Failure> {
    let project = match project {
        Some(p) => store.project_by_name(p).ok_or_else(|| Failure::usage(format!("no project named '{p}'")))?,
        None => store
            .last_project
            .as_deref()
            .and_then(|id| store.project(id))
            .ok_or_else(|| Failure::usage("no project to look the connection up in: pass -p <project>"))?,
    };
    let profile = project
        .connections
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| Failure::usage(format!("no connection named '{name}' in project '{}'", project.name)))?;
    let (password, note) = store::saved_password(&project.id, profile);
    if let Some(note) = note {
        let _ = writeln!(std::io::stderr(), "podelis: no saved password ({note}); connecting without it");
    }
    Ok(ConnectForm::from_profile(profile, password))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> ParseOutcome {
        parse(list.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn parses_arguments() {
        let ParseOutcome::Run(a) = args(&["-p", "shop", "prod", "-e", "select 1", "-f", "json", "--limit", "5", "-y"])
        else {
            panic!()
        };
        assert_eq!(a.project.as_deref(), Some("shop"));
        assert_eq!(a.connection.as_deref(), Some("prod"));
        assert_eq!(
            (a.execute.as_deref(), a.format, a.limit, a.yes),
            (Some("select 1"), Some(Format::Json), Some(5), true)
        );
        assert!(matches!(args(&["--frob"]), ParseOutcome::Exit(2, _)));
        assert!(matches!(args(&["-f", "xml"]), ParseOutcome::Exit(2, _)));
        assert!(matches!(args(&["a", "b"]), ParseOutcome::Exit(2, _)));
        assert!(matches!(args(&["-V"]), ParseOutcome::Exit(0, v) if v.starts_with("podelis ")));
        let delimiter = |d: &str| match args(&["--delimiter", d]) {
            ParseOutcome::Run(a) => a.delimiter,
            ParseOutcome::Exit(..) => None,
        };
        assert_eq!([";", "|", "tab", "\\t", "\t"].map(delimiter), (*b";|\t\t\t").map(Some));
        assert!(matches!(args(&["--delimiter", "ab"]), ParseOutcome::Exit(2, _)));
    }

    #[test]
    fn gateway_words() {
        let run = |args: &[&str]| match parse(args.iter().map(|a| (*a).to_string())) {
            ParseOutcome::Run(a) => Ok(a),
            ParseOutcome::Exit(code, text) => Err((code, text)),
        };
        assert_eq!(run(&["gateway"]).unwrap().gateway, Some(GatewayCommand::Run));
        assert_eq!(run(&["gateway", "stop"]).unwrap().gateway, Some(GatewayCommand::Stop));
        assert_eq!(
            run(&["gateway", "test", "local"]).unwrap().gateway,
            Some(GatewayCommand::Test(Some("local".into())))
        );
        let ctx = run(&["-p", "shop", "gateway", "context", "prod", "orders", "by", "month"]).unwrap();
        assert_eq!(ctx.project.as_deref(), Some("shop"));
        let words = ["prod", "orders", "by", "month"].map(String::from).to_vec();
        assert_eq!(ctx.gateway, Some(GatewayCommand::Context(words)));
        let (code, text) = run(&["gateway", "key"]).unwrap_err();
        assert!(code == 2 && text.contains("needs the model's name"));
        assert!(run(&["gateway", "context", "prod"]).unwrap_err().1.contains("and a question"));
        let ask = run(&["-p", "shop", "ask", "prod", "top", "customers"]).unwrap();
        assert_eq!(ask.ask, Some(["prod", "top", "customers"].map(String::from).to_vec()));
        assert!(run(&["ask", "prod"]).unwrap_err().1.contains("and a question"));
        let add = run(&["memory", "add", "orders.status", "3", "=", "cancelled"]).unwrap();
        assert_eq!(add.memory, Some(["add", "orders.status", "3", "=", "cancelled"].map(String::from).to_vec()));
        assert!(run(&["memory", "forget", "x"]).is_err() && run(&["memory", "drop"]).is_err());
        assert_eq!(run(&["memory"]).unwrap().memory, Some(Vec::new()));
        // Words after `memory` are its own, `ask` and `gateway` too.
        let odd = run(&["memory", "add", "quotes.side", "means", "ask", "gateway"]).unwrap();
        assert_eq!((odd.ask, odd.gateway), (None, None));
        assert_eq!(odd.memory.unwrap().len(), 5);
        // Without `gateway`, they're connection names as before.
        let plain = run(&["status"]).unwrap();
        assert_eq!((plain.gateway, plain.connection.as_deref()), (None, Some("status")));
    }

    #[test]
    fn the_man_page_lists_every_option() {
        let man = include_str!("../docs/podelis.1");
        // `--project` in USAGE is `Fl -project` in mdoc.
        let options = USAGE
            .split_whitespace()
            .filter(|w| w.starts_with("--"))
            .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()));
        for option in options {
            let flag = format!("Fl -{}", &option[2..]);
            assert!(man.contains(&flag), "docs/podelis.1 doesn't list {option}");
        }
        assert!(man.contains("Cm doctor"));
    }

    #[test]
    fn classifies_connections() {
        assert_eq!(classify("postgres://me@db/x"), Target::Url("postgres://me@db/x".into()));
        assert_eq!(classify("sqlite:app.db"), Target::Url("sqlite:app.db".into()));
        assert_eq!(classify("./data/app.sqlite"), Target::File("./data/app.sqlite".into()));
        assert_eq!(classify("app.db"), Target::File("app.db".into()));
        assert_eq!(classify("prod"), Target::Saved("prod".into()));
        let form = form_for_address(&Target::File("w.db".into())).unwrap();
        assert_eq!((form.engine, form.host.as_str()), (crate::db::Engine::Sqlite, "w.db"));
    }

    #[test]
    fn renders_tables_and_tsv() {
        let outcome = QueryOutcome {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec![Some("1".into()), Some("žąsis".into())], vec![Some("22".into()), None]],
            total_rows: 2,
            ..QueryOutcome::default()
        };
        assert_eq!(
            export::render_all_rows(&outcome, Format::Table),
            "id | name\n---+------\n1  | žąsis\n22 | NULL\n(2 rows)\n"
        );
        assert_eq!(export::render_all_rows(&outcome, Format::Tsv), "id\tname\n1\tžąsis\n22\t\n");
    }

    #[test]
    fn renders_every_result_set_the_format_can_hold() {
        let set = |n: &str| QueryOutcome {
            columns: vec!["n".into()],
            rows: vec![vec![Some(n.into())]],
            total_rows: 1,
            ..QueryOutcome::default()
        };
        let script = QueryOutcome { earlier: vec![set("1")], ..set("2") };
        assert_eq!(render_all(&script, Format::Json), "[\n  {\"n\": \"1\"}\n]\n[\n  {\"n\": \"2\"}\n]\n");
        assert_eq!(render_all(&script, Format::Csv), "n\r\n2\r\n");
        assert_eq!(render_all(&script, Format::Table), "n\n-\n1\n(1 row)\n\nn\n-\n2\n(1 row)\n");
    }

    #[tokio::test]
    async fn executes_against_a_sqlite_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.db");
        let (store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
        let run = |sql: &str, yes: bool| Args {
            connection: Some(path.to_string_lossy().into_owned()),
            execute: Some(sql.into()),
            format: Some(Format::Csv),
            yes,
            ..Args::default()
        };
        // A new file is only created with --create (a missing one is usually
        // a typo); --yes alone doesn't.
        assert_eq!(execute(&run("select 1", false), &store).await, exit::USAGE);
        assert_eq!(execute(&run("select 1", true), &store).await, exit::USAGE);
        assert!(!path.exists());
        let create = Args { create: true, ..run("create table t (x int); insert into t values (1)", false) };
        assert_eq!(execute(&create, &store).await, 0);
        assert_eq!(execute(&run("insert into t values (2)", false), &store).await, 0);
        assert_eq!(execute(&run("delete from t", false), &store).await, exit::REFUSED, "destructive needs --yes");
        assert_eq!(execute(&run("delete from t", true), &store).await, 0);
        assert_eq!(execute(&run("select * from nope", false), &store).await, exit::FAILED);
        let unknown = Args { connection: Some("nowhere".into()), ..run("select 1", false) };
        assert_eq!(execute(&unknown, &store).await, exit::USAGE, "no project to find a saved connection in");
        // A missing file to import is a usage error, found before connecting:
        // --create doesn't leave an empty database behind.
        let fresh = dir.path().join("fresh.db");
        let import_missing = Args {
            connection: Some(fresh.to_string_lossy().into_owned()),
            import: Some(dir.path().join("typo.csv").to_string_lossy().into_owned()),
            create: true,
            ..Args::default()
        };
        assert_eq!(import(&import_missing, &store).await, exit::USAGE);
        assert!(!fresh.exists());
    }
}
