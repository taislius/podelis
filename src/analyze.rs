//! What a script will do before it is sent: which statements read, write
//! or change the schema, and which look dangerous. Uses `sqlparser` when it
//! understands the statement and falls back to keywords when it doesn't
//! (its grammar doesn't cover every dialect feature).

use sqlparser::ast::{
    AlterTableOperation, Expr, GroupByExpr, ObjectNamePart, SelectItem, SetExpr, Statement, TableFactor,
};

use crate::db::{Engine, ErrorPos};
use crate::parse::{ParsedScript, ParsedStatement, line_col_offset};
use crate::sql::{self, TokenKind};

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
    script
        .statements
        .iter()
        .map(|stmt| {
            let words = words(stmt.text, script.engine);
            let (kind, risk) = match stmt.single() {
                Some(ast) => {
                    let (kind, risk) = classify_ast(ast, &words);
                    // Rules that look inside the statement (an EXPLAIN that
                    // doesn't run it needs no confirmation).
                    let modifies = matches!(kind, Kind::Write | Kind::Ddl);
                    (kind, risk.or_else(|| crate::lint::risk(stmt, script.engine).filter(|_| modifies)))
                }
                None => classify_words(&words),
            };
            StatementInfo { summary: summarize(stmt.text), kind, risk }
        })
        .collect()
}

/// Uppercased keywords/identifiers of a statement, skipping strings,
/// comments and quoted identifiers.
fn words(stmt: &str, engine: Engine) -> Vec<String> {
    sql::tokenize(stmt, engine)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| stmt[t.range].to_ascii_uppercase())
        .collect()
}

fn has(words: &[String], w: &str) -> bool {
    words.iter().any(|x| x == w)
}

fn modifies_data(words: &[String]) -> bool {
    ["INSERT", "UPDATE", "DELETE", "MERGE"].iter().any(|w| has(words, w))
}

// One arm per kind of statement reads better than merged patterns.
#[allow(clippy::match_same_arms)]
fn classify_ast(stmt: &Statement, words: &[String]) -> (Kind, Option<&'static str>) {
    match stmt {
        // A data-modifying CTE (`WITH x AS (DELETE ...) SELECT`) writes.
        Statement::Query(_) if modifies_data(words) => (Kind::Write, None),
        Statement::Query(_) => (Kind::Read, None),
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => (Kind::Write, None),
        Statement::Truncate(_) => (Kind::Ddl, Some("TRUNCATE")),
        Statement::Drop { .. } => (Kind::Ddl, Some("DROP")),
        Statement::AlterTable(a) if a.operations.iter().any(drops_data_or_keys) => (Kind::Ddl, Some("ALTER … DROP")),
        // DROP DEFAULT, DROP NOT NULL and the like lose nothing.
        Statement::AlterTable(_) => (Kind::Ddl, None),
        Statement::StartTransaction { .. } => (Kind::Transaction(TxnOp::Begin), None),
        Statement::Commit { .. } => (Kind::Transaction(TxnOp::Commit), None),
        Statement::Rollback { savepoint: None, .. } => (Kind::Transaction(TxnOp::Rollback), None),
        Statement::Rollback { .. } => (Kind::Session, None),
        // EXPLAIN ANALYZE really runs the statement.
        Statement::Explain { analyze: true, statement, .. } => classify_ast(statement, words),
        Statement::Explain { .. } | Statement::ExplainTable { .. } => (Kind::Read, None),
        Statement::Set(_) | Statement::Use(_) => (Kind::Session, None),
        // Everything else (SHOW..., CREATE..., GRANT...) by keyword.
        _ => classify_words(words),
    }
}

/// Drops a column or partition (their data goes) or a constraint or index
/// (what kept the data consistent or fast).
fn drops_data_or_keys(op: &AlterTableOperation) -> bool {
    matches!(
        op,
        AlterTableOperation::DropColumn { .. }
            | AlterTableOperation::DropPartitions { .. }
            | AlterTableOperation::DropConstraint { .. }
            | AlterTableOperation::DropPrimaryKey { .. }
            | AlterTableOperation::DropForeignKey { .. }
            | AlterTableOperation::DropIndex { .. }
    )
}

#[allow(clippy::match_same_arms)]
fn classify_words(words: &[String]) -> (Kind, Option<&'static str>) {
    let first = words.first().map_or("", String::as_str);
    let second = words.get(1).map_or("", String::as_str);
    match first {
        "SELECT" | "WITH" | "VALUES" | "TABLE" if modifies_data(words) => (Kind::Write, None),
        "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "DESCRIBE" | "DESC" => (Kind::Read, None),
        "EXPLAIN" if has(words, "ANALYZE") && modifies_data(words) => (Kind::Write, None),
        "EXPLAIN" => (Kind::Read, None),
        "BEGIN" | "START" => (Kind::Transaction(TxnOp::Begin), None),
        "COMMIT" | "END" => (Kind::Transaction(TxnOp::Commit), None),
        "ROLLBACK" if has(words, "TO") => (Kind::Session, None),
        "ROLLBACK" | "ABORT" => (Kind::Transaction(TxnOp::Rollback), None),
        "SAVEPOINT" | "RELEASE" => (Kind::Session, None),
        "SET" if second == "AUTOCOMMIT" || (second == "SESSION" && has(words, "AUTOCOMMIT")) => (Kind::Session, None),
        "SET" | "USE" | "RESET" | "DISCARD" | "LISTEN" | "UNLISTEN" | "PRAGMA" => (Kind::Session, None),
        "UPDATE" if !has(words, "WHERE") => (Kind::Write, Some("UPDATE without WHERE")),
        "DELETE" if !has(words, "WHERE") && !has(words, "USING") => (Kind::Write, Some("DELETE without WHERE")),
        "TRUNCATE" => (Kind::Ddl, Some("TRUNCATE")),
        "DROP" => (Kind::Ddl, Some("DROP")),
        "ALTER" if has(words, "DROP") => (Kind::Ddl, Some("ALTER … DROP")),
        "CREATE" | "ALTER" | "RENAME" | "GRANT" | "REVOKE" | "COMMENT" => (Kind::Ddl, None),
        // INSERT, UPDATE, DELETE, MERGE, REPLACE, COPY, CALL, DO, VACUUM and
        // anything unrecognised: assume it writes.
        _ => (Kind::Write, None),
    }
}

fn summarize(stmt: &str) -> String {
    let flat = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 60 { format!("{}…", flat.chars().take(59).collect::<String>()) } else { flat }
}

/// Where a result set's rows come from, when they map 1:1 onto one table's
/// rows (so cells can be edited by primary key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditSource {
    pub schema: Option<String>,
    pub table: String,
    /// Columns selected by plain name, or `None` for `*` (all columns).
    pub columns: Option<Vec<String>>,
}

/// The single table behind `script`, if it is one plain `SELECT` from one
/// table (no joins, grouping, DISTINCT, set operations or CTEs). Computed
/// columns are left out of `columns`, so they can't be edited.
pub fn edit_source(script: &ParsedScript) -> Option<EditSource> {
    let [stmt] = script.statements.as_slice() else { return None };
    let Statement::Query(query) = stmt.single()? else { return None };
    if query.with.is_some() {
        return None;
    }
    let SetExpr::Select(select) = &*query.body else { return None };
    let plain_group = matches!(&select.group_by, GroupByExpr::Expressions(e, m) if e.is_empty() && m.is_empty());
    if select.distinct.is_some() || select.having.is_some() || !plain_group || select.from.len() != 1 {
        return None;
    }
    let from = &select.from[0];
    let TableFactor::Table { name, args: None, .. } = &from.relation else { return None };
    if !from.joins.is_empty() {
        return None;
    }
    let parts: Vec<String> = name
        .0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) => Some(id.value.clone()),
            ObjectNamePart::Function(_) => None,
        })
        .collect::<Option<_>>()?;
    let (schema, table) = match parts.as_slice() {
        [table] => (None, table.clone()),
        [.., schema, table] => (Some(schema.clone()), table.clone()),
        [] => return None,
    };
    let mut columns = Some(Vec::new());
    for item in &select.projection {
        match item {
            SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..) => columns = None,
            SelectItem::UnnamedExpr(Expr::Identifier(id)) => {
                if let Some(cols) = &mut columns {
                    cols.push(id.value.clone());
                }
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(ids)) => {
                if let (Some(cols), Some(last)) = (&mut columns, ids.last()) {
                    cols.push(last.value.clone());
                }
            }
            // Expressions and aliases aren't editable columns.
            _ => {}
        }
    }
    Some(EditSource { schema, table, columns })
}

/// A possible problem in the editor's SQL: message and byte offset into
/// `script`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxHint {
    pub message: String,
    pub offset: Option<usize>,
    /// Length in bytes of the problem at `offset`, if known.
    pub len: Option<usize>,
    pub source: HintSource,
    /// Which lint warning this is and how many there are (1-based).
    pub nth: Option<(usize, usize)>,
}

/// Who found the problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintSource {
    /// The database server (exact).
    Server,
    /// `sqlparser`, which can be wrong about dialect SQL.
    Parser,
    /// A lint rule (by name): valid SQL that probably isn't what was meant.
    Lint(&'static str),
}

/// The first statement `sqlparser` can't parse, as a gentle hint. It may be
/// wrong for valid dialect-specific SQL, so callers must not block on it.
pub fn syntax_hint(script: &ParsedScript) -> Option<SyntaxHint> {
    let (message, offset) = script.statements.iter().find_map(ParsedStatement::error)?;
    Some(SyntaxHint { message, offset, len: None, source: HintSource::Parser, nth: None })
}

/// The byte range in `sent` (the SQL given to the server) that a reported
/// error position refers to: the word at a char position, the whole line
/// for a line number, or the first whole-word match of a token.
pub fn error_range(sent: &str, position: &ErrorPos) -> Option<(usize, usize)> {
    let word_len = |rest: &str| {
        let word = rest.find(|c: char| !crate::editor::is_word_char(c)).unwrap_or(rest.len());
        if word > 0 { word } else { rest.chars().next().map_or(0, char::len_utf8) }
    };
    match position {
        ErrorPos::Char(c) => {
            let start = sent.char_indices().nth(*c).map(|(b, _)| b)?;
            Some((start, word_len(&sent[start..])))
        }
        ErrorPos::Line(n) => {
            let start = line_col_offset(sent, *n, 1)?;
            Some((start, sent[start..].find('\n').unwrap_or(sent.len() - start)))
        }
        ErrorPos::Token(token) if !token.is_empty() => {
            let lower = sent.to_lowercase();
            let needle = token.to_lowercase();
            let boundary = |i: usize| {
                !lower[..i].chars().next_back().is_some_and(crate::editor::is_word_char)
                    && !lower[i + needle.len()..].chars().next().is_some_and(crate::editor::is_word_char)
            };
            // Lowercasing can change byte lengths for non-ASCII text; then
            // only offsets that exist in both are safe.
            let start = lower
                .match_indices(&needle)
                .map(|(i, _)| i)
                .find(|&i| boundary(i))
                .or_else(|| lower.find(&needle))
                .filter(|&i| sent.len() == lower.len() && sent.is_char_boundary(i))?;
            Some((start, needle.len()))
        }
        ErrorPos::Token(_) => None,
        ErrorPos::Near { line, text } => {
            let line_start = line_col_offset(sent, *line, 1)?;
            // MySQL truncates the quoted text; its start is enough.
            let probe: String = text.chars().take(20).collect();
            match sent[line_start..].find(&probe) {
                Some(i) => {
                    let start = line_start + i;
                    Some((start, word_len(&sent[start..])))
                }
                None => error_range(sent, &ErrorPos::Line(*line)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    const PG: Engine = Engine::Postgres;
    const MY: Engine = Engine::MySql;

    fn kinds(sql: &str, engine: Engine) -> Vec<(Kind, Option<&'static str>)> {
        analyze(&parse(sql, engine)).into_iter().map(|i| (i.kind, i.risk)).collect()
    }

    #[test]
    fn reads_and_writes() {
        assert_eq!(kinds("select 1; show tables; explain select 1", MY), vec![(Kind::Read, None); 3]);
        assert_eq!(kinds("insert into t values (1)", PG), [(Kind::Write, None)]);
        assert_eq!(kinds("update t set a = 1 where id = 2", PG), [(Kind::Write, None)]);
        assert_eq!(kinds("create table t (id int)", PG), [(Kind::Ddl, None)]);
        assert_eq!(kinds("set search_path to x; use db", MY)[0].0, Kind::Session);
    }

    #[test]
    fn risky_statements() {
        assert_eq!(kinds("update t set a = 1", PG), [(Kind::Write, Some("UPDATE without WHERE"))]);
        assert_eq!(kinds("DELETE FROM t", MY), [(Kind::Write, Some("DELETE without WHERE"))]);
        assert_eq!(kinds("delete from t using u where t.id = u.id", PG), [(Kind::Write, None)]);
        assert_eq!(kinds("drop table t", PG), [(Kind::Ddl, Some("DROP"))]);
        assert_eq!(kinds("truncate t", PG), [(Kind::Ddl, Some("TRUNCATE"))]);
        assert_eq!(kinds("alter table t drop column c", PG), [(Kind::Ddl, Some("ALTER … DROP"))]);
        assert_eq!(kinds("alter table t drop constraint t_pk", PG), [(Kind::Ddl, Some("ALTER … DROP"))]);
        assert_eq!(kinds("alter table t drop index ix", MY), [(Kind::Ddl, Some("ALTER … DROP"))]);
        // Nothing is lost.
        assert_eq!(kinds("alter table t alter column c drop default", PG), [(Kind::Ddl, None)]);
        assert_eq!(kinds("alter table t alter column c drop not null", PG), [(Kind::Ddl, None)]);
        assert_eq!(kinds("alter table t add column drop_date date", PG), [(Kind::Ddl, None)]);
        // A WHERE inside a string doesn't count.
        assert_eq!(kinds("update t set note = 'where'", PG)[0].1, Some("UPDATE without WHERE"));
        // Only when it runs: EXPLAIN alone doesn't, EXPLAIN ANALYZE does.
        assert_eq!(kinds("explain delete from t", PG), [(Kind::Read, None)]);
        assert_eq!(kinds("explain analyze delete from t", PG), [(Kind::Write, Some("DELETE without WHERE"))]);
        // Found inside the statement too.
        let cte = "with d as (delete from t returning *) select * from d";
        assert_eq!(kinds(cte, PG), [(Kind::Write, Some("DELETE without WHERE"))]);
    }

    #[test]
    fn data_modifying_cte_and_explain_analyze_write() {
        assert_eq!(kinds("with d as (delete from t returning *) select * from d", PG)[0].0, Kind::Write);
        assert_eq!(kinds("explain analyze delete from t where id = 1", PG)[0].0, Kind::Write);
        assert_eq!(kinds("explain delete from t where id = 1", PG)[0].0, Kind::Read);
    }

    #[test]
    fn transactions() {
        let k: Vec<_> = kinds("begin; commit; start transaction; rollback; rollback to savepoint a", PG)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            k,
            [
                Kind::Transaction(TxnOp::Begin),
                Kind::Transaction(TxnOp::Commit),
                Kind::Transaction(TxnOp::Begin),
                Kind::Transaction(TxnOp::Rollback),
                Kind::Session,
            ]
        );
    }

    #[test]
    fn falls_back_to_keywords_when_unparseable() {
        // Not valid for sqlparser, but clearly a DELETE without WHERE.
        assert_eq!(kinds("delete from t ;;; ", PG), [(Kind::Write, Some("DELETE without WHERE"))]);
        assert_eq!(kinds("vacuum full t", PG)[0].0, Kind::Write);
    }

    #[test]
    fn syntax_hint_points_at_problem() {
        assert_eq!(syntax_hint(&parse("select 1; select 2", PG)), None);
        let script = "select 1;\nselect * fro t";
        let hint = syntax_hint(&parse(script, PG)).unwrap();
        assert!(hint.message.contains("Expected"), "{}", hint.message);
        let offset = hint.offset.unwrap();
        assert!(offset >= script.find("select *").unwrap(), "{offset}");
    }

    #[test]
    fn error_ranges() {
        let sql = "select a,\n  fro m from t";
        assert_eq!(error_range(sql, &ErrorPos::Char(12)), Some((12, 3)));
        assert_eq!(error_range(sql, &ErrorPos::Line(2)), Some((10, 14)));
        // Whole-word match, case-insensitive: not the "fro" inside "from".
        assert_eq!(error_range("from x fro y", &ErrorPos::Token("FRO".into())), Some((7, 3)));
        assert_eq!(error_range("abc", &ErrorPos::Token("zzz".into())), None);
        assert_eq!(error_range("abc", &ErrorPos::Char(99)), None);
        // MySQL: the quoted text after the error, located on its line.
        let sql = "select host from user\nwher host = 'x'";
        let near = ErrorPos::Near { line: 2, text: "host = 'x'".into() };
        assert_eq!(error_range(sql, &near), Some((27, 4)));
        let missing = ErrorPos::Near { line: 2, text: "nowhere".into() };
        assert_eq!(error_range(sql, &missing), Some((22, 15)));
    }

    #[test]
    fn edit_sources() {
        let src = edit_source(&parse(
            "select id, u.email, upper(note) as n from public.users u where id > 1 order by id limit 5",
            PG,
        ))
        .unwrap();
        assert_eq!(
            src,
            EditSource {
                schema: Some("public".into()),
                table: "users".into(),
                columns: Some(vec!["id".into(), "email".into()])
            }
        );
        assert_eq!(edit_source(&parse("SELECT * FROM `orders`", MY)).unwrap().columns, None);
        for not_editable in [
            "select * from a join b on true",
            "select count(*) from a group by x",
            "select distinct x from a",
            "with c as (select 1) select * from c",
            "select 1 union select 2",
            "select 1",
            "update a set x = 1",
            "select * from a; select * from b",
        ] {
            assert_eq!(edit_source(&parse(not_editable, PG)), None, "{not_editable}");
        }
    }

    #[test]
    fn summary_is_short_and_flat() {
        let info = &analyze(&parse(&format!("select\n  {}", "x, ".repeat(40)), PG))[0];
        assert!(info.summary.chars().count() <= 60 && !info.summary.contains('\n'));
    }
}
