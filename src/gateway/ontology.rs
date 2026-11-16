//! The gateway's `ontology` and `ontology.import`: a connection's
//! database as an ontology (its catalog with what the project's memory
//! says of it), to read or export, and an edited file loaded back.

use podelis_ai::memory::{self as mem, Fact, MemoryError};

use super::call::Call;
use super::methods::{
    ImportApplied, ImportParams, ImportPlan, ImportResult, OntologyParams, OntologyResult, OutlineSection, Scope,
};
use super::protocol::{RpcError, code};
use super::source::{Source, load};

/// `ontology`: the connection's ontology: its database's catalog as
/// concepts and relations, with what the project's memory says of them
/// (suggestions too, marked), as `text`, as SKOS `turtle`, as `yaml` to
/// edit and as the browser's `outline`; only `format`'s, when it names one
/// (each is a pass over every concept). Sends nothing to any model.
pub(super) async fn ontology(call: &mut Call<'_>, p: OntologyParams) -> Result<OntologyResult, RpcError> {
    let (source, facts) = ontology_source(call, &p.scope, p.refresh).await?;
    let connection = p.scope.connection().unwrap_or_default();
    let ontology = (*source.catalog).clone().with_facts(&facts);
    call.log("concepts", ontology.concepts.len());
    let wanted = |name: &str| p.format.as_deref().is_none_or(|f| f == name);
    let mut result = OntologyResult { database: source.database.clone(), ..OntologyResult::default() };
    if wanted("text") {
        result.text = Some(ontology.describe());
    }
    if wanted("turtle") {
        let base = format!(
            "urn:podelis:{}:{}/",
            podelis_ai::ontology::iri_part(source.project_id.as_str()),
            podelis_ai::ontology::iri_part(&source.database)
        );
        result.turtle = Some(ontology.to_turtle(&base));
    }
    if wanted("yaml") {
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
        result.yaml = Some(podelis_ai::ontology::file::export(&ontology, &facts, &source.database, &title));
    }
    if wanted("outline") {
        let outline = podelis_ai::ontology::outline::outline(&ontology, &facts);
        result.outline = Some(
            outline
                .into_iter()
                .map(|s| OutlineSection { kind: s.kind.as_str().to_string(), subject: s.subject, facts: s.facts })
                .collect(),
        );
    }
    Ok(result)
}

/// `ontology.import`: what loading the YAML `text` would change in the
/// memory of the connection's database (`add`, `confirm`, `forget`, `move`:
/// each a fact's line); with `apply`, changes it so and says how many.
/// Added facts are the user's, confirmed, and about this database.
pub(super) async fn import(call: &mut Call<'_>, p: ImportParams) -> Result<ImportResult, RpcError> {
    use podelis_ai::ontology::file;
    // Before connecting to anything: a call without the file is answered at once.
    let Some(text) = p.text.as_deref() else {
        return Err(RpcError::new(code::INVALID_PARAMS, "ontology.import needs the file's `text`"));
    };
    let (source, facts) = ontology_source(call, &p.scope, p.refresh).await?;
    let read = file::read(text).map_err(|e| RpcError::new(code::INVALID_PARAMS, e))?;
    if let Some(other) = read.database.as_ref().filter(|d| !d.eq_ignore_ascii_case(&source.database)) {
        let e = format!(
            "the file is the ontology of database '{other}', not of '{}': load it with a connection to {other}",
            source.database
        );
        return Err(RpcError::new(code::INVALID_PARAMS, e));
    }
    let ontology = (*source.catalog).clone().with_facts(&facts);
    let plan = podelis_ai::ontology::plan::plan(&ontology, &facts, &read.wanted, read.exported.as_deref());
    let every = |f: &Fact| if f.database.is_empty() { " (every database)" } else { "" };
    if !p.apply {
        return Ok(ImportResult::Plan(ImportPlan {
            add: plan.add.iter().map(|w| format!("{} {} {}", w.subject, w.relation, w.value)).collect(),
            confirm: plan.confirm.iter().map(|f| format!("{}{}", f.line(), every(f))).collect(),
            forget: plan.forget.iter().map(|f| format!("{}{}", f.line(), every(f))).collect(),
            moved: plan
                .moved
                .iter()
                .map(|(f, to_every)| {
                    let to = if *to_every { "every database" } else { source.database.as_str() };
                    format!("{} → {to}", f.line())
                })
                .collect(),
            kept: plan.kept,
            undated: plan.undated,
        }));
    }
    // All of it, or (on any failure) none of it.
    let applied = async {
        use sqlx::Connection as _;
        let conn = call.caches.memory(call.config_dir, &source.project_id).await?;
        let mut conn = conn.lock().await;
        let mut tx = conn.begin().await?;
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
        tx.commit().await?;
        Ok::<_, MemoryError>(())
    };
    applied.await.map_err(|e| memory_failed(call, &source, e))?;
    Ok(ImportResult::Applied(ImportApplied {
        added: plan.add.len(),
        confirmed: plan.confirm.len(),
        forgotten: plan.forget.len(),
        moved: plan.moved.len(),
    }))
}

fn memory_failed(call: &Call<'_>, source: &Source, e: MemoryError) -> RpcError {
    call.caches.memory_failed(&source.project_id, &e);
    RpcError::new(code::MEMORY, String::from(e))
}

/// The connection's schema and its database's facts (all of them,
/// suggestions too), for `ontology` and `ontology.import`.
async fn ontology_source(
    call: &mut Call<'_>,
    scope: &Scope,
    refresh: Option<bool>,
) -> Result<(Source, Vec<Fact>), RpcError> {
    let Some(connection) = scope.connection() else {
        return Err(RpcError::new(code::INVALID_PARAMS, "ontology needs `connection`"));
    };
    call.log("connection", connection);
    // The command line reads the catalog as it is now; the app's browser
    // may take the kept one (`refresh: false`).
    let source = load(call, scope.project(), connection, scope.dir(), refresh.unwrap_or(true))
        .await
        .map_err(|e| RpcError::new(code::DATABASE, e))?;
    let facts = async {
        let conn = call.caches.memory(call.config_dir, &source.project_id).await?;
        let mut conn = conn.lock().await;
        mem::list(&mut conn, None, Some(&source.database)).await
    };
    match facts.await {
        Ok(facts) => Ok((source, facts)),
        Err(e) => Err(memory_failed(call, &source, e)),
    }
}

/// What `ontology.import`'s plan would change, as the app's window and
/// `podelis ontology … import` both list it: a heading for each kind of
/// change that has some (`true`), then a line for each change. Empty:
/// nothing to change.
pub fn plan_lines(plan: &ImportPlan) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if plan.undated && !plan.is_empty() {
        out.push((false, t!("memory.plan_undated").to_string()));
    }
    for (key, list) in plan.sections() {
        let title = match key {
            "add" => t!("memory.plan_add"),
            "confirm" => t!("memory.plan_confirm"),
            "move" => t!("memory.plan_move"),
            _ => t!("memory.plan_forget"),
        };
        if !list.is_empty() {
            out.push((true, format!("{title} ({})", list.len())));
            out.extend(list.iter().map(|l| (false, l.clone())));
        }
    }
    out
}
