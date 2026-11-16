//! `podelis gateway [status | start | stop]`: running it in the
//! foreground, and asking about a background one. Printing is fine here,
//! as in `cli.rs`: no TUI is running.

use std::io::Write as _;
use std::path::Path;

use super::client::{CallError, Client, connect_or_start, spawn_gateway};
use super::methods::{self as m, Note, Scope};
use super::server::{Ended, Options, serve};

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

/// Does `command` for `config_dir` (and `project`, where it's about
/// one); returns the exit code.
pub async fn run(command: GatewayCommand, config_dir: &Path, project: Option<&str>) -> i32 {
    match command {
        GatewayCommand::Run => foreground(config_dir).await,
        GatewayCommand::Status => match Client::connect(config_dir, "podelis gateway status").await {
            Ok(mut client) => status(&mut client).await,
            Err(e @ CallError::Io(_)) if unusable(&e) => fail(&e),
            Err(CallError::Io(_)) => say("The gateway isn't running (it starts when a feature needs it).", 1),
            Err(CallError::Rpc(e)) if e.code == super::protocol::code::VERSION => say(
                "A gateway from another Podėlis version runs: the next use replaces it (or `podelis gateway stop`).",
                1,
            ),
            Err(e) => fail(&e),
        },
        GatewayCommand::Start => {
            let start = || spawn_gateway(config_dir);
            match connect_or_start(config_dir, "podelis gateway start", start).await {
                Ok(mut client) => status(&mut client).await,
                Err(e) => fail(&e),
            }
        }
        GatewayCommand::Stop => match Client::connect(config_dir, "podelis gateway stop").await {
            Ok(mut client) => match client.request::<m::Shutdown>(&m::NoParams {}).await {
                Ok(_) => say("The gateway is stopping.", 0),
                Err(e) => fail(&e),
            },
            Err(e @ CallError::Io(_)) if unusable(&e) => fail(&e),
            Err(CallError::Io(_)) => say("The gateway isn't running.", 0),
            Err(CallError::Rpc(e)) if e.code == super::protocol::code::VERSION => {
                match super::client::replace_other(config_dir, &e).await {
                    Ok(()) => say("The gateway (from another Podėlis version) stopped.", 0),
                    Err(e) => fail(&e),
                }
            }
            Err(e) => fail(&e),
        },
        GatewayCommand::Models => match started(config_dir, "podelis gateway models").await {
            Ok(mut client) => match client.request::<m::Models>(&m::NoParams {}).await {
                Ok(listed) => list_models(&listed),
                Err(e) => fail(&e),
            },
            Err(e) => fail(&e),
        },
        GatewayCommand::Key(name) => store_key(config_dir, &name),
        GatewayCommand::Test(name) => test(config_dir, name.as_deref()).await,
        GatewayCommand::Context(words) => context(config_dir, project, &words).await,
    }
}

/// `podelis mcp`: a bridge between the agent (stdin and stdout) and the
/// gateway, which serves the project's shared connections (started if it
/// isn't running). Returns the exit code.
pub async fn mcp(config_dir: &Path, project: Option<&str>, name: Option<String>) -> i32 {
    let mut client = match connect_or_start(config_dir, "podelis mcp", || spawn_gateway(config_dir)).await {
        Ok(client) => client,
        Err(e) => {
            let _ = eprintln_safe(&format!("podelis mcp: {e}"));
            return 1;
        }
    };
    let open = m::McpOpenParams { project: project.map(str::to_string), name, dir: here() };
    match client.request::<m::McpOpen>(&open).await {
        Ok(_) => {}
        Err(CallError::Rpc(e)) => {
            let _ = eprintln_safe(&format!("podelis mcp: {}", e.message));
            return 2;
        }
        Err(e) => {
            let _ = eprintln_safe(&format!("podelis mcp: {e}"));
            return 1;
        }
    }
    let (mut from_gateway, mut to_gateway) = client.into_stream();
    let to = async {
        let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut to_gateway).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut to_gateway).await;
    };
    let from = async {
        let _ = tokio::io::copy(&mut from_gateway, &mut tokio::io::stdout()).await;
    };
    tokio::pin!(to, from);
    tokio::select! {
        () = &mut from => {
            let _ = eprintln_safe("podelis mcp: the gateway closed the session");
            return 1;
        }
        () = &mut to => from.await,
    }
    0
}

/// Where this runs: a relative SQLite file is from here.
fn here() -> Option<String> {
    std::env::current_dir().ok().map(|d| d.display().to_string())
}

/// About `connection` (if any) in `project` (the last one opened when none).
fn scope(project: Option<&str>, connection: Option<&str>) -> Scope {
    Scope { project: project.map(str::to_string), connection: connection.map(str::to_string), dir: here() }
}

/// A message on stderr (stdout carries the protocol).
fn eprintln_safe(message: &str) -> std::io::Result<()> {
    writeln!(std::io::stderr(), "{message}")
}

/// `ontology <connection> [turtle | export [<file>] | import <file>]`: the
/// connection's ontology (its database's catalog and what the project's
/// memory says of it) as text, as SKOS Turtle, as YAML to edit, or a YAML
/// file loaded back, saying what changes first. Sends nothing to models.
pub async fn ontology(config_dir: &Path, project: Option<&str>, words: &[String], yes: bool) -> i32 {
    let mut client = match started(config_dir, "podelis ontology").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let scope = scope(project, Some(&words[0]));
    match words.get(1).map(String::as_str) {
        Some("import") => import_ontology(&mut client, scope, &words[2], yes).await,
        action => show_ontology(&mut client, scope, action, words.get(2), &words[0], project).await,
    }
}

/// `ontology <connection> [turtle | yaml | export [<file>]]`: as text, SKOS
/// Turtle or YAML, written out or to `file`.
async fn show_ontology(
    client: &mut Client,
    scope: Scope,
    action: Option<&str>,
    file: Option<&String>,
    connection: &str,
    project: Option<&str>,
) -> i32 {
    let format = match action {
        Some("turtle") => "turtle",
        Some("export" | "yaml") => "yaml",
        _ => "text",
    };
    let params = m::OntologyParams { scope, format: Some(format.to_string()), refresh: None };
    let built = match client.request::<m::Ontology>(&params).await {
        Ok(built) => built,
        Err(e) => return fail(&e),
    };
    let text = match format {
        "turtle" => built.turtle,
        "yaml" => built.yaml,
        _ => built.text,
    }
    .unwrap_or_default();
    let Some(file) = file else {
        let _ = write!(std::io::stdout(), "{text}");
        return 0;
    };
    match std::fs::write(file, text) {
        Ok(()) => {
            let project = project.map(|p| format!("-p '{p}' ")).unwrap_or_default();
            let again = format!("podelis ontology {project}{connection} import {file}");
            say(&format!("Wrote {file}: edit it, then load it back with `{again}`."), 0)
        }
        Err(e) => say(&format!("couldn't write {file}: {e}"), 1),
    }
}

/// `ontology <connection> import <file>`: what loading the edited file
/// would change, as the app's `E` shows it; changed so after a yes (or
/// `-y`).
async fn import_ontology(client: &mut Client, scope: Scope, file: &str, yes: bool) -> i32 {
    use std::io::IsTerminal as _;
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) => return say(&format!("couldn't read {file}: {e}"), 2),
    };
    let mut params = m::ImportParams { scope, text: Some(text), apply: false, refresh: None };
    let planned = match client.request::<m::OntologyImport>(&params).await {
        Ok(m::ImportResult::Plan(planned)) => planned,
        Ok(m::ImportResult::Applied(_)) => {
            return say("The gateway changed the memory when only asked what it would change.", 1);
        }
        Err(e) => return fail(&e),
    };
    let mut out = std::io::stdout();
    // (A file that doesn't say when it was exported: its plan says so.)
    if planned.kept > 0 && !planned.undated {
        let kept = planned.kept;
        let _ = writeln!(out, "Kept: {kept} fact(s) said since the file was exported, though it doesn't have them.");
    }
    let lines = super::ontology::plan_lines(&planned);
    if lines.is_empty() {
        // Undated, so its plan says nothing of what it lacks: said here.
        if planned.undated && planned.kept > 0 {
            let kept = planned.kept;
            let _ = writeln!(
                out,
                "Kept: {kept} fact(s) the file doesn't have, as it doesn't say when it was exported (export it again to remove facts)."
            );
        }
        return say(&format!("Nothing to change: {file} says what the memory does."), 0);
    }
    for (heading, line) in lines {
        let _ = if heading { writeln!(out, "{line}:") } else { writeln!(out, "  {line}") };
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return say("Not changed: run it in a terminal to answer, or add -y.", 3);
        }
        let _ = write!(out, "Change the memory so? [y/N] ");
        let _ = out.flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !answer.trim().eq_ignore_ascii_case("y") {
            return say("Not changed.", 3);
        }
    }
    params.apply = true;
    match client.request::<m::OntologyImport>(&params).await {
        Ok(m::ImportResult::Applied(done)) => {
            let said = format!(
                "Done: {} added, {} confirmed, {} moved, {} forgotten.",
                done.added, done.confirmed, done.moved, done.forgotten
            );
            say(&said, 0)
        }
        Ok(m::ImportResult::Plan(_)) => say("The gateway didn't say what it changed.", 1),
        Err(e) => fail(&e),
    }
}

/// `context <connection> <question…>`: what a model would be shown, and
/// why each table is there.
async fn context(config_dir: &Path, project: Option<&str>, words: &[String]) -> i32 {
    let (connection, question) = (&words[0], words[1..].join(" "));
    let mut client = match started(config_dir, "podelis gateway context").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let params = m::ContextParams { scope: scope(project, Some(connection)), question, refresh: true };
    let built = match client.request::<m::Context>(&params).await {
        Ok(built) => built,
        Err(e) => return fail(&e),
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "Tables ({}):", built.tables.len());
    for t in &built.tables {
        let _ = writeln!(out, "  {}: {}", t.name, t.why);
    }
    if let Some(n) = Some(built.left_out).filter(|n| *n > 0) {
        use podelis_ai::context::{MAX_CHARS, MAX_ENTRY, MAX_TABLES};
        let tokens = MAX_CHARS / 4;
        let _ = writeln!(
            out,
            "  ({n} more qualified, left out: at most {MAX_ENTRY} tables by their words and {MAX_TABLES} in all, within about {tokens} tokens)"
        );
    }
    if !built.stale_facts.is_empty() {
        let _ = writeln!(out, "Left out, the table or column they name isn't in this database (stale?):");
        for f in &built.stale_facts {
            let _ = writeln!(out, "  {f}");
        }
    }
    let chars = built.chars;
    let _ = writeln!(
        out,
        "
What a model would be shown (about {} tokens):
",
        chars.div_ceil(4)
    );
    let _ = write!(out, "{}", built.text);
    0
}

/// No socket could be made in the config folder (`usable_socket_path`):
/// not "isn't running", which would hide why.
fn unusable(e: &CallError) -> bool {
    matches!(e, CallError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput)
}

/// The gateway, started if it isn't running.
async fn started(config_dir: &Path, name: &str) -> Result<Client, CallError> {
    connect_or_start(config_dir, name, || spawn_gateway(config_dir)).await
}

fn list_models(listed: &m::ModelsResult) -> i32 {
    let models = &listed.models;
    if models.is_empty() {
        return say("No models are set up: add a [[models]] entry to config.toml (podelis(1), FILES).", 1);
    }
    let mut out = std::io::stdout();
    for (i, m) in models.iter().enumerate() {
        let default = if i == 0 { "  (default)" } else { "" };
        let _ = writeln!(out, "{}  {} {} at {}; key: {}{default}", m.name, m.api, m.model, m.url, m.key);
    }
    0
}

/// `key <model>`: asks for the key without showing it (or reads it from
/// standard input when that isn't a terminal: `pbpaste | podelis gateway
/// key claude`) and stores it in the OS keychain, where only the gateway
/// reads it.
fn store_key(config_dir: &Path, name: &str) -> i32 {
    use std::io::IsTerminal as _;
    let (settings, _) = crate::config::settings::Settings::load(config_dir);
    if !settings.models.iter().any(|m| m.name == name) {
        let known: Vec<&str> = settings.models.iter().map(|m| m.name.as_str()).collect();
        let known = if known.is_empty() { "none yet".to_string() } else { known.join(", ") };
        return say(&format!("No model `{name}` in config.toml (there: {known})."), 2);
    }
    let key = if std::io::stdin().is_terminal() {
        let Some(key) = crate::system::tty::prompt_password(&format!("API key for {name} (not shown): ")) else {
            return say("Nothing stored.", 1);
        };
        key
    } else {
        let Some(key) = key_from(std::io::stdin().lock()) else {
            let _ = writeln!(
                std::io::stderr(),
                "No key on standard input, and it isn't a terminal to ask in: run this in a terminal, or pipe the key in (`pbpaste | podelis gateway key {name}`). Nothing stored."
            );
            return 1;
        };
        key
    };
    let account = podelis_ai::models::ModelConfig::keychain_account(name);
    match crate::db::keychain::store_password(&account, key.trim()) {
        Ok(()) => say(&format!("Stored in the OS keychain (service podelis, account {account})."), 0),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "Couldn't store it: {e:#}");
            1
        }
    }
}

/// The key piped in: its first line that isn't blank, trimmed.
pub(super) fn key_from(input: impl std::io::BufRead) -> Option<String> {
    input.lines().map_while(Result::ok).map(|line| line.trim().to_string()).find(|line| !line.is_empty())
}

/// `test [model]`: one short question, to check the model answers.
async fn test(config_dir: &Path, name: Option<&str>) -> i32 {
    let mut client = match started(config_dir, "podelis gateway test").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let params = m::CompleteParams {
        model: name.map(str::to_string),
        system: None,
        messages: vec![m::ChatMessage { role: "user".into(), content: "Reply with the single word OK.".into() }],
        max_tokens: Some(2000),
    };
    let started_at = std::time::Instant::now();
    match client.request::<m::Complete>(&params).await {
        Ok(reply) => {
            let text = format!(
                "{} ({}) answered in {:.1} s: {:?} · {} tokens in, {} out · stopped: {}",
                reply.model,
                reply.provider_model,
                started_at.elapsed().as_secs_f64(),
                reply.text.trim(),
                reply.usage.input_tokens,
                reply.usage.output_tokens,
                reply.stop_reason,
            );
            say(&text, 0)
        }
        Err(e) => fail(&e),
    }
}

/// Runs the gateway here until Ctrl+C, `stop` or the idle time.
async fn foreground(config_dir: &Path) -> i32 {
    let (settings, _) = crate::config::settings::Settings::load(config_dir);
    let idle = super::idle_from_minutes(settings.gateway_idle);
    let socket = super::socket_path(config_dir);
    // Ctrl+C, or SIGTERM from `kill`, launchd or systemd: stop cleanly
    // (the socket file goes).
    let interrupt = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    };
    let mut err = std::io::stderr();
    let _ = writeln!(err, "Podėlis gateway on {} (Ctrl+C stops it).", socket.display());
    match serve(config_dir, Options::new(idle), interrupt).await {
        Ok(Ended::AlreadyRunning) => {
            let _ = writeln!(err, "A gateway already runs for {}.", config_dir.display());
            0
        }
        Ok(Ended::Stopped(reason)) => {
            let _ = writeln!(err, "The gateway stopped ({reason}).");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "The gateway couldn't start: {e}");
            1
        }
    }
}

async fn status(client: &mut Client) -> i32 {
    let s = match client.request::<m::Status>(&m::NoParams {}).await {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let idle = s
        .idle_stop_secs
        .map_or_else(|| "keeps running".to_string(), |secs| format!("stops after {} min with no client", secs / 60));
    let text = format!(
        "The gateway runs: version {}, process {}, up {} s, {} other client(s); {idle}.",
        s.version,
        s.pid,
        s.uptime_secs,
        // Not counting this one.
        s.clients.saturating_sub(1),
    );
    say(&text, 0)
}

fn say(text: &str, code: i32) -> i32 {
    let _ = writeln!(std::io::stdout(), "{text}");
    code
}

fn fail(e: &CallError) -> i32 {
    let _ = writeln!(std::io::stderr(), "{e}");
    1
}

/// `ask <connection> <question…>`: the SQL on stdout, the rest on
/// stderr (so `> query.sql` keeps just the query). Exit 0 with SQL, 1 if
/// none came, 3 if the schema wasn't allowed to go to the model.
pub async fn ask(config_dir: &Path, project: Option<&str>, model: Option<&str>, words: &[String]) -> i32 {
    let mut err = std::io::stderr();
    let (connection, question) = (&words[0], words[1..].join(" "));
    let mut client = match started(config_dir, "podelis ask").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let mut params = m::AskParams {
        scope: scope(project, Some(connection)),
        question,
        model: model.map(str::to_string),
        ..m::AskParams::default()
    };
    let _ = writeln!(err, "Asking…");
    let mut answer = client.request_streaming::<m::Ask>(&params, |note| waiting_key(&note)).await;
    if let Err(CallError::Rpc(e)) = &answer
        && e.code == super::protocol::code::NOT_SHARED
    {
        match allow(&e.message, m::NotShared::from_data(e.data.as_ref())) {
            Some(()) => {
                params.share_once = true;
                answer = client.request_streaming::<m::Ask>(&params, |note| waiting_key(&note)).await;
            }
            None => return 3,
        }
    }
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => return fail(&e),
    };
    let tables: Vec<&str> = answer.tables.iter().map(|t| t.name.as_str()).collect();
    let _ = writeln!(
        err,
        "{} ({}) was shown {} · {} tokens in, {} out{}",
        answer.model,
        answer.provider_model,
        tables.join(", "),
        answer.usage.input_tokens,
        answer.usage.output_tokens,
        match answer.attempts {
            n if n > 1 => format!(" · asked {n} times: its first SQL used names not in the schema"),
            _ => String::new(),
        },
    );
    if !answer.looked_at.is_empty() {
        let _ = writeln!(err, "It also looked at: {}", answer.looked_at.join(", "));
    }
    if !answer.said.is_empty() {
        let _ = writeln!(err, "{}", answer.said);
    }
    if !answer.problems.is_empty() {
        let _ = writeln!(err, "Check before running it: {}", answer.problems.join("; "));
    }
    for f in &answer.suggested {
        let _ = writeln!(
            err,
            "It assumed: {} {} {} — `podelis memory confirm {id}` if that's right, `podelis memory forget {id}` if not",
            f.subject,
            f.relation,
            f.value,
            id = f.id
        );
    }
    if let Some(sql) = &answer.sql {
        let _ = writeln!(std::io::stdout(), "{}", sql.trim_end());
        0
    } else {
        let _ = writeln!(err, "No SQL came back.");
        1
    }
}

/// The project doesn't share its schema with this model: asks in the
/// terminal whether to, this once or always. `None`: no.
fn allow(message: &str, data: Option<m::NotShared>) -> Option<()> {
    use std::io::IsTerminal as _;
    let mut err = std::io::stderr();
    let project = data.as_ref().map_or("?", |d| d.project.as_str()).to_string();
    if !std::io::stdin().is_terminal() {
        let _ = writeln!(err, "Not sent: {message}. Run `podelis ask` in a terminal to allow it.");
        return None;
    }
    let _ =
        write!(err, "Not sent yet: {message}.\nSend them? [y] this once, [a] always for project '{project}', [N] no: ");
    let _ = err.flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(()),
        "a" | "always" => {
            let Some(id) = data.and_then(|d| d.project_id) else {
                let _ = writeln!(err, "Couldn't tell which project to save it for: sent this once.");
                return Some(());
            };
            let (mut store, _) = crate::config::store::ConnectionStore::load();
            match store.set_share_schema(&id, crate::config::store::ShareSchema::All) {
                Ok(()) => {
                    let _ = writeln!(err, "Project '{project}' now shares its schemas with every model set up.");
                    Some(())
                }
                Err(e) => {
                    let _ = writeln!(err, "Couldn't save it: {e:#}");
                    None
                }
            }
        }
        _ => {
            let _ = writeln!(err, "Not sent.");
            None
        }
    }
}

/// `memory [list [words] | add <subject> <what…> | forget <id> | export]`.
pub async fn memory(config_dir: &Path, project: Option<&str>, connection: Option<&str>, words: &[String]) -> i32 {
    let mut client = match started(config_dir, "podelis memory").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    // `-c prod`: about prod's database.
    let scope = Scope { project: project.map(str::to_string), connection: connection.map(str::to_string), dir: None };
    let id = || words.get(1).and_then(|w| w.parse::<i64>().ok()).map(podelis_ai::memory::FactId::from_written);
    let action = words.first().map_or("list", String::as_str);
    let mut out = std::io::stdout();
    let said = |said: &str, f: &podelis_ai::memory::Fact| {
        format!("{said} #{}: {} {} {}", f.id, f.subject, f.relation, f.value)
    };
    let done = match action {
        "add" => {
            let p = m::MemoryAddParams { scope, text: Some(words[1..].join(" ")), ..m::MemoryAddParams::default() };
            client.request::<m::MemoryAdd>(&p).await.map(|f| said("Remembered", &f))
        }
        "edit" => {
            let p = m::MemoryUpdateParams { scope, id: id(), text: Some(words[2..].join(" ")) };
            client.request::<m::MemoryUpdate>(&p).await.map(|f| said("Changed", &f))
        }
        "forget" => {
            let p = m::FactParams { scope, id: id() };
            client.request::<m::MemoryForget>(&p).await.map(|d| format!("Forgot #{}", d.id))
        }
        "confirm" => {
            let p = m::FactParams { scope, id: id() };
            client.request::<m::MemoryConfirm>(&p).await.map(|d| format!("Confirmed #{}", d.id))
        }
        "learn" => {
            // The gateway may run elsewhere: the folder goes as a full path.
            let folder = crate::config::settings::expand_home(&words[1]);
            let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
            let p = m::MemoryLearnParams { scope, folder: Some(folder.display().to_string()) };
            client.request::<m::MemoryLearn>(&p).await.map(|l| {
                format!(
                    "Learned {} facts from {} migration files in {} (what was learned from there before is replaced).",
                    l.facts, l.files, l.folder
                )
            })
        }
        "export" => match client.request::<m::MemoryExport>(&scope).await {
            Ok(exported) => {
                let _ = write!(out, "{}", exported.text);
                return 0;
            }
            Err(e) => Err(e),
        },
        _ => {
            let search = (words.len() > 1).then(|| words[1..].join(" "));
            match client.request::<m::MemoryList>(&m::MemoryListParams { scope, search }).await {
                Ok(listed) => {
                    if listed.facts.is_empty() {
                        let _ =
                            writeln!(out, "No facts yet: `podelis memory add orders.status 3 = cancelled` adds one.");
                    }
                    for f in &listed.facts {
                        let unconfirmed = if f.confirmed { "" } else { "  (not confirmed)" };
                        let _ = writeln!(
                            out,
                            "{:>4}  {} {} {}  · {}{unconfirmed}",
                            f.id.get(),
                            f.subject,
                            f.relation,
                            f.value,
                            f.source
                        );
                    }
                    return 0;
                }
                Err(e) => Err(e),
            }
        }
    };
    match done {
        Ok(said) => {
            let _ = writeln!(out, "{said}");
            0
        }
        Err(e) => fail(&e),
    }
}

/// A key slow to come (`key.waiting`): said on stderr, as the app's status
/// bar says it.
fn waiting_key(note: &Note) {
    if matches!(note, Note::KeyWaiting(_)) {
        let _ = writeln!(std::io::stderr(), "{}", t!("ask.waiting_key"));
    }
}
