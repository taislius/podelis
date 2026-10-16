use super::*;

/// A SQL string literal for `value`. MySQL also treats backslashes as
/// escapes, so they're doubled there.
pub fn sql_literal(value: &str, engine: Engine) -> String {
    engine.dialect().string_literal(value)
}

/// A literal for `value` from a result column of type `ty`: a binary
/// value in the engine's hex notation, anything else as a string.
pub fn value_literal(value: &str, ty: &str, engine: Engine) -> String {
    let dialect = engine.dialect();
    if dialect.is_binary_type(ty)
        && let Some(literal) = dialect.binary_literal(value)
    {
        return literal;
    }
    dialect.string_literal(value)
}

/// What the server said about a statement it prepared but didn't run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Ok,
    Problem {
        message: String,
        position: Option<ErrorPos>,
        /// A syntax error, as opposed to e.g. an unknown table (which may
        /// just not exist *yet* when a script creates it first).
        syntax: bool,
    },
    /// The server couldn't judge it (e.g. statements MySQL can't prepare).
    Unsupported,
}

/// Where in the submitted SQL the server says an error is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorPos {
    /// 0-based char index (Postgres).
    Char(usize),
    /// 1-based line (MySQL's "... at line N").
    Line(usize),
    /// The first occurrence of this token (SQLite's `near "x"`).
    Token(String),
    /// The text that follows the error, on a 1-based line (MySQL's
    /// `near '...' at line N`).
    Near { line: usize, text: String },
}

/// A readable message for an error from `run`/`connect`: the server's own
/// message (plus Postgres's detail and hint) rather than sqlx's wrapping,
/// which repeats it and appends a line number from Postgres's C source.
/// Marks an error after which the session was reopened: settings and any
/// open transaction of the old session are gone.
#[derive(Debug)]
pub struct Reconnected;

impl std::fmt::Display for Reconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("connection lost; reconnected (session settings and any open transaction were reset)")
    }
}

/// Marks an error meaning the server didn't answer (a dead link, a VPN
/// that dropped): nothing was run, and the connection looks down.
#[derive(Debug)]
pub struct NoAnswer;

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the server doesn't answer: the connection looks down (a VPN or network that dropped?)")
    }
}

/// Whether the session was reopened after this error.
pub fn is_reconnected(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Reconnected>().is_some()
}

/// The server's address couldn't be reached at all (see `db::reach`):
/// why, in a user's words.
#[derive(Debug)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// Whether the server couldn't be reached at all: the connection broke,
/// its address didn't answer, or no connection to it could be had in time
/// (the pool timed out).
pub fn is_unreachable(err: &anyhow::Error) -> bool {
    is_broken(err)
        || err.chain().any(|e| e.downcast_ref::<Unreachable>().is_some())
        || matches!(err.downcast_ref::<sqlx::Error>(), Some(sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed))
}

/// Whether the error means the connection itself broke.
pub fn is_broken(err: &anyhow::Error) -> bool {
    match err.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Io(_) | sqlx::Error::Protocol(_) | sqlx::Error::Tls(_)) => true,
        // A connection exception (SQLSTATE class 08), or what an engine
        // says first when it closes the connection (PostgreSQL's shutdowns).
        Some(sqlx::Error::Database(db)) => {
            db.code().is_some_and(|c| c.starts_with("08"))
                || Engine::ALL.iter().any(|e| e.dialect().closes_connection(db.as_ref()))
        }
        _ => false,
    }
}

pub fn error_message(err: &anyhow::Error) -> String {
    let Some(sqlx::Error::Database(db)) = err.downcast_ref::<sqlx::Error>() else {
        return format!("{err:#}");
    };
    // The engine whose error it is words it (its detail and hint, its
    // boilerplate cut); else the server's message as it came.
    let msg = Engine::ALL
        .iter()
        .find_map(|e| e.dialect().database_error_message(db.as_ref()))
        .unwrap_or_else(|| db.message().to_string());
    // Keep context added on top, e.g. "connection lost; reconnected".
    let context: Vec<String> =
        err.chain().take_while(|e| e.downcast_ref::<sqlx::Error>().is_none()).map(ToString::to_string).collect();
    if context.is_empty() { msg } else { format!("{}: {msg}", context.join(": ")) }
}

pub fn error_position(err: &anyhow::Error) -> Option<ErrorPos> {
    let Some(sqlx::Error::Database(db)) = err.downcast_ref::<sqlx::Error>() else {
        return None;
    };
    Engine::ALL.iter().find_map(|e| e.dialect().database_error_position(db.as_ref()))
}
