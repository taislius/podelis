//! `podelis gateway [status | start | stop]`: running it in the
//! foreground, and asking about a background one. Printing is fine here,
//! as in `cli.rs`: no TUI is running.

use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::client::{CallError, Client, connect_or_start, spawn_gateway};
use super::server::{Ended, Options, serve};
use crate::cli::GatewayCommand;

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
            Ok(mut client) => match client.call("shutdown", json!({})).await {
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
            Ok(mut client) => match client.call("models", json!({})).await {
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

/// `context <connection> <question…>`: what a model would be shown, and
/// why each table is there.
async fn context(config_dir: &Path, project: Option<&str>, words: &[String]) -> i32 {
    let (connection, question) = (&words[0], words[1..].join(" "));
    let mut client = match started(config_dir, "podelis gateway context").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let dir = std::env::current_dir().ok().map(|d| d.display().to_string());
    let mut params = json!({ "connection": connection, "question": question, "refresh": true, "dir": dir });
    if let Some(project) = project {
        params["project"] = json!(project);
    }
    let built = match client.call("context", params).await {
        Ok(built) => built,
        Err(e) => return fail(&e),
    };
    let mut out = std::io::stdout();
    let tables = built["tables"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(out, "Tables ({}):", tables.len());
    for t in &tables {
        let _ = writeln!(out, "  {}: {}", t["name"].as_str().unwrap_or("?"), t["why"].as_str().unwrap_or(""));
    }
    if let Some(n) = built["left_out"].as_u64().filter(|n| *n > 0) {
        let _ = writeln!(out, "  ({n} more qualified, left out for size)");
    }
    let chars = built["chars"].as_u64().unwrap_or(0);
    let _ = writeln!(
        out,
        "
What a model would be shown (about {} tokens):
",
        chars.div_ceil(4)
    );
    let _ = write!(out, "{}", built["text"].as_str().unwrap_or_default());
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

fn list_models(listed: &serde_json::Value) -> i32 {
    let models = listed["models"].as_array().cloned().unwrap_or_default();
    if models.is_empty() {
        return say("No models are set up: add a [[models]] entry to config.toml (podelis(1), FILES).", 1);
    }
    let mut out = std::io::stdout();
    for (i, m) in models.iter().enumerate() {
        let default = if i == 0 { "  (default)" } else { "" };
        let _ = writeln!(
            out,
            "{}  {} {} at {}; key: {}{default}",
            m["name"].as_str().unwrap_or("?"),
            m["api"].as_str().unwrap_or("?"),
            m["model"].as_str().unwrap_or("?"),
            m["url"].as_str().unwrap_or("?"),
            m["key"].as_str().unwrap_or("?"),
        );
    }
    0
}

/// `key <model>`: asks for the key without showing it and stores it in
/// the OS keychain, where only the gateway reads it.
fn store_key(config_dir: &Path, name: &str) -> i32 {
    let (settings, _) = crate::config::settings::Settings::load(config_dir);
    if !settings.models.iter().any(|m| m.name == name) {
        let known: Vec<&str> = settings.models.iter().map(|m| m.name.as_str()).collect();
        let known = if known.is_empty() { "none yet".to_string() } else { known.join(", ") };
        return say(&format!("No model `{name}` in config.toml (there: {known})."), 2);
    }
    let Some(key) = crate::cli::prompt_password(&format!("API key for {name} (not shown): ")) else {
        return say("Nothing stored.", 1);
    };
    let account = super::models::ModelConfig::keychain_account(name);
    match crate::config::store::store_password(&account, key.trim()) {
        Ok(()) => say(&format!("Stored in the OS keychain (service podelis, account {account})."), 0),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "Couldn't store it: {e:#}");
            1
        }
    }
}

/// `test [model]`: one short question, to check the model answers.
async fn test(config_dir: &Path, name: Option<&str>) -> i32 {
    let mut client = match started(config_dir, "podelis gateway test").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let mut params = json!({
        "messages": [{ "role": "user", "content": "Reply with the single word OK." }],
        "max_tokens": 2000,
    });
    if let Some(name) = name {
        params["model"] = json!(name);
    }
    let started_at = std::time::Instant::now();
    match client.call("complete", params).await {
        Ok(reply) => {
            let text = format!(
                "{} ({}) answered in {:.1} s: {:?} · {} tokens in, {} out · stopped: {}",
                reply["model"].as_str().unwrap_or("?"),
                reply["provider_model"].as_str().unwrap_or("?"),
                started_at.elapsed().as_secs_f64(),
                reply["text"].as_str().unwrap_or_default().trim(),
                reply["usage"]["input_tokens"],
                reply["usage"]["output_tokens"],
                reply["stop_reason"].as_str().unwrap_or("?"),
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
    let s = match client.call("status", json!({})).await {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    let idle = s["idle_stop_secs"]
        .as_u64()
        .map_or_else(|| "keeps running".to_string(), |secs| format!("stops after {} min with no client", secs / 60));
    let text = format!(
        "The gateway runs: version {}, process {}, up {} s, {} other client(s); {idle}.",
        s["version"].as_str().unwrap_or("?"),
        s["pid"],
        s["uptime_secs"],
        // Not counting this one.
        s["clients"].as_u64().unwrap_or(1).saturating_sub(1),
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
    let dir = std::env::current_dir().ok().map(|d| d.display().to_string());
    let mut params = json!({ "connection": connection, "question": question, "dir": dir });
    if let Some(project) = project {
        params["project"] = json!(project);
    }
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    let _ = writeln!(err, "Asking…");
    let mut answer = client.call("ask", params.clone()).await;
    if let Err(CallError::Rpc(e)) = &answer
        && e.code == super::protocol::code::NOT_SHARED
    {
        match allow(&e.message, e.data.as_ref()) {
            Some(()) => {
                params["share_once"] = json!(true);
                answer = client.call("ask", params).await;
            }
            None => return 3,
        }
    }
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => return fail(&e),
    };
    let tables: Vec<&str> =
        answer["tables"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str()).collect();
    let _ = writeln!(
        err,
        "{} ({}) was shown {} · {} tokens in, {} out{}",
        answer["model"].as_str().unwrap_or("?"),
        answer["provider_model"].as_str().unwrap_or("?"),
        tables.join(", "),
        answer["usage"]["input_tokens"],
        answer["usage"]["output_tokens"],
        match answer["attempts"].as_u64() {
            Some(n) if n > 1 => format!(" · asked {n} times: its first SQL used names not in the schema"),
            _ => String::new(),
        },
    );
    if let Some(said) = answer["said"].as_str().filter(|s| !s.is_empty()) {
        let _ = writeln!(err, "{said}");
    }
    let problems: Vec<&str> = answer["problems"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !problems.is_empty() {
        let _ = writeln!(err, "Check before running it: {}", problems.join("; "));
    }
    for f in answer["suggested"].as_array().into_iter().flatten() {
        let _ = writeln!(
            err,
            "It assumed: {} {} {} — `podelis memory confirm {id}` if that's right, `podelis memory forget {id}` if not",
            f["subject"].as_str().unwrap_or("?"),
            f["relation"].as_str().unwrap_or("?"),
            f["value"].as_str().unwrap_or("?"),
            id = f["id"]
        );
    }
    if let Some(sql) = answer["sql"].as_str() {
        let _ = writeln!(std::io::stdout(), "{}", sql.trim_end());
        0
    } else {
        let _ = writeln!(err, "No SQL came back.");
        1
    }
}

/// The project doesn't share its schema with this model: asks in the
/// terminal whether to, this once or always. `None`: no.
fn allow(message: &str, data: Option<&Value>) -> Option<()> {
    use std::io::IsTerminal as _;
    let mut err = std::io::stderr();
    let project = data.and_then(|d| d["project"].as_str()).unwrap_or("?").to_string();
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
            let Some(id) = data.and_then(|d| d["project_id"].as_str()) else {
                let _ = writeln!(err, "Couldn't tell which project to save it for: sent this once.");
                return Some(());
            };
            let (mut store, _) = crate::config::store::ConnectionStore::load();
            match store.set_share_schema(id, crate::config::store::ShareSchema::All) {
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
pub async fn memory(config_dir: &Path, project: Option<&str>, words: &[String]) -> i32 {
    let mut client = match started(config_dir, "podelis memory").await {
        Ok(client) => client,
        Err(e) => return fail(&e),
    };
    let mut params = json!({});
    if let Some(project) = project {
        params["project"] = json!(project);
    }
    let action = words.first().map_or("list", String::as_str);
    let (method, said) = match action {
        "add" => {
            params["text"] = json!(words[1..].join(" "));
            ("memory.add", "Remembered")
        }
        "forget" | "confirm" => {
            params["id"] = json!(words[1].parse::<i64>().unwrap_or(0));
            if action == "forget" { ("memory.forget", "Forgot") } else { ("memory.confirm", "Confirmed") }
        }
        "learn" => {
            // The gateway may run elsewhere: the folder goes as a full path.
            let folder = crate::config::settings::expand_home(&words[1]);
            let folder = std::fs::canonicalize(&folder).unwrap_or(folder);
            params["folder"] = json!(folder.display().to_string());
            ("memory.learn", "")
        }
        "export" => ("memory.export", ""),
        _ => {
            if words.len() > 1 {
                params["search"] = json!(words[1..].join(" "));
            }
            ("memory.list", "")
        }
    };
    let answer = match client.call(method, params).await {
        Ok(answer) => answer,
        Err(e) => return fail(&e),
    };
    let mut out = std::io::stdout();
    match method {
        "memory.export" => {
            let _ = write!(out, "{}", answer["text"].as_str().unwrap_or_default());
        }
        "memory.learn" => {
            let _ = writeln!(
                out,
                "Learned {} facts from {} migration files in {} (what was learned from there before is replaced).",
                answer["facts"],
                answer["files"],
                answer["folder"].as_str().unwrap_or("?")
            );
        }
        "memory.list" => {
            let facts = answer["facts"].as_array().cloned().unwrap_or_default();
            if facts.is_empty() {
                let _ = writeln!(out, "No facts yet: `podelis memory add orders.status 3 = cancelled` adds one.");
            }
            for f in &facts {
                let unconfirmed = if f["confirmed"].as_bool() == Some(true) { "" } else { "  (not confirmed)" };
                let _ = writeln!(
                    out,
                    "{:>4}  {} {} {}  · {}{unconfirmed}",
                    f["id"].as_i64().unwrap_or(0),
                    f["subject"].as_str().unwrap_or("?"),
                    f["relation"].as_str().unwrap_or("?"),
                    f["value"].as_str().unwrap_or("?"),
                    f["source"].as_str().unwrap_or("?"),
                );
            }
        }
        _ => {
            let what = if method == "memory.add" {
                format!(
                    " #{}: {} {} {}",
                    answer["id"],
                    answer["subject"].as_str().unwrap_or("?"),
                    answer["relation"].as_str().unwrap_or("?"),
                    answer["value"].as_str().unwrap_or("?")
                )
            } else {
                format!(" #{}", answer["id"])
            };
            let _ = writeln!(out, "{said}{what}");
        }
    }
    0
}
