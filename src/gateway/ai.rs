//! The gateway's methods about models and what they're shown: `models`
//! lists them, `complete` asks one and streams its answer back as
//! `complete.delta` notifications before the response, and `context`
//! picks what of a database a model would see for a question.
//!
//! The configuration is read from `config.toml` on each call, so an edit
//! counts without restarting the gateway.

use std::path::Path;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedWriteHalf;

use super::context::Schema;
use super::models::{self, ModelConfig, Prompt};
use super::protocol::{self, RpcError, code};

/// The models in `config.toml`. A file that doesn't parse is said as it
/// is, not taken for one with no models (its defaults would have none).
fn configured(config_dir: &Path) -> Result<Vec<ModelConfig>, RpcError> {
    crate::config::settings::Settings::load_strict(config_dir)
        .map(|settings| settings.models)
        .map_err(|e| RpcError::new(code::MODEL, format!("invalid {e}")))
}

/// `models`: the configured models, the default first, and whether each
/// has its key (never the key).
pub async fn models(config_dir: &Path) -> Result<Value, RpcError> {
    let configs = configured(config_dir)?;
    let listed = tokio::task::spawn_blocking(move || {
        configs
            .iter()
            .map(|m| {
                let key = json!(models::key_source(m).label());
                json!({ "name": m.name, "api": m.api.label(), "model": m.model, "url": m.url(), "key": key })
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| RpcError::new(code::MODEL, format!("listing the models failed: {e}")))?;
    Ok(json!({ "models": listed }))
}

/// How long a key may take before the client is told it's waiting (for
/// the macOS keychain's Allow dialog, or a password manager).
const KEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// The model named (the default when `None`) and its key. A key that
/// takes longer than [`KEY_PATIENCE`] sends `key.waiting` (`{ id, model }`)
/// to the client first, so it can say what it's waiting for.
async fn model_and_key(
    config_dir: &Path,
    caches: &Caches,
    name: Option<&str>,
    id: &Value,
    write: &mut OwnedWriteHalf,
) -> Result<(ModelConfig, Option<String>), RpcError> {
    let configs = configured(config_dir)?;
    let config = models::pick(&configs, name).map_err(|e| RpcError::new(code::MODEL, e.message))?.clone();
    let lookup = config.clone();
    let keys = std::sync::Arc::clone(&caches.keys);
    let found = tokio::task::spawn_blocking(move || {
        models::key_with(&lookup, |config, command| {
            let entry = (config.name.clone(), command.to_string());
            if let Ok(keys) = keys.lock()
                && let Some((at, key)) = keys.get(&entry)
                && at.elapsed() < KEY_TTL
            {
                return Ok(key.clone());
            }
            let key = models::run_key_command(config, command)?;
            if let Ok(mut keys) = keys.lock() {
                keys.insert(entry, (std::time::Instant::now(), key.clone()));
            }
            Ok(key)
        })
    });
    tokio::pin!(found);
    let found = tokio::select! {
        found = &mut found => found,
        () = tokio::time::sleep(KEY_PATIENCE) => {
            let line = protocol::notification("key.waiting", &json!({ "id": id, "model": config.name }));
            let _ = write.write_all(line.as_bytes()).await;
            found.await
        }
    };
    match found {
        Ok(Ok((key, _))) => Ok((config, key)),
        Ok(Err(e)) => Err(RpcError::new(code::MODEL, e.message)),
        Err(e) => Err(RpcError::new(code::MODEL, e.to_string())),
    }
}

/// Asks the model, sending the answer's pieces to the client as `event`
/// notifications (`{ id, text }`) while the request runs.
async fn streamed(
    config: &ModelConfig,
    key: Option<&str>,
    prompt: &Prompt,
    id: &Value,
    event: &str,
    write: &mut OwnedWriteHalf,
) -> Result<models::Reply, models::ModelError> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ask = async {
        let reply = models::complete(config, key, prompt, |text| {
            let _ = tx.send(text.to_string());
        })
        .await;
        drop(tx);
        reply
    };
    let forward = async {
        while let Some(text) = rx.recv().await {
            let line = protocol::notification(event, &json!({ "id": id, "text": text }));
            // A client that went away still gets its answer logged.
            let _ = write.write_all(line.as_bytes()).await;
        }
    };
    tokio::join!(ask, forward).0
}

/// `complete`: `{ model?, system?, messages: [{ role, content }], max_tokens? }`.
/// Returns the answer and what the log may keep about it (the model and
/// the tokens, never the text).
pub async fn complete(
    config_dir: &Path,
    caches: &Caches,
    id: &Value,
    params: &Value,
    write: &mut OwnedWriteHalf,
) -> (Result<Value, RpcError>, Value) {
    let name = params.get("model").and_then(Value::as_str);
    let prompt: Prompt = match serde_json::from_value(params.clone()) {
        Ok(p) => p,
        Err(e) => return (Err(RpcError::new(code::INVALID_PARAMS, format!("complete: {e}"))), Value::Null),
    };
    if prompt.messages.is_empty() {
        return (Err(RpcError::new(code::INVALID_PARAMS, "complete needs at least one message")), Value::Null);
    }
    let (config, key) = match model_and_key(config_dir, caches, name, id, write).await {
        Ok(found) => found,
        Err(e) => return (Err(e), json!({ "model": name })),
    };
    match streamed(&config, key.as_deref(), &prompt, id, "complete.delta", write).await {
        Ok(reply) => {
            let log = json!({
                "model": config.name,
                "input_tokens": reply.input_tokens,
                "output_tokens": reply.output_tokens,
                "stop_reason": reply.stop_reason,
            });
            (Ok(reply.to_json(&config.name)), log)
        }
        Err(e) => {
            refused_key(caches, &config, &e);
            (Err(RpcError::new(code::MODEL, e.message)), json!({ "model": config.name }))
        }
    }
}

/// A key the provider refused isn't used again: the next call gets it
/// afresh (the user may have fixed it).
fn refused_key(caches: &Caches, config: &ModelConfig, e: &models::ModelError) {
    if e.message.starts_with("the key was refused") {
        caches.forget_key(&config.name);
    }
}

/// How long a loaded schema is used before it's loaded again.
const SCHEMA_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// What the gateway keeps between calls: schemas loaded (by project id
/// and connection name), each project's open memory, and what models'
/// `key_command`s printed (by model and command), so a password manager
/// isn't asked on every question.
#[derive(Default)]
pub struct Caches {
    schemas: tokio::sync::Mutex<std::collections::HashMap<(String, String), Loaded>>,
    memories: tokio::sync::Mutex<std::collections::HashMap<String, Memory>>,
    keys: std::sync::Arc<std::sync::Mutex<KeyCache>>,
}

/// Keys `key_command`s printed: by model and command, with when.
type KeyCache = std::collections::HashMap<(String, String), (std::time::Instant, String)>;

/// A project's memory, open.
type Memory = std::sync::Arc<tokio::sync::Mutex<sqlx::SqliteConnection>>;

/// How long a key a `key_command` printed is used before it's run again.
const KEY_TTL: std::time::Duration = std::time::Duration::from_mins(15);

impl Caches {
    /// The project's memory: opened (and set up) once, then kept.
    async fn memory(&self, config_dir: &Path, project_id: &str) -> Result<Memory, String> {
        if let Some(conn) = self.memories.lock().await.get(project_id) {
            return Ok(std::sync::Arc::clone(conn));
        }
        // Opened without holding the list: other projects' memories are
        // used meanwhile. Two opening it at once: the first kept wins.
        let opened = std::sync::Arc::new(tokio::sync::Mutex::new(super::memory::open(config_dir, project_id).await?));
        let mut open = self.memories.lock().await;
        Ok(std::sync::Arc::clone(open.entry(project_id.to_string()).or_insert(opened)))
    }

    /// After a failure: the next call opens the memory again.
    async fn forget_memory(&self, project_id: &str) {
        self.memories.lock().await.remove(project_id);
    }

    /// A refused key: the next call runs the model's `key_command` again.
    fn forget_key(&self, model: &str) {
        if let Ok(mut keys) = self.keys.lock() {
            keys.retain(|(name, _), _| name != model);
        }
    }
}

/// A schema, when it was loaded, and the connection's settings then
/// (changed ones load it again).
struct Loaded {
    profile: crate::config::store::ConnectionProfile,
    at: std::time::Instant,
    schema: std::sync::Arc<Schema>,
}

/// `context`: `{ connection, question, project?, refresh? }`. The tables
/// that matter for the question and the text a model would be shown,
/// from a saved connection, opened read-only. Returns what the log may
/// keep (the connection and how many tables, not the question).
pub async fn context(config_dir: &Path, caches: &Caches, params: &Value) -> (Result<Value, RpcError>, Value) {
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let (Some(connection), Some(question)) = (text("connection"), text("question")) else {
        return (Err(RpcError::new(code::INVALID_PARAMS, "context needs `connection` and `question`")), Value::Null);
    };
    let about = json!({ "connection": connection });
    let refresh = params.get("refresh").and_then(Value::as_bool).unwrap_or(false);
    let source = match load(config_dir, caches, text("project"), connection, text("dir"), refresh).await {
        Ok(source) => source,
        Err(e) => return (Err(RpcError::new(code::DATABASE, e)), about),
    };
    let schema = source.schema;
    let built = built_for(config_dir, caches, &source.project_id, &schema, question).await;
    let tables: Vec<Value> = built.tables.iter().map(|t| json!({ "name": t.name, "why": t.why })).collect();
    let about = json!({ "connection": connection, "tables": tables.len() });
    let result = json!({
        "engine": schema.engine,
        "text": built.text,
        "tables": tables,
        "left_out": built.left_out,
        "facts": built.facts,
        "chars": built.text.len(),
    });
    (Ok(result), about)
}

/// The project named (the last one opened when `None`), read from
/// `connections.toml` without changing it: a background process mustn't
/// move the user's file aside.
pub(crate) fn find_project(config_dir: &Path, name: Option<&str>) -> Result<crate::config::store::Project, String> {
    let (store, warning) = crate::config::store::ConnectionStore::peek(config_dir.join("connections.toml"));
    // The app names it by id (`id:<id>`): names are matched ignoring case,
    // and two projects may differ only in case.
    let found = match name {
        Some(name) => match name.strip_prefix("id:") {
            Some(id) => store.project(id),
            None => store.project_by_name(name),
        },
        None => store.last_project.as_deref().and_then(|id| store.project(id)),
    };
    found.cloned().ok_or_else(|| {
        let missing = match name {
            Some(name) => format!("no project named '{name}'"),
            None => "no project yet: name one (-p)".to_string(),
        };
        // The connections file may not have been readable.
        warning.map_or(missing.clone(), |w| format!("{missing} ({w})"))
    })
}

/// The context for `question`, with the project's memory read once: the
/// facts matching the question help choose the tables ("revenue is
/// payment.amount" brings `payment`), then those about the chosen tables
/// are shown with them. Memory that can't be read is logged and left
/// out: asking still works without it.
async fn built_for(
    config_dir: &Path,
    caches: &Caches,
    project_id: &str,
    schema: &Schema,
    question: &str,
) -> super::context::Context {
    let found = async {
        let conn = caches.memory(config_dir, project_id).await?;
        let mut conn = conn.lock().await;
        super::memory::for_question(&mut conn, question).await
    };
    let memory = found.await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "gateway: the project's memory couldn't be read");
        super::memory::ForQuestion::default()
    });
    let built = super::context::build(schema, &format!("{question}{}", memory.words()));
    let tables: Vec<String> = built.tables.iter().map(|t| t.name.clone()).collect();
    let facts = memory.about(&tables);
    built.with_facts(&facts)
}

/// A connection's schema, and its project's name and setting for
/// sharing schemas with models.
struct Source {
    schema: std::sync::Arc<Schema>,
    project: String,
    project_id: String,
    share: crate::config::store::ShareSchema,
}

/// The connection's schema: cached, or loaded through a read-only
/// connection opened as the app opens it (keychain, password command).
async fn load(
    config_dir: &Path,
    caches: &Caches,
    project: Option<&str>,
    connection: &str,
    dir: Option<&str>,
    refresh: bool,
) -> Result<Source, String> {
    let project = find_project(config_dir, project)?;
    let Some(saved) = project.connections.iter().find(|c| c.name.eq_ignore_ascii_case(connection)) else {
        let known: Vec<&str> = project.connections.iter().map(|c| c.name.as_str()).collect();
        return Err(format!(
            "no connection '{connection}' in project '{}' (there: {})",
            project.name,
            known.join(", ")
        ));
    };
    // A relative SQLite file is the asker's, from its folder.
    let profile = crate::config::store::opened_from(saved, dir);
    let source = |schema: std::sync::Arc<Schema>| Source {
        schema,
        project: project.name.clone(),
        project_id: project.id.clone(),
        share: project.share_schema,
    };
    let key = (project.id.clone(), profile.name.clone());
    if !refresh
        && let Some(loaded) = caches.schemas.lock().await.get(&key)
        && loaded.profile == profile
        && loaded.at.elapsed() < SCHEMA_TTL
    {
        return Ok(source(std::sync::Arc::clone(&loaded.schema)));
    }
    let project_id = project.id.clone();
    let for_password = profile.clone();
    let (password, _) =
        tokio::task::spawn_blocking(move || crate::config::store::saved_password(&project_id, &for_password))
            .await
            .map_err(|e| e.to_string())?;
    let mut params = crate::app::ConnectForm::from_profile(&profile, password).params()?;
    // Reading the catalog is all it does.
    params.read_only = true;
    let db =
        crate::db::Database::connect(params).await.map_err(|e| crate::db::with_hint(&crate::db::error_message(&e)))?;
    let said = |e: anyhow::Error| crate::db::error_message(&e);
    let schema = std::sync::Arc::new(Schema {
        engine: profile.engine.label().to_string(),
        dialect: profile.engine,
        tables: db.catalog(false).await.map_err(said)?,
        links: db.links().await.map_err(said)?,
        comments: db.comments().await.map_err(said)?,
    });
    let loaded = Loaded { profile, at: std::time::Instant::now(), schema: std::sync::Arc::clone(&schema) };
    caches.schemas.lock().await.insert(key, loaded);
    Ok(source(schema))
}

/// Times a model's SQL with unknown names goes back to it.
const NAME_RETRIES: usize = 2;

/// What the model is told when asked to write SQL; the context follows.
fn ask_system(engine: &str, context: &str) -> String {
    format!(
        "You write SQL for {engine}. Using only the tables and columns below (and what the team has \
         said about them, where it's given), write one query that answers the user's question. Reply with the query in a ```sql block, then one or two sentences \
         on what it does and any assumption you made (say how you read anything ambiguous). Write a \
         read-only query (SELECT or WITH); never change data. If these tables can't answer the \
         question, say so instead of writing a query. If you had to assume what a column or a value \
         means and nothing below says it, end with each assumption on a line of its own, as \
         `FACT: subject | relation | value` (for example `FACT: orders.status | means | 3 = cancelled`), \
         at most three: the user will confirm or correct them.\n\n{context}"
    )
}

/// What the model is told when asked what a query does; the context follows.
fn explain_system(engine: &str, context: &str) -> String {
    format!(
        "You explain SQL for {engine} to the person about to run it. Using the tables and columns below (and \
         what the team has said about them, where it's given), say in plain words what the user's query does \
         and what its result holds: a short paragraph, then the steps worth knowing (joins, filters, grouping, \
         anything slow on big tables) as short lines starting with `- `. If something in it looks wrong (a join \
         that multiplies rows, a filter that can't match, a name not below), say so, and only then add the \
         query as you'd write it in a ```sql block. Don't write a query otherwise.\n\n{context}"
    )
}

/// The system prompt and the user's message: to write SQL for a
/// question, or (`explain`) to say what the SQL in `question` does.
fn ask_or_explain(explain: bool, engine: &str, context: &str, question: &str) -> (String, String) {
    if explain {
        (explain_system(engine, context), format!("What does this query do?\n\n```sql\n{question}\n```"))
    } else {
        (ask_system(engine, context), question.to_string())
    }
}

/// `ask`: `{ connection, question, project?, model?, share_once?, explain? }`;
/// with `explain`, `question` is SQL the model says the meaning of. A
/// model writes SQL for the question over the connection's schema (the
/// tables `context` picks), its names are checked against the schema and
/// sent back to it when they're not there; the SQL is returned, never run.
/// A model not on this machine is shown the schema only if the project
/// shares it (or the user said yes this once: `share_once`).
pub async fn ask(
    config_dir: &Path,
    caches: &Caches,
    id: &Value,
    params: &Value,
    write: &mut OwnedWriteHalf,
) -> (Result<Value, RpcError>, Value) {
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let (Some(connection), Some(question)) = (text("connection"), text("question")) else {
        return (Err(RpcError::new(code::INVALID_PARAMS, "ask needs `connection` and `question`")), Value::Null);
    };
    let mut about = json!({ "connection": connection });
    let source = match load(config_dir, caches, text("project"), connection, text("dir"), false).await {
        Ok(source) => source,
        Err(e) => return (Err(RpcError::new(code::DATABASE, e)), about),
    };
    let built = built_for(config_dir, caches, &source.project_id, &source.schema, question).await;
    if built.tables.is_empty() {
        return (Err(RpcError::new(code::DATABASE, built.text.trim().to_string())), about);
    }
    // The model first, its key only once the schema may go to it: a
    // refused question mustn't wake up a password manager.
    let configs = match configured(config_dir) {
        Ok(configs) => configs,
        Err(e) => return (Err(e), about),
    };
    let config = match models::pick(&configs, text("model")) {
        Ok(config) => config.clone(),
        Err(e) => return (Err(RpcError::new(code::MODEL, e.message)), about),
    };
    about["model"] = json!(config.name);
    let once = params.get("share_once").and_then(Value::as_bool).unwrap_or(false);
    if source.share == crate::config::store::ShareSchema::Local && !models::on_this_machine(&config) && !once {
        let message = format!(
            "project '{}' shares its schemas only with models on this machine, and {} ({}) isn't one: \
             its table and column names would go to {}",
            source.project,
            config.name,
            config.model,
            config.url()
        );
        let data = json!({ "project": source.project, "project_id": source.project_id, "model": config.name, "url": config.url() });
        return (Err(RpcError::with_data(code::NOT_SHARED, message, data)), about);
    }
    let (config, key) = match model_and_key(config_dir, caches, Some(&config.name), id, write).await {
        Ok(found) => found,
        Err(e) => return (Err(e), about),
    };
    let explain = params.get("explain").and_then(Value::as_bool).unwrap_or(false);
    about["explain"] = json!(explain);
    let (system, content) = ask_or_explain(explain, &source.schema.engine, &built.text, question);
    let mut prompt = Prompt {
        system: Some(system),
        messages: vec![models::Message { role: "user".into(), content }],
        max_tokens: None,
    };
    let (mut input_tokens, mut output_tokens, mut attempts) = (0, 0, 0);
    let (sql, said, problems, reply) = loop {
        attempts += 1;
        let reply = match streamed(&config, key.as_deref(), &prompt, id, "ask.delta", write).await {
            Ok(reply) => reply,
            Err(e) => {
                refused_key(caches, &config, &e);
                return (Err(RpcError::new(code::MODEL, e.message)), about);
            }
        };
        input_tokens += reply.input_tokens;
        output_tokens += reply.output_tokens;
        // An explanation is words (its blocks quoted): no SQL to check.
        let (sql, said) =
            if explain { (None, reply.text.trim().to_string()) } else { super::names::split_answer(&reply.text) };
        let problems = sql
            .as_deref()
            .map(|sql| super::names::problems(sql, source.schema.dialect, &source.schema))
            .unwrap_or_default();
        if problems.is_empty() || attempts > NAME_RETRIES {
            break (sql, said, problems, reply);
        }
        // Back to the model, in the same conversation.
        prompt.messages.push(models::Message { role: "assistant".into(), content: reply.text.clone() });
        let feedback = format!(
            "That doesn't fit the schema: {}. Use only the tables and columns given, and write the query again.",
            problems.join("; ")
        );
        prompt.messages.push(models::Message { role: "user".into(), content: feedback });
    };
    about["tables"] = json!(built.tables.len());
    about["attempts"] = json!(attempts);
    about["input_tokens"] = json!(input_tokens);
    about["output_tokens"] = json!(output_tokens);
    let tables: Vec<Value> = built.tables.iter().map(|t| json!({ "name": t.name, "why": t.why })).collect();
    about["facts"] = json!(built.facts.len());
    // (An explanation's words are kept whole: it suggests no facts.)
    let (said, suggested) = if explain { (said, Vec::new()) } else { suggestions(&said) };
    let suggested = keep_suggestions(config_dir, caches, &source.project_id, &config.name, &suggested).await;
    about["suggested"] = json!(suggested.len());
    let result = json!({
        "sql": sql,
        "said": said,
        "suggested": suggested.iter().map(super::memory::Fact::to_json).collect::<Vec<_>>(),
        "problems": problems,
        "tables": tables,
        "facts": built.facts,
        "model": config.name,
        "provider_model": reply.model,
        "stop_reason": reply.stop_reason,
        "attempts": attempts,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens },
    });
    (Ok(result), about)
}

/// The `FACT: subject | relation | value` lines a model ended its note
/// with, taken out of it: (the note without them, the facts).
fn suggestions(said: &str) -> (String, Vec<(String, String, String)>) {
    let mut kept = Vec::new();
    let mut facts = Vec::new();
    for line in said.lines() {
        // `- FACT:`, `**FACT:**`, `_FACT:_`: markdown around it isn't part of it.
        let fact = line.trim().trim_start_matches(['-', '*', '_', '`', ' ']).strip_prefix("FACT:").map(|f| {
            let f = f.trim_start_matches(['*', '_', '`', ' ']);
            let parts: Vec<String> = f.trim().trim_end_matches('`').split('|').map(|p| p.trim().to_string()).collect();
            parts
        });
        match fact {
            Some(parts) if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) && facts.len() < 3 => {
                let [s, r, v]: [String; 3] = parts.try_into().unwrap_or_default();
                facts.push((s, r, v));
            }
            Some(_) => {}
            None => kept.push(line),
        }
    }
    (kept.join("\n").trim().to_string(), facts)
}

/// Keeps what a model suggested as unconfirmed facts (once each), so the
/// user can confirm them; returns them, with those already waiting to be
/// confirmed (the answer rests on them just the same).
async fn keep_suggestions(
    config_dir: &Path,
    caches: &Caches,
    project_id: &str,
    model: &str,
    facts: &[(String, String, String)],
) -> Vec<super::memory::Fact> {
    if facts.is_empty() {
        return Vec::new();
    }
    let kept = async {
        let conn = caches.memory(config_dir, project_id).await?;
        let mut conn = conn.lock().await;
        let known = super::memory::list(&mut conn, None).await?;
        let mut kept = Vec::new();
        for (subject, relation, value) in facts {
            let same = |f: &super::memory::Fact| {
                f.subject.eq_ignore_ascii_case(subject) && f.value.eq_ignore_ascii_case(value)
            };
            if let Some(waiting) = known.iter().find(|f| same(f)) {
                if !waiting.confirmed {
                    kept.push(waiting.clone());
                }
                continue;
            }
            if super::memory::rejected(&mut conn, subject, value).await? {
                continue;
            }
            let source = format!("model:{model}");
            kept.push(super::memory::add(&mut conn, subject, relation, value, &source, false).await?);
        }
        Ok::<_, String>(kept)
    };
    kept.await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "gateway: couldn't keep a model's suggested facts");
        Vec::new()
    })
}

/// `memory.add` / `memory.list` / `memory.forget` / `memory.confirm` /
/// `memory.export`, each with `project?`. Returns what the log may keep
/// (the project, never a fact's text).
pub async fn memory(
    config_dir: &Path,
    caches: &Caches,
    method: &str,
    params: &Value,
) -> (Result<Value, RpcError>, Value) {
    use super::memory as mem;
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let project = match find_project(config_dir, text("project")) {
        Ok(p) => p,
        Err(e) => return (Err(RpcError::new(code::DATABASE, e)), Value::Null),
    };
    let about = json!({ "project": project.name });
    let conn = match caches.memory(config_dir, &project.id).await {
        Ok(conn) => conn,
        Err(e) => return (Err(RpcError::new(code::MEMORY, e)), about),
    };
    let mut conn = conn.lock().await;
    let id = params.get("id").and_then(Value::as_i64);
    let failed = |e: String| RpcError::new(code::MEMORY, e);
    let outcome = match method {
        "memory.add" => {
            // The user's own word: confirmed. (A model's suggestion would
            // come with `source` and wait to be confirmed.)
            // As typed (`text`), read here so the app and the command line
            // read it alike; or already apart.
            let parsed = text("text").and_then(mem::parse_fact);
            let (subject, relation, value) = match (&parsed, text("subject"), text("value")) {
                (Some((s, r, v)), _, _) => (s.as_str(), r.as_str(), v.as_str()),
                (None, Some(s), Some(v)) => (s, text("relation").unwrap_or("means"), v),
                _ => {
                    let needs = "memory.add needs the fact: what it's about, then what's said (`text`)";
                    return (Err(RpcError::new(code::INVALID_PARAMS, needs)), about);
                }
            };
            mem::add(&mut conn, subject, relation, value, "user", true)
                .await
                .map(|f| f.to_json())
                .map_err(failed)
        }
        "memory.list" => mem::list(&mut conn, text("search"))
            .await
            .map(|facts| json!({ "project": project.name, "facts": facts.iter().map(mem::Fact::to_json).collect::<Vec<_>>() }))
            .map_err(failed),
        "memory.forget" | "memory.confirm" => {
            let Some(id) = id else {
                return (Err(RpcError::new(code::INVALID_PARAMS, format!("{method} needs the fact's `id`"))), about);
            };
            let done = if method == "memory.forget" { mem::forget(&mut conn, id).await } else { mem::confirm(&mut conn, id).await };
            match done {
                Ok(true) => Ok(json!({ "id": id })),
                Ok(false) => Err(RpcError::new(code::MEMORY, format!("no fact {id} in project '{}'", project.name))),
                Err(e) => Err(failed(e)),
            }
        }
        "memory.learn" => {
            let Some(folder) = text("folder") else {
                return (Err(RpcError::new(code::INVALID_PARAMS, "memory.learn needs the `folder`")), about);
            };
            let folder = crate::config::settings::expand_home(folder);
            let reading = folder.clone();
            let files = tokio::task::spawn_blocking(move || super::migrations::read_folder(&reading)).await;
            match files {
                Ok(Ok(files)) => {
                    let engine = super::migrations::engine_of(&project.connections);
                    let learned = super::migrations::learn(&files, engine);
                    let source = format!("file:{}", folder.display());
                    mem::replace_source(&mut conn, &source, &learned)
                        .await
                        .map(|n| json!({ "files": files.len(), "facts": n, "folder": folder.display().to_string() }))
                        .map_err(failed)
                }
                Ok(Err(e)) => Err(RpcError::new(code::MEMORY, e)),
                Err(e) => Err(failed(e.to_string())),
            }
        }
        "memory.export" => mem::list(&mut conn, None)
            .await
            .map(|facts| json!({ "text": mem::export(&project.name, &facts), "facts": facts.len() }))
            .map_err(failed),
        _ => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("no method `{method}`"))),
    };
    // Only after the database failed (its errors start "memory:"), not a
    // wrong fact number or folder.
    if outcome.as_ref().is_err_and(|e| e.code == code::MEMORY && e.message.starts_with("memory:")) {
        // Open again next time: the file may have been moved or replaced.
        drop(conn);
        caches.forget_memory(&project.id).await;
    }
    (outcome, about)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fact_lines_in_markdown_are_read() {
        let (said, facts) =
            super::suggestions("Counts.\n**FACT:** orders.status | means | 3 = cancelled\n_FACT:_ a | is | b");
        assert_eq!(said, "Counts.");
        let subjects: Vec<&str> = facts.iter().map(|(s, _, _)| s.as_str()).collect();
        assert_eq!(subjects, ["orders.status", "a"]);
    }
}
