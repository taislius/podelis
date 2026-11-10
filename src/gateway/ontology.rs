//! The gateway's `ontology` and `ontology.import`: a connection's
//! database as an ontology (its catalog with what the project's memory
//! says of it), to read or export, and an edited file loaded back.

use std::path::Path;

use serde_json::{Value, json};

use super::ai::{Caches, Source, load};
use super::protocol::{RpcError, code};

/// `ontology`: `{ connection, project?, dir?, format? }`. The connection's
/// ontology: its database's catalog as concepts and relations, with what
/// the project's memory says of them (suggestions too, marked), as `text`,
/// as SKOS `turtle`, as `yaml` to edit and as the browser's `outline`;
/// only `format`'s, when it names one (each is a pass over every concept).
/// Sends nothing to any model. Returns what the log may keep.
pub async fn ontology(config_dir: &Path, caches: &Caches, params: &Value) -> (Result<Value, RpcError>, Value) {
    let (source, facts, _) = match ontology_source(config_dir, caches, params).await {
        Ok(found) => found,
        Err(failed) => return *failed,
    };
    let connection = params["connection"].as_str().unwrap_or_default().trim();
    let ontology = (*source.catalog).clone().with_facts(&facts);
    let base = format!(
        "urn:podelis:{}:{}/",
        podelis_ai::ontology::iri_part(&source.project_id),
        podelis_ai::ontology::iri_part(&source.database)
    );
    let title = format!(
        "Podėlis ontology · project {} · database {} · {}\n\
         Edit it, then load it back: podelis ontology -p '{}' '{connection}' import <this file>\n\
         suggested: a model's guesses, not shown to models. Move one under its table or term to confirm it,\n\
         delete it to reject it. stale: about a table or column the database doesn't have.",
        source.project,
        source.database,
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        source.project,
    );
    let format = params.get("format").and_then(Value::as_str);
    let wanted = |name: &str| format.is_none_or(|f| f == name);
    let mut result = json!({ "database": source.database });
    if wanted("text") {
        result["text"] = json!(ontology.describe());
    }
    if wanted("turtle") {
        result["turtle"] = json!(ontology.to_turtle(&base));
    }
    if wanted("yaml") {
        result["yaml"] = json!(podelis_ai::ontology::file::export(&ontology, &facts, &source.database, &title));
    }
    if wanted("outline") {
        let outline = podelis_ai::ontology::outline::outline(&ontology, &facts);
        result["outline"] = outline
            .iter()
            .map(|s| {
                json!({
                    "kind": s.kind.as_str(),
                    "subject": s.subject,
                    "facts": s.facts.iter().map(podelis_ai::memory::Fact::to_json).collect::<Vec<_>>(),
                })
            })
            .collect();
    }
    (Ok(result), json!({ "connection": connection, "concepts": ontology.concepts.len() }))
}

/// `ontology.import`: `{ connection, text, apply?, project?, dir? }`. What
/// loading the YAML `text` would change in the memory of the connection's
/// database (`add`, `confirm`, `forget`: each a fact's line); with
/// `apply`, changes it so and says how many. Added facts are the user's,
/// confirmed, and about this database.
pub async fn ontology_import(config_dir: &Path, caches: &Caches, params: &Value) -> (Result<Value, RpcError>, Value) {
    use podelis_ai::memory as mem;
    use podelis_ai::ontology::file;
    let (source, facts, about) = match ontology_source(config_dir, caches, params).await {
        Ok(found) => found,
        Err(failed) => return *failed,
    };
    let Some(text) = params["text"].as_str() else {
        return (Err(RpcError::new(code::INVALID_PARAMS, "ontology.import needs the file's `text`")), about);
    };
    let read = match file::read(text) {
        Ok(read) => read,
        Err(e) => return (Err(RpcError::new(code::INVALID_PARAMS, e)), about),
    };
    if let Some(other) = read.database.as_ref().filter(|d| !d.eq_ignore_ascii_case(&source.database)) {
        let e = format!(
            "the file is the ontology of database '{other}', not of '{}': load it with a connection to {other}",
            source.database
        );
        return (Err(RpcError::new(code::INVALID_PARAMS, e)), about);
    }
    let ontology = (*source.catalog).clone().with_facts(&facts);
    let plan = podelis_ai::ontology::plan::plan(&ontology, &facts, &read.wanted, read.exported.as_deref());
    let every = |f: &mem::Fact| if f.database.is_empty() { " (every database)" } else { "" };
    if params["apply"].as_bool() != Some(true) {
        let result = json!({
            "add": plan.add.iter().map(|w| format!("{} {} {}", w.subject, w.relation, w.value)).collect::<Vec<_>>(),
            "confirm": plan.confirm.iter().map(|f| format!("{}{}", f.line(), every(f))).collect::<Vec<_>>(),
            "forget": plan.forget.iter().map(|f| format!("{}{}", f.line(), every(f))).collect::<Vec<_>>(),
            "move": plan.moved.iter().map(|(f, to_every)| {
                let to = if *to_every { "every database" } else { source.database.as_str() };
                format!("{} → {to}", f.line())
            }).collect::<Vec<_>>(),
            "kept": plan.kept,
        });
        return (Ok(result), about);
    }
    // All of it, or (on any failure) none of it.
    let applied = async {
        use sqlx::Connection as _;
        let conn = caches.memory(config_dir, &source.project_id).await?;
        let mut conn = conn.lock().await;
        let mut tx = conn.begin().await.map_err(|e| format!("memory: {e}"))?;
        for w in &plan.add {
            let database = if w.everywhere == Some(true) { "" } else { source.database.as_str() };
            mem::add(&mut tx, &w.subject, &w.relation, &w.value, "user", true, database).await?;
        }
        for (f, to_every) in &plan.moved {
            mem::set_database(&mut tx, f.id, if *to_every { "" } else { &source.database }).await?;
        }
        for f in &plan.confirm {
            mem::confirm(&mut tx, f.id).await?;
        }
        for f in &plan.forget {
            mem::forget(&mut tx, f.id).await?;
        }
        tx.commit().await.map_err(|e| format!("memory: {e}"))
    };
    match applied.await {
        Ok(()) => (
            Ok(json!({
                "added": plan.add.len(),
                "confirmed": plan.confirm.len(),
                "forgotten": plan.forget.len(),
                "moved": plan.moved.len(),
            })),
            about,
        ),
        Err(e) => (Err(RpcError::new(code::MEMORY, e)), about),
    }
}

/// The connection's schema and its database's facts (all of them,
/// suggestions too), for `ontology` and `ontology.import`.
async fn ontology_source(
    config_dir: &Path,
    caches: &Caches,
    params: &Value,
) -> Result<(Source, Vec<podelis_ai::memory::Fact>, Value), Box<(Result<Value, RpcError>, Value)>> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let Some(connection) = text("connection") else {
        return Err(Box::new((Err(RpcError::new(code::INVALID_PARAMS, "ontology needs `connection`")), Value::Null)));
    };
    let about = json!({ "connection": connection });
    // The command line reads the catalog as it is now; the app's browser
    // may take the cached one (`refresh: false`).
    let refresh = params.get("refresh").and_then(Value::as_bool).unwrap_or(true);
    let source = match load(config_dir, caches, text("project"), connection, text("dir"), refresh).await {
        Ok(source) => source,
        Err(e) => return Err(Box::new((Err(RpcError::new(code::DATABASE, e)), about))),
    };
    let facts = async {
        let conn = caches.memory(config_dir, &source.project_id).await?;
        let mut conn = conn.lock().await;
        podelis_ai::memory::list(&mut conn, None, Some(&source.database)).await
    };
    match facts.await {
        Ok(facts) => Ok((source, facts, about)),
        Err(e) => Err(Box::new((Err(RpcError::new(code::MEMORY, e)), about))),
    }
}

/// What `ontology.import`'s plan would change, as the app's window and
/// `podelis ontology … import` both list it: a heading for each kind of
/// change that has some (`true`), then a line for each change. Empty:
/// nothing to change.
pub fn plan_lines(plan: &Value) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    for (key, title) in [
        ("add", t!("memory.plan_add")),
        ("confirm", t!("memory.plan_confirm")),
        ("move", t!("memory.plan_move")),
        ("forget", t!("memory.plan_forget")),
    ] {
        let list: Vec<&str> = plan[key].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if !list.is_empty() {
            out.push((true, format!("{title} ({})", list.len())));
            out.extend(list.into_iter().map(|l| (false, l.to_string())));
        }
    }
    out
}
