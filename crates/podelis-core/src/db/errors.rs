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
        /// A syntax error or unknown function: maybe a feature the server
        /// is too old for ([`missing_feature`]).
        missing_feature: bool,
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

/// What a driver makes of one of its own errors, in no driver's terms:
/// each engine's `driver::error_facts` reads its driver's.
#[derive(Debug, Default)]
pub struct ErrorFacts {
    /// The connection itself broke.
    pub broken: bool,
    /// No connection could be had in time (the pool timed out, or closed).
    pub no_connection: bool,
    /// Why TLS failed, if that's what it is.
    pub tls: Option<String>,
    /// What the server said, if it's the server's error.
    pub server: Option<ServerError>,
}

/// A server's error, as its engine reads it.
#[derive(Debug, Default)]
pub struct ServerError {
    /// Its message, as the engine words it (a detail and hint added,
    /// boilerplate cut).
    pub message: String,
    pub position: Option<ErrorPos>,
    /// A syntax error or unknown function: what a feature the server's
    /// version lacks looks like.
    pub missing_feature: bool,
    /// Said just before the server closes the connection (a shutdown).
    pub closes_connection: bool,
    /// As a statement check's finding: a syntax error (`Some(true)`),
    /// another problem worth showing (`Some(false)`: an unknown table), or
    /// none to show.
    pub check: Option<bool>,
}

/// The first error in `err`'s chain a driver reads, with how many causes
/// (context added on top) come before it.
fn facts(err: &anyhow::Error) -> Option<(usize, ErrorFacts)> {
    err.chain().enumerate().find_map(|(i, cause)| {
        Engine::BUILT.into_iter().find_map(|e| on_driver!(e, engine::driver::error_facts(cause))).map(|f| (i, f))
    })
}

/// What the server said, if `err` is the server's error.
pub fn server_error(err: &anyhow::Error) -> Option<ServerError> {
    facts(err)?.1.server
}

/// Why TLS failed, if that's why connecting did.
pub fn tls_failure(err: &anyhow::Error) -> Option<String> {
    facts(err)?.1.tls
}

/// Whether the server couldn't be reached at all: the connection broke,
/// its address didn't answer, or no connection to it could be had in time
/// (the pool timed out).
pub fn is_unreachable(err: &anyhow::Error) -> bool {
    is_broken(err)
        || err.chain().any(|e| e.downcast_ref::<Unreachable>().is_some())
        || facts(err).is_some_and(|(_, f)| f.no_connection)
}

/// Whether the error means the connection itself broke.
pub fn is_broken(err: &anyhow::Error) -> bool {
    facts(err).is_some_and(|(_, f)| f.broken)
}

/// A readable message for an error from `run`/`connect`: the server's own
/// message, as its engine words it, rather than the driver's wrapping;
/// context added on top kept ("connection lost; reconnected").
pub fn error_message(err: &anyhow::Error) -> String {
    let Some((before, ErrorFacts { server: Some(server), .. })) = facts(err) else {
        return format!("{err:#}");
    };
    let context: Vec<String> = err.chain().take(before).map(ToString::to_string).collect();
    if context.is_empty() { server.message } else { format!("{}: {}", context.join(": "), server.message) }
}

/// Whether `err` is a server's syntax error or unknown function (by its
/// code): what a feature the server's version lacks looks like.
pub fn missing_feature(err: &anyhow::Error) -> bool {
    server_error(err).is_some_and(|s| s.missing_feature)
}

pub fn error_position(err: &anyhow::Error) -> Option<ErrorPos> {
    server_error(err)?.position
}
