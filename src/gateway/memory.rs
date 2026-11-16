//! The gateway's `memory.*` methods: a project's facts, to list, add,
//! change, confirm, forget, move between databases, learn from a folder
//! of migrations, and export. The log keeps the project, never a fact's
//! text.

use podelis_ai::memory::{self as mem, Fact, FactId, MemoryError};

use super::caches::Memory;
use super::call::Call;
use super::methods::{
    ExportResult, FactIdResult, FactParams, LearnResult, MemoryAddParams, MemoryLearnParams, MemoryListParams,
    MemoryListResult, MemoryMoveParams, MemoryUpdateParams, MovedResult, Scope, given,
};
use super::protocol::{RpcError, code};
use super::source::{connection_of, find_project};
use crate::config::store::Project;

/// A project's memory, open, for a call about it.
struct Opened {
    project: Project,
    /// The connection's database, as the memory names it: what's added is
    /// about it, and what's listed is its and every database's. None: the
    /// whole project (and what's added is about every database).
    database: Option<String>,
    conn: Memory,
}

impl Opened {
    /// Says `e` to the client; a file that failed is opened again next time.
    fn failed(&self, call: &Call<'_>, e: MemoryError) -> RpcError {
        call.caches.memory_failed(&self.project.id, &e);
        RpcError::new(code::MEMORY, String::from(e))
    }

    fn no_fact(&self, id: FactId) -> RpcError {
        RpcError::new(code::MEMORY, format!("no fact {id} in project '{}'", self.project.name))
    }
}

async fn open(call: &mut Call<'_>, scope: &Scope) -> Result<Opened, RpcError> {
    let project = find_project(call.config_dir, scope.project()).map_err(|e| RpcError::new(code::DATABASE, e))?;
    call.log("project", &project.name);
    let database = match scope.connection() {
        Some(name) => Some(connection_of(&project, name).map_err(|e| RpcError::new(code::DATABASE, e))?.database_key()),
        None => None,
    };
    let conn = call.caches.memory(call.config_dir, &project.id).await.map_err(|e| {
        call.caches.memory_failed(&project.id, &e);
        RpcError::new(code::MEMORY, String::from(e))
    })?;
    Ok(Opened { project, database, conn })
}

/// `memory.add`: the user's own word, so confirmed. (A model's suggestion
/// comes with its `source` and waits to be confirmed.) As typed (`text`),
/// read here so the app and the command line read it alike; or already
/// apart.
pub(super) async fn add(call: &mut Call<'_>, p: MemoryAddParams) -> Result<Fact, RpcError> {
    let opened = open(call, &p.scope).await?;
    let parsed = given(p.text.as_deref()).and_then(mem::parse_fact);
    let (subject, relation, value) = match (&parsed, given(p.subject.as_deref()), given(p.value.as_deref())) {
        (Some((s, r, v)), _, _) => (s.as_str(), r.as_str(), v.as_str()),
        (None, Some(s), Some(v)) => (s, given(p.relation.as_deref()).unwrap_or("means"), v),
        _ => {
            let needs = "memory.add needs the fact: what it's about, then what's said (`text`)";
            return Err(RpcError::new(code::INVALID_PARAMS, needs));
        }
    };
    let database = opened.database.as_deref().unwrap_or_default();
    let mut conn = opened.conn.lock().await;
    mem::add(&mut conn, subject, relation, value, "user", true, database).await.map_err(|e| opened.failed(call, e))
}

/// `memory.update`: fact `id` now says `text`.
pub(super) async fn update(call: &mut Call<'_>, p: MemoryUpdateParams) -> Result<Fact, RpcError> {
    let opened = open(call, &p.scope).await?;
    let (Some(id), Some(text)) = (p.id, given(p.text.as_deref())) else {
        let needs = "memory.update needs the fact's `id` and what it says now (`text`)";
        return Err(RpcError::new(code::INVALID_PARAMS, needs));
    };
    let mut conn = opened.conn.lock().await;
    match mem::update(&mut conn, id, text).await {
        Ok(Some(fact)) => Ok(fact),
        Ok(None) => Err(opened.no_fact(id)),
        Err(e) => Err(opened.failed(call, e)),
    }
}

/// `memory.list`: the facts (those matching `search`), oldest first.
pub(super) async fn list(call: &mut Call<'_>, p: MemoryListParams) -> Result<MemoryListResult, RpcError> {
    let opened = open(call, &p.scope).await?;
    let mut conn = opened.conn.lock().await;
    let facts = mem::list(&mut conn, given(p.search.as_deref()), opened.database.as_deref())
        .await
        .map_err(|e| opened.failed(call, e))?;
    Ok(MemoryListResult { project: opened.project.name.clone(), facts })
}

/// `memory.forget`.
pub(super) async fn forget(call: &mut Call<'_>, p: FactParams) -> Result<FactIdResult, RpcError> {
    by_id(call, p, "memory.forget", false).await
}

/// `memory.confirm`: a model's suggestion, now the user's.
pub(super) async fn confirm(call: &mut Call<'_>, p: FactParams) -> Result<FactIdResult, RpcError> {
    by_id(call, p, "memory.confirm", true).await
}

async fn by_id(call: &mut Call<'_>, p: FactParams, method: &str, confirm: bool) -> Result<FactIdResult, RpcError> {
    let opened = open(call, &p.scope).await?;
    let Some(id) = p.id else {
        return Err(RpcError::new(code::INVALID_PARAMS, format!("{method} needs the fact's `id`")));
    };
    let mut conn = opened.conn.lock().await;
    let done = if confirm { mem::confirm(&mut conn, id).await } else { mem::forget(&mut conn, id).await };
    match done {
        Ok(true) => Ok(FactIdResult { id }),
        Ok(false) => Err(opened.no_fact(id)),
        Err(e) => Err(opened.failed(call, e)),
    }
}

/// `memory.move`: fact `id` about every database, or about the
/// connection's alone.
pub(super) async fn move_fact(call: &mut Call<'_>, p: MemoryMoveParams) -> Result<MovedResult, RpcError> {
    let opened = open(call, &p.scope).await?;
    let to = if p.every { Some("") } else { opened.database.as_deref() };
    let (Some(id), Some(to)) = (p.id, to) else {
        let needs = "memory.move needs the fact's `id`, and `every` or a `connection` to move it to";
        return Err(RpcError::new(code::INVALID_PARAMS, needs));
    };
    let mut conn = opened.conn.lock().await;
    match mem::set_database(&mut conn, id, to).await {
        Ok(true) => Ok(MovedResult { id, database: to.to_string() }),
        Ok(false) => Err(opened.no_fact(id)),
        Err(e) => Err(opened.failed(call, e)),
    }
}

/// `memory.learn`: a folder of migrations read into facts, replacing what
/// was learned from it before.
pub(super) async fn learn(call: &mut Call<'_>, p: MemoryLearnParams) -> Result<LearnResult, RpcError> {
    let opened = open(call, &p.scope).await?;
    let Some(folder) = given(p.folder.as_deref()) else {
        return Err(RpcError::new(code::INVALID_PARAMS, "memory.learn needs the `folder`"));
    };
    let folder = crate::config::settings::expand_home(folder);
    let reading = folder.clone();
    let files = match tokio::task::spawn_blocking(move || podelis_ai::migrations::read_folder(&reading)).await {
        Ok(Ok(files)) => files,
        Ok(Err(e)) => return Err(RpcError::new(code::MEMORY, e)),
        Err(e) => return Err(RpcError::new(code::MEMORY, e.to_string())),
    };
    let engine = podelis_ai::migrations::engine_of(opened.project.connections.iter().map(|c| c.engine));
    let learned = podelis_ai::migrations::learn(&files, engine);
    let source = format!("file:{}", folder.display());
    let database = opened.database.as_deref().unwrap_or_default();
    let mut conn = opened.conn.lock().await;
    let facts =
        mem::replace_source(&mut conn, &source, &learned, database).await.map_err(|e| opened.failed(call, e))?;
    Ok(LearnResult { files: files.len(), facts, folder: folder.display().to_string() })
}

/// `memory.export`: the facts as a text file.
pub(super) async fn export(call: &mut Call<'_>, scope: Scope) -> Result<ExportResult, RpcError> {
    let opened = open(call, &scope).await?;
    let mut conn = opened.conn.lock().await;
    let facts = mem::list(&mut conn, None, opened.database.as_deref()).await.map_err(|e| opened.failed(call, e))?;
    Ok(ExportResult { text: mem::export(&opened.project.name, &facts), facts: facts.len() })
}
