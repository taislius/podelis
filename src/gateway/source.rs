//! Where a call's database comes from: the project (read from
//! `connections.toml` without changing it), its saved connection, and
//! that database's schema, loaded through a read-only connection opened as
//! the app opens it, and kept a while.

use std::path::Path;
use std::sync::Arc;

use crate::config::store::{ConnectionProfile, Project, ProjectId, ShareSchema};
use podelis_ai::context::Schema;
use podelis_ai::ontology::Ontology;

use super::caches::Loaded;
use super::call::Call;

/// The project named (the last one opened when `None`), read from
/// `connections.toml` without changing it: a background process mustn't
/// move the user's file aside.
pub(crate) fn find_project(config_dir: &Path, name: Option<&str>) -> Result<Project, String> {
    let (store, warning) = crate::config::store::ConnectionStore::peek(config_dir.join("connections.toml"));
    // The app names it by id (`id:<id>`): names are matched ignoring case,
    // and two projects may differ only in case.
    let found = match name {
        Some(name) => match name.strip_prefix("id:") {
            Some(id) => ProjectId::from_written(id).and_then(|id| store.project(&id)),
            None => store.project_by_name(name),
        },
        None => store.last_project.as_ref().and_then(|id| store.project(id)),
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

/// The project's saved connection named `name` (ignoring case).
pub(super) fn connection_of<'a>(project: &'a Project, name: &str) -> Result<&'a ConnectionProfile, String> {
    project.connections.iter().find(|c| c.name.eq_ignore_ascii_case(name)).ok_or_else(|| {
        let known: Vec<&str> = project.connections.iter().map(|c| c.name.as_str()).collect();
        format!("no connection '{name}' in project '{}' (there: {})", project.name, known.join(", "))
    })
}

/// A connection's schema, and its project's name and setting for
/// sharing schemas with models.
pub(super) struct Source {
    pub(super) schema: Arc<Schema>,
    pub(super) catalog: Arc<Ontology>,
    pub(super) project: String,
    pub(super) project_id: ProjectId,
    /// What its schema is kept by.
    pub(super) key: super::caches::SchemaKey,
    pub(super) share: ShareSchema,
    /// The database it's a connection to, as the memory names it (its
    /// facts are about it, or about every database).
    pub(super) database: String,
}

/// The connection's schema: kept, or loaded through a read-only
/// connection opened as the app opens it (keychain, password command).
/// `refresh` loads it again whatever is kept.
pub(super) async fn load(
    call: &Call<'_>,
    project: Option<&str>,
    connection: &str,
    dir: Option<&str>,
    refresh: bool,
) -> Result<Source, String> {
    let project = find_project(call.config_dir, project)?;
    let saved = connection_of(&project, connection)?;
    // A relative SQLite file is the asker's, from its folder.
    let profile = crate::config::store::opened_from(saved, dir);
    let key = (project.id.clone(), profile.name.clone());
    let source = |loaded: &Loaded| Source {
        schema: Arc::clone(&loaded.schema),
        catalog: Arc::clone(&loaded.catalog),
        project: project.name.clone(),
        project_id: project.id.clone(),
        key: key.clone(),
        share: project.share_schema,
        database: saved.database_key(),
    };
    // One kept is used at once, whatever load runs meanwhile (a slow or
    // unreachable server mustn't hold up a question the cache answers).
    // With `refresh`, only one loaded since it was asked.
    let asked = std::time::Instant::now();
    let kept = || call.caches.schema(&key, &profile).filter(|l| !refresh || l.at >= asked);
    if let Some(loaded) = kept() {
        return Ok(source(&loaded));
    }
    // One load at a time: a question that came while one ran takes what
    // it loaded, or how it failed (not waiting out the server again).
    let loading = call.caches.loading(&key);
    let mut failed = loading.lock().await;
    if let Some(loaded) = kept() {
        return Ok(source(&loaded));
    }
    if let Some((_, e)) = failed.as_ref().filter(|(at, _)| *at >= asked) {
        return Err(e.clone());
    }
    match read_schema(&project.id, profile).await {
        Ok(loaded) => {
            *failed = None;
            call.caches.keep_schema(key.clone(), loaded.clone());
            Ok(source(&loaded))
        }
        Err(e) => {
            *failed = Some((std::time::Instant::now(), e.clone()));
            Err(e)
        }
    }
}

/// Reads the schema of `profile`'s database.
async fn read_schema(project_id: &ProjectId, profile: ConnectionProfile) -> Result<Loaded, String> {
    let for_password = (project_id.clone(), profile.clone());
    let (password, _) =
        tokio::task::spawn_blocking(move || crate::config::store::saved_password(&for_password.0, &for_password.1))
            .await
            .map_err(|e| e.to_string())?;
    let mut params = crate::config::draft::ConnectionDraft::from_profile(&profile, password).params()?;
    // Reading the catalog is all it does: shut off as agents' connections
    // are (which it can then share a DuckDB file with, in the gateway).
    params.read_only = true;
    params.for_agents = true;
    let db =
        crate::db::Database::connect(params).await.map_err(|e| crate::db::with_hint(&crate::db::error_message(&e)))?;
    let said = |e: anyhow::Error| crate::db::error_message(&e);
    let schema = Arc::new(Schema {
        engine: profile.engine.label().to_string(),
        dialect: profile.engine,
        tables: db.catalog(false).await.map_err(said)?,
        links: db.links().await.map_err(said)?,
        comments: db.comments().await.map_err(said)?,
    });
    let catalog = Arc::new(Ontology::from_catalog(&schema));
    Ok(Loaded { profile, at: std::time::Instant::now(), schema, catalog })
}

/// The context for `question`, with the project's memory read once: the
/// facts matching the question help choose the tables ("revenue is
/// payment.amount" brings `payment`), then those about the chosen tables
/// are shown with them. Memory that can't be read is logged and left
/// out: asking still works without it.
pub(super) async fn context_for(call: &Call<'_>, source: &Source, question: &str) -> podelis_ai::context::Context {
    let found = async {
        let conn = call.caches.memory(call.config_dir, &source.project_id).await?;
        let mut conn = conn.lock().await;
        podelis_ai::memory::for_question(&mut conn, question, Some(&source.database)).await
    };
    let memory = found.await.unwrap_or_else(|e| {
        call.caches.memory_failed(&source.project_id, &e);
        tracing::warn!(error = %e, "gateway: the project's memory couldn't be read");
        podelis_ai::memory::ForQuestion::default()
    });
    // With the team's other names, kept: not a copy of the catalog for
    // every question.
    let catalog = call.caches.aliased(&source.key, &source.catalog, memory.all());
    podelis_ai::context::for_question(&source.schema, &catalog, question, &memory)
}
