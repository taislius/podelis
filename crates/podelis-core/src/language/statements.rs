//! What a script will do before it is sent, in any language: which
//! statements read, write or change the schema, and which look dangerous.
//! SQL's are told in `sql::analyze`; another language says what its own do.

use super::Language;
use crate::sql::parse::ParsedScript;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxnOp {
    Begin,
    Commit,
    Rollback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Only reads data (SELECT, SHOW, EXPLAIN without ANALYZE...).
    Read,
    /// Modifies rows.
    Write,
    /// Changes the schema (CREATE, ALTER, DROP...). MySQL commits implicitly.
    Ddl,
    Transaction(TxnOp),
    /// Session settings and the like (SET, USE).
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementInfo {
    /// The statement, abbreviated for messages.
    pub summary: String,
    pub kind: Kind,
    /// Why this statement deserves a confirmation, e.g. "DELETE without WHERE".
    pub risk: Option<&'static str>,
}

impl StatementInfo {
    pub fn modifies(&self) -> bool {
        matches!(self.kind, Kind::Write | Kind::Ddl)
    }
}

pub fn analyze(script: &ParsedScript) -> Vec<StatementInfo> {
    let language = script.engine.backend().language();
    script
        .statements
        .iter()
        .map(|stmt| {
            let (kind, risk) = match language {
                Language::Sql(_) => crate::sql::analyze::classify(stmt, script.engine),
                Language::Other(language) => language.classify(stmt.text),
            };
            StatementInfo { summary: summarize(stmt.text), kind, risk }
        })
        .collect()
}

fn summarize(stmt: &str) -> String {
    let flat = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 60 { format!("{}…", flat.chars().take(59).collect::<String>()) } else { flat }
}
