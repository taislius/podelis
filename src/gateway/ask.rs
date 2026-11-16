//! `context`, what a model would be shown of a database for a question,
//! and `ask`, which has a model write SQL over it (or say what SQL does).
//! The conversation itself is `podelis_ai::ask`; what's decided here is
//! what may go to which model, the key, and what's kept of the answer.

use podelis_ai::memory::Fact;
use podelis_ai::models;

use super::ai::{key_for, model_failed, pick};
use super::call::Call;
use super::methods::{AskParams, AskResult, ContextParams, ContextResult, NotShared, Note, TableWhy, Usage};
use super::protocol::{RpcError, code};
use super::source::{Source, context_for, load};
use crate::config::store::{ProjectId, ShareSchema};

/// The connection and question a call needs, or why it can't go on.
fn needs<'a>(method: &str, connection: Option<&'a str>, question: &'a str) -> Result<(&'a str, &'a str), RpcError> {
    match (connection, super::methods::given(Some(question))) {
        (Some(connection), Some(question)) => Ok((connection, question)),
        _ => Err(RpcError::new(code::INVALID_PARAMS, format!("{method} needs `connection` and `question`"))),
    }
}

/// The connection's schema, said as a database error when it can't be had.
async fn source(
    call: &mut Call<'_>,
    scope: &super::methods::Scope,
    connection: &str,
    refresh: bool,
) -> Result<Source, RpcError> {
    call.log("connection", connection);
    load(call, scope.project(), connection, scope.dir(), refresh).await.map_err(|e| RpcError::new(code::DATABASE, e))
}

/// `context`: the tables that matter for the question and the text a
/// model would be shown, from a saved connection, opened read-only. The
/// log keeps the connection and how many tables, not the question.
pub(super) async fn context(call: &mut Call<'_>, p: ContextParams) -> Result<ContextResult, RpcError> {
    let (connection, question) = needs("context", p.scope.connection(), &p.question)?;
    let source = source(call, &p.scope, connection, p.refresh).await?;
    let built = context_for(call, &source, question).await;
    call.log("tables", built.tables.len());
    Ok(ContextResult {
        engine: source.schema.engine.clone(),
        chars: built.text.len(),
        text: built.text,
        tables: tables(&built.tables),
        left_out: built.left_out,
        facts: built.facts,
        stale_facts: built.stale,
    })
}

fn tables(chosen: &[podelis_ai::context::Chosen]) -> Vec<TableWhy> {
    chosen.iter().map(|t| TableWhy { name: t.name.clone(), why: t.why.clone() }).collect()
}

/// `ask`: a model writes SQL for the question over the connection's
/// schema (the tables `context` picks); its names are checked against the
/// schema and sent back to it when they're not there; the SQL is
/// returned, never run. With `explain`, `question` is SQL and the model
/// says what it does. A model not on this machine is shown the schema
/// only if the project shares it (or the user said yes this once).
pub(super) async fn ask(call: &mut Call<'_>, p: AskParams) -> Result<AskResult, RpcError> {
    let (connection, question) = needs("ask", p.scope.connection(), &p.question)?;
    // `refresh`: the schema read again, not the one kept (the client knows
    // it changed, or can't know what the gateway kept: a client just
    // started, as the gateway outlives it).
    let source = source(call, &p.scope, connection, p.refresh).await?;
    let built = context_for(call, &source, question).await;
    if built.tables.is_empty() {
        return Err(RpcError::new(code::DATABASE, built.text.trim().to_string()));
    }
    // The model first, its key only once the schema may go to it: a
    // refused question mustn't wake up a password manager.
    let config = pick(call.config_dir, super::methods::given(p.model.as_deref()))?;
    call.log("model", &config.name);
    if source.share == ShareSchema::Local && !models::on_this_machine(&config) && !p.share_once {
        let message = format!(
            "project '{}' shares its schemas only with models on this machine, and {} ({}) isn't one: \
             its table and column names would go to {}",
            source.project,
            config.name,
            config.model,
            config.url()
        );
        let data = NotShared {
            project: source.project.clone(),
            project_id: Some(source.project_id.clone()),
            model: config.name.clone(),
            url: config.url().to_string(),
        };
        return Err(RpcError::with_data(code::NOT_SHARED, message, serde_json::to_value(data).unwrap_or_default()));
    }
    let key = key_for(call, &config).await?;
    call.log("explain", p.explain);
    let mut prompt = podelis_ai::ask::prompt(&config, &source.schema.engine, &built.text, question, p.explain);
    let (pieces, received) = tokio::sync::mpsc::unbounded_channel::<String>();
    let conversation =
        podelis_ai::ask::converse(&config, key.as_deref(), &mut prompt, &source.schema, p.explain, move |text| {
            let _ = pieces.send(text.to_string());
        });
    let answered =
        call.relay(Note::AskDelta, received, conversation).await.map_err(|e| model_failed(call, &config, e))?;
    let podelis_ai::ask::Answered { sql, said, problems, reply, attempts, input_tokens, output_tokens, looked_at } =
        answered;
    call.log("looked_up", looked_at.len());
    call.log("tables", built.tables.len());
    call.log("attempts", attempts);
    call.log("input_tokens", input_tokens);
    call.log("output_tokens", output_tokens);
    call.log("facts", built.facts.len());
    // (An explanation's words are kept whole: it suggests no facts.)
    let (said, suggested) = if p.explain { (said, Vec::new()) } else { podelis_ai::ask::suggestions(&said) };
    let suggested = keep_suggestions(call, &source.project_id, &source.database, &config.name, &suggested).await;
    call.log("suggested", suggested.len());
    Ok(AskResult {
        sql,
        said,
        suggested,
        problems,
        tables: tables(&built.tables),
        looked_at,
        facts: built.facts,
        stale_facts: built.stale,
        model: config.name,
        provider_model: reply.model,
        stop_reason: reply.stop_reason,
        attempts,
        usage: Usage { input_tokens, output_tokens },
    })
}

/// Keeps what a model suggested as unconfirmed facts (once each), so the
/// user can confirm them; returns them, with those already waiting to be
/// confirmed (the answer rests on them just the same).
async fn keep_suggestions(
    call: &Call<'_>,
    project_id: &ProjectId,
    database: &str,
    model: &str,
    facts: &[(String, String, String)],
) -> Vec<Fact> {
    if facts.is_empty() {
        return Vec::new();
    }
    let kept = async {
        let conn = call.caches.memory(call.config_dir, project_id).await?;
        let mut conn = conn.lock().await;
        let known = podelis_ai::memory::list(&mut conn, None, Some(database)).await?;
        let mut kept = Vec::new();
        for (subject, relation, value) in facts {
            let same = |f: &Fact| f.subject.eq_ignore_ascii_case(subject) && f.value.eq_ignore_ascii_case(value);
            if let Some(waiting) = known.iter().find(|f| same(f)) {
                if !waiting.confirmed {
                    kept.push(waiting.clone());
                }
                continue;
            }
            if podelis_ai::memory::rejected(&mut conn, subject, value).await? {
                continue;
            }
            let source = format!("model:{model}");
            kept.push(podelis_ai::memory::add(&mut conn, subject, relation, value, &source, false, database).await?);
        }
        Ok::<_, podelis_ai::memory::MemoryError>(kept)
    };
    kept.await.unwrap_or_else(|e| {
        call.caches.memory_failed(project_id, &e);
        tracing::warn!(error = %e, "gateway: couldn't keep a model's suggested facts");
        Vec::new()
    })
}
