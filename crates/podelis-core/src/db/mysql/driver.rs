//! How `db` talks to MySQL: through sqlx (`via_sqlx`), and how its errors
//! read.

use sqlx::mysql::MySqlDatabaseError;

use crate::db::ErrorPos;
use crate::db::errors::{ErrorFacts, ServerError};
use crate::db::via_sqlx;

via_sqlx::driver!();

/// What one of sqlx's MySQL errors says.
pub(in crate::db) fn error_facts(cause: &(dyn std::error::Error + 'static)) -> Option<ErrorFacts> {
    via_sqlx::error_facts(cause, |err| {
        let my = err.try_downcast_ref::<MySqlDatabaseError>()?;
        let msg = my.message();
        Some(ServerError {
            // Its syntax errors start with a long boilerplate sentence.
            message: match msg.find("for the right syntax to use ") {
                Some(i) => format!("syntax error {}", &msg[i + "for the right syntax to use ".len()..]),
                None => msg.to_string(),
            },
            position: position(msg),
            // a syntax error, FUNCTION … does not exist
            missing_feature: matches!(my.number(), 1064 | 1305),
            closes_connection: false,
            check: match my.number() {
                1064 | 1149 => Some(true),
                // unknown table/column/function, no database selected
                1146 | 1054 | 1305 | 1046 | 1109 => Some(false),
                // 1295: statement can't be prepared; anything else: not a hint
                _ => None,
            },
        })
    })
}

/// "… at line N", and "near 'rest of the statement' at line N": the
/// quoted text is what follows the error, which pins down the column.
fn position(msg: &str) -> Option<ErrorPos> {
    let at = msg.rfind(" at line ")?;
    let digits: String = msg[at + " at line ".len()..].chars().take_while(char::is_ascii_digit).collect();
    let line = digits.parse().ok()?;
    match msg[..at].find("near '") {
        Some(i) if msg[..at].ends_with('\'') => {
            let near = &msg[i + "near '".len()..at - 1];
            Some(if near.is_empty() { ErrorPos::Line(line) } else { ErrorPos::Near { line, text: near.to_string() } })
        }
        _ => Some(ErrorPos::Line(line)),
    }
}
