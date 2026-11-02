//! How `db` talks to PostgreSQL: through sqlx (`via_sqlx`), and how its
//! errors read.

use sqlx::postgres::{PgDatabaseError, PgErrorPosition};

use crate::db::ErrorPos;
use crate::db::errors::{ErrorFacts, ServerError};
use crate::db::via_sqlx;

via_sqlx::driver!();

/// What one of sqlx's PostgreSQL errors says.
pub(in crate::db) fn error_facts(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorFacts> {
    via_sqlx::error_facts(cause, |err| {
        let pg = err.try_downcast_ref::<PgDatabaseError>()?;
        let mut message = pg.message().to_string();
        if let Some(detail) = pg.detail() {
            message = format!("{message}. {detail}");
        }
        if let Some(hint) = pg.hint() {
            message = format!("{message} (hint: {hint})");
        }
        let position = match pg.position() {
            Some(PgErrorPosition::Original(p)) => Some(ErrorPos::Char(p.saturating_sub(1))),
            _ => None,
        };
        Some(ServerError {
            message,
            position,
            // syntax_error, undefined_function
            missing_feature: matches!(pg.code(), "42601" | "42883"),
            // Admin and crash shutdowns: said just before the connection goes.
            closes_connection: matches!(pg.code(), "57P01" | "57P02" | "57P03"),
            check: match pg.code() {
                "42601" => Some(true),
                // undefined table/column/function/object, wrong argument types
                "42P01" | "42703" | "42883" | "42704" | "42P02" | "42804" => Some(false),
                _ => None,
            },
        })
    })
}
