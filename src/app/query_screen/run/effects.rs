//! Database work started by the run coordinator. Each task gets its inputs
//! and an event sender; none of them reads or mutates app state.

use crate::app::events::RunEvent;
use crate::app::*;

use super::ANSWER_WITHIN;

pub(super) async fn run_query(
    db: Database,
    sql: String,
    row_limit: usize,
    stream: bool,
    check_first: bool,
    send: impl Fn(RunEvent) + Send + 'static,
) {
    // A session that didn't answer is given up on: the run then opens a new
    // one (a read runs on it and says it reconnected; anything else isn't
    // run, and says so), or says the server can't be reached.
    if check_first && let Err(e) = db.answers_within(ANSWER_WITHIN).await {
        send(RunEvent::SessionReplaced(crate::db::error_message(&e)));
    }
    if stream {
        // No transaction probe: the open stream holds the session, and a
        // single read can't change the transaction state.
        match db.run_streaming(&sql, row_limit).await {
            Ok((outcome, rows)) => send(RunEvent::QueryDone(outcome, None, rows)),
            Err(e) => send(failed(&e, db.engine(), after_error(&db, &e).await)),
        }
        return;
    }
    match db.run(&sql, row_limit).await {
        Ok(outcome) => {
            let txn = db.transaction_state().await;
            send(RunEvent::QueryDone(outcome, txn, None));
        }
        Err(e) => send(failed(&e, db.engine(), after_error(&db, &e).await)),
    }
}

pub(super) async fn fetch_page(stream: RowStream, rows: usize, shown: bool, send: impl Fn(RunEvent) + Send + 'static) {
    match stream.next_page(rows).await {
        Ok(page) => send(RunEvent::MoreRows { page, shown }),
        Err(e) => send(RunEvent::MoreFailed(e)),
    }
}

pub(super) async fn cancel_on_server(db: Database, send: impl Fn(RunEvent) + Send + 'static) {
    if let Err(e) = db.cancel().await {
        send(RunEvent::CancelFailed {
            message: crate::db::error_message(&e),
            unreachable: crate::db::is_unreachable(&e),
        });
    }
}

pub(super) async fn check_guarded_read(
    db: Database,
    sql: String,
    offset: usize,
    infos: Vec<StatementInfo>,
    filled: Option<Filled>,
    send: impl Fn(DbEventKind) + Send + 'static,
) {
    let scans = db.full_scans(&sql).await.unwrap_or_default();
    send(DbEventKind::Guarded { sql, offset, infos, filled, scans });
}

fn failed(e: &anyhow::Error, engine: crate::db::Engine, txn: Option<TxnState>) -> RunEvent {
    let message = crate::db::error_message(e);
    let position = crate::db::error_position(e).or_else(|| engine.backend().position_in_message(&message));
    RunEvent::QueryFailed { message, position, txn, missing_feature: crate::db::missing_feature(e) }
}

async fn after_error(db: &Database, err: &anyhow::Error) -> Option<TxnState> {
    if crate::db::is_reconnected(err) { Some(TxnState::Idle) } else { db.transaction_state().await }
}
