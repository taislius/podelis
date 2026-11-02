//! How `db` talks to SQLite: through sqlx (`via_sqlx`), and how its errors
//! read.

use sqlx::sqlite::SqliteError;

use crate::db::errors::{ErrorFacts, ServerError};
use crate::db::via_sqlx;

via_sqlx::driver!();

/// What one of sqlx's SQLite errors says. Its position is only in the
/// message's text (`SqlDialect::position_in_message`).
pub(in crate::db) fn error_facts(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorFacts> {
    via_sqlx::error_facts(cause, |err| {
        err.try_downcast_ref::<SqliteError>()?;
        let message = err.message().to_string();
        let check = if ["syntax error", "incomplete input", "unrecognized token"].iter().any(|w| message.contains(w)) {
            Some(true)
        } else if message.starts_with("no such") {
            Some(false)
        } else {
            None
        };
        Some(ServerError { message, position: None, missing_feature: false, closes_connection: false, check })
    })
}
