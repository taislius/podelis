//! The gateway's `memory.*` methods: a project's facts, to list, add,
//! change, confirm, forget, move between databases, and export.

use std::path::Path;

use serde_json::{Value, json};

use super::ai::{Caches, find_project};
use super::protocol::{RpcError, code};

/// `memory.move` `{ id, every }`: fact `id` about every database, or about
/// the connection's (`database`) alone.
async fn move_fact(
    conn: &mut sqlx::SqliteConnection,
    params: &Value,
    id: Option<i64>,
    database: Option<&str>,
    project: &str,
) -> Result<Value, RpcError> {
    let every = params.get("every").and_then(Value::as_bool).unwrap_or(false);
    let to = if every { Some("") } else { database };
    let (Some(id), Some(to)) = (id, to) else {
        let needs = "memory.move needs the fact's `id`, and `every` or a `connection` to move it to";
        return Err(RpcError::new(code::INVALID_PARAMS, needs));
    };
    match podelis_ai::memory::set_database(conn, id, to).await {
        Ok(true) => Ok(json!({ "id": id, "database": to })),
        Ok(false) => Err(RpcError::new(code::MEMORY, format!("no fact {id} in project '{project}'"))),
        Err(e) => Err(RpcError::new(code::MEMORY, e)),
    }
}

/// The database a project's saved connection is to, as the memory names it.
fn database_of(project: &crate::config::store::Project, connection: &str) -> Result<String, String> {
    let found = project.connections.iter().find(|c| c.name.eq_ignore_ascii_case(connection));
    found.map(crate::config::store::ConnectionProfile::database_key).ok_or_else(|| {
        let known: Vec<&str> = project.connections.iter().map(|c| c.name.as_str()).collect();
        format!("no connection '{connection}' in project '{}' (there: {})", project.name, known.join(", "))
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
    use podelis_ai::memory as mem;
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let project = match find_project(config_dir, text("project")) {
        Ok(p) => p,
        Err(e) => return (Err(RpcError::new(code::DATABASE, e)), Value::Null),
    };
    let about = json!({ "project": project.name });
    // With a `connection`, about its database: what's added is about it,
    // what's listed is its and every database's. Without one, the whole
    // project (and what's added is about every database).
    let database = match text("connection").map(|name| database_of(&project, name)).transpose() {
        Ok(database) => database,
        Err(e) => return (Err(RpcError::new(code::DATABASE, e)), about),
    };
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
            mem::add(&mut conn, subject, relation, value, "user", true, database.as_deref().unwrap_or_default())
                .await
                .map(|f| f.to_json())
                .map_err(failed)
        }
        "memory.update" => {
            let (Some(id), Some(text)) = (id, text("text").filter(|t| !t.trim().is_empty())) else {
                let needs = "memory.update needs the fact's `id` and what it says now (`text`)";
                return (Err(RpcError::new(code::INVALID_PARAMS, needs)), about);
            };
            match mem::update(&mut conn, id, text).await {
                Ok(Some(f)) => Ok(f.to_json()),
                Ok(None) => Err(RpcError::new(code::MEMORY, format!("no fact {id} in project '{}'", project.name))),
                Err(e) => Err(failed(e)),
            }
        }
        "memory.list" => mem::list(&mut conn, text("search"), database.as_deref())
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
        "memory.move" => move_fact(&mut conn, params, id, database.as_deref(), &project.name).await,
        "memory.learn" => {
            let Some(folder) = text("folder") else {
                return (Err(RpcError::new(code::INVALID_PARAMS, "memory.learn needs the `folder`")), about);
            };
            let folder = crate::config::settings::expand_home(folder);
            let reading = folder.clone();
            let files = tokio::task::spawn_blocking(move || podelis_ai::migrations::read_folder(&reading)).await;
            match files {
                Ok(Ok(files)) => {
                    let engine = podelis_ai::migrations::engine_of(project.connections.iter().map(|c| c.engine));
                    let learned = podelis_ai::migrations::learn(&files, engine);
                    let source = format!("file:{}", folder.display());
                    mem::replace_source(&mut conn, &source, &learned, database.as_deref().unwrap_or_default())
                        .await
                        .map(|n| json!({ "files": files.len(), "facts": n, "folder": folder.display().to_string() }))
                        .map_err(failed)
                }
                Ok(Err(e)) => Err(RpcError::new(code::MEMORY, e)),
                Err(e) => Err(failed(e.to_string())),
            }
        }
        "memory.export" => mem::list(&mut conn, None, database.as_deref())
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
