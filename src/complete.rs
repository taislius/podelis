//! Context-aware completion from the tokens around the cursor plus the
//! database catalog. Deliberately parser-free: the SQL being typed is
//! incomplete, which is exactly where parsers give up.

use std::collections::HashMap;
use std::ops::Range;

use crate::db::{ColumnInfo, Engine};
use crate::sql::{self, KEYWORDS, TokenKind};

/// A table as completion sees it.
pub struct TableRef<'a> {
    pub schema: &'a str,
    pub name: &'a str,
    pub columns: &'a [ColumnInfo],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandKind {
    Column,
    Table,
    Schema,
    Keyword,
    /// A saved query from the `snippets` group: inserts its SQL.
    Snippet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// What the list shows.
    pub label: String,
    /// What gets inserted.
    pub insert: String,
    pub kind: CandKind,
    /// Extra info (column type, table of a column).
    pub detail: String,
}

/// Completions for the word before byte offset `cursor` in `text`, and the
/// length in chars of the prefix they replace.
pub fn complete(text: &str, cursor: usize, engine: Engine, tables: &[TableRef]) -> (usize, Vec<Candidate>) {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$')
        .last()
        .map_or(cursor, |(i, _)| i);
    let prefix = &text[start..cursor];
    let prefix_len = prefix.chars().count();

    // The statement around the cursor, so FROM clauses of other statements
    // don't leak in.
    let (stmt_start, stmt_end) = sql::statement_bounds(text, cursor, engine);
    let statement = &text[stmt_start..stmt_end];
    let toks = sig_tokens(statement, engine);

    // CTEs and `(SELECT …) alias` act as tables too; they come first so a
    // CTE shadows a real table of the same name.
    let derived = derived_tables(&toks, tables);
    let with_derived: Vec<TableRef> = derived
        .iter()
        .map(|d| TableRef { schema: "", name: &d.name, columns: &d.columns })
        .chain(tables.iter().map(|t| TableRef { schema: t.schema, name: t.name, columns: t.columns }))
        .collect();
    let refs = scoped_tables(&toks, cursor - stmt_start, &with_derived);
    let ctes: Vec<&TableRef> = derived.iter().zip(&with_derived).filter(|(d, _)| d.cte).map(|(_, t)| t).collect();
    let tables = &with_derived[derived.len()..];

    let mut out = Vec::new();
    if let Some(qualifier) = qualifier_before(text, start) {
        // `alias.` or `table.` → its columns; `schema.` → its tables.
        let q = qualifier.to_lowercase();
        if let Some(table) = refs.aliases.get(&q).copied().or_else(|| find_table(&with_derived, &q)) {
            out.extend(column_candidates(engine, table, prefix, false));
        } else {
            out.extend(
                tables
                    .iter()
                    .filter(|t| t.schema.eq_ignore_ascii_case(&qualifier))
                    .filter(|t| matches_prefix(t.name, prefix))
                    .map(|t| table_candidate(engine, t, false)),
            );
        }
        return (prefix_len, rank(out, prefix));
    }

    let wants_table = expects_table(&text[stmt_start..start], engine);
    let show_schema = engine.capabilities().schemas;
    let table_cands = || {
        let ctes = ctes.iter().copied().filter(|t| matches_prefix(t.name, prefix)).map(|t| Candidate {
            label: t.name.to_string(),
            insert: engine.quote_ident(t.name),
            kind: CandKind::Table,
            detail: "with".into(),
        });
        let real =
            tables.iter().filter(|t| matches_prefix(t.name, prefix)).map(|t| table_candidate(engine, t, show_schema));
        ctes.chain(real)
    };
    if wants_table {
        out.extend(table_cands());
        if show_schema {
            let mut schemas: Vec<&str> = tables.iter().map(|t| t.schema).collect();
            schemas.sort_unstable();
            schemas.dedup();
            out.extend(schemas.into_iter().filter(|s| matches_prefix(s, prefix)).map(|s| Candidate {
                label: format!("{s}."),
                insert: format!("{}.", engine.quote_ident(s)),
                kind: CandKind::Schema,
                detail: "schema".into(),
            }));
        }
    } else {
        if refs.tables.is_empty() {
            // No FROM yet (`select em|`): columns of every table.
            if !prefix.is_empty() {
                // A column name many tables share is offered once (from the
                // first): with a big catalog, building each copy only to
                // drop it later made typing lag.
                let mut seen = std::collections::HashSet::new();
                for table in tables {
                    let new = table.columns.iter().filter(|c| matches_prefix(&c.name, prefix) && seen.insert(&c.name));
                    out.extend(new.map(|c| column_candidate(engine, table, c, true)));
                }
            }
        } else {
            let qualify = refs.tables.len() > 1;
            let listed = insert_columns_listed(&toks, cursor - stmt_start);
            for table in &refs.tables {
                out.extend(
                    column_candidates(engine, table, prefix, qualify)
                        .filter(|c| !listed.iter().any(|l| l.eq_ignore_ascii_case(&c.label))),
                );
            }
        }
        if !prefix.is_empty() {
            out.extend(table_cands());
        }
    }
    if !prefix.is_empty() {
        let lower = prefix.chars().all(|c| !c.is_ascii_uppercase());
        out.extend(KEYWORDS.iter().filter(|k| matches_prefix(k, prefix)).map(|k| {
            let word = if lower { k.to_ascii_lowercase() } else { k.to_string() };
            Candidate { label: word.clone(), insert: word, kind: CandKind::Keyword, detail: String::new() }
        }));
    }
    // Ignore an exact match of what's already typed (nothing to complete).
    out.retain(|c| c.insert != prefix);
    (prefix_len, rank(out, prefix))
}

fn matches_prefix(candidate: &str, prefix: &str) -> bool {
    candidate.len() >= prefix.len()
        && candidate.is_char_boundary(prefix.len())
        && candidate[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Orders by kind (columns, tables, schemas, keywords), then exact-case
/// prefix matches, then name; drops duplicates.
fn rank(mut cands: Vec<Candidate>, prefix: &str) -> Vec<Candidate> {
    // Each key once (lowercasing allocates), not on every comparison.
    cands.sort_by_cached_key(|c| (c.kind, !c.label.starts_with(prefix), c.label.to_lowercase()));
    cands.dedup_by(|a, b| a.insert == b.insert);
    cands.truncate(50);
    cands
}

fn table_candidate(engine: Engine, t: &TableRef, show_schema: bool) -> Candidate {
    let qualified = show_schema && t.schema != "public";
    let insert = if qualified {
        format!("{}.{}", engine.quote_ident(t.schema), engine.quote_ident(t.name))
    } else {
        engine.quote_ident(t.name)
    };
    let label = if qualified { format!("{}.{}", t.schema, t.name) } else { t.name.to_string() };
    Candidate { label, insert, kind: CandKind::Table, detail: "table".into() }
}

fn column_candidates<'a>(
    engine: Engine,
    table: &'a TableRef<'a>,
    prefix: &'a str,
    show_table: bool,
) -> impl Iterator<Item = Candidate> + 'a {
    table
        .columns
        .iter()
        .filter(move |c| matches_prefix(&c.name, prefix))
        .map(move |c| column_candidate(engine, table, c, show_table))
}

fn column_candidate(engine: Engine, table: &TableRef, c: &ColumnInfo, show_table: bool) -> Candidate {
    Candidate {
        label: c.name.clone(),
        insert: engine.quote_ident(&c.name),
        kind: CandKind::Column,
        detail: if show_table { format!("{} · {}", c.data_type, table.name) } else { c.data_type.clone() },
    }
}

fn find_table<'a>(tables: &'a [TableRef<'a>], name: &str) -> Option<&'a TableRef<'a>> {
    tables.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

/// The identifier before a `.` that directly precedes `start`, unquoted.
fn qualifier_before(text: &str, start: usize) -> Option<String> {
    let before = text[..start].strip_suffix('.')?;
    let last = before.chars().last()?;
    if matches!(last, '"' | '`') {
        let inner = &before[..before.len() - 1];
        let open = inner.rfind(last)?;
        return Some(inner[open + 1..].to_string());
    }
    let ident: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!ident.is_empty()).then_some(ident)
}

/// Whether the cursor is where a table name goes: right after FROM, JOIN,
/// INTO, UPDATE, TABLE or DESCRIBE, or after a comma in a FROM list.
fn expects_table(before: &str, engine: Engine) -> bool {
    let significant: Vec<String> = sql::tokenize(before, engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|t| before[t.range].to_ascii_uppercase())
        .collect();
    const TABLE_WORDS: [&str; 6] = ["FROM", "JOIN", "INTO", "UPDATE", "TABLE", "DESCRIBE"];
    match significant.last().map(String::as_str) {
        Some(w) if TABLE_WORDS.contains(&w) => true,
        Some(",") => {
            // In a FROM list if the last clause keyword is FROM.
            significant
                .iter()
                .rev()
                .find(|w| ["FROM", "SELECT", "WHERE", "SET", "BY", "ON", "VALUES", "JOIN"].contains(&w.as_str()))
                .is_some_and(|w| w == "FROM")
        }
        _ => false,
    }
}

struct Referenced<'a> {
    /// Tables named in the statement, in order.
    tables: Vec<&'a TableRef<'a>>,
    /// Lowercased alias (or table name) → table.
    aliases: HashMap<String, &'a TableRef<'a>>,
}

/// A significant token (no whitespace or comments), quotes stripped.
#[derive(Clone)]
struct Tok {
    kind: TokenKind,
    text: String,
    range: Range<usize>,
}

impl Tok {
    fn is(&self, word: &str) -> bool {
        self.kind != TokenKind::QuotedIdent && self.text.eq_ignore_ascii_case(word)
    }

    fn is_ident(&self) -> bool {
        self.kind == TokenKind::QuotedIdent || (self.kind == TokenKind::Word && !is_keyword(&self.text))
    }
}

fn sig_tokens(statement: &str, engine: Engine) -> Vec<Tok> {
    sql::tokenize(statement, engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .map(|t| {
            let raw = &statement[t.range.clone()];
            // Strip the quotes; an unterminated identifier (`"users`) has
            // only the opening one.
            let text = if t.kind == TokenKind::QuotedIdent {
                let inner = &raw[1..];
                inner.strip_suffix(&raw[..1]).unwrap_or(inner).to_string()
            } else {
                raw.to_string()
            };
            Tok { kind: t.kind, text, range: t.range }
        })
        .collect()
}

/// Index of the `)` matching the `(` at `open`, if it's closed.
fn matching_paren(toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Tables after FROM/JOIN/UPDATE/INTO in `toks`, with their aliases.
/// A parenthesised subquery after FROM/JOIN counts as its alias (found in
/// `tables` as a derived table); what's inside it is skipped.
fn referenced_tables<'a>(toks: &[Tok], tables: &'a [TableRef<'a>]) -> Referenced<'a> {
    let is_ident = |i: usize| toks.get(i).is_some_and(Tok::is_ident);
    let mut refs = Referenced { tables: Vec::new(), aliases: HashMap::new() };
    let add_alias = |refs: &mut Referenced<'a>, k: usize, table: &'a TableRef<'a>| {
        let k = if toks.get(k).is_some_and(|t| t.is("AS")) { k + 1 } else { k };
        if is_ident(k) {
            refs.aliases.insert(toks[k].text.to_lowercase(), table);
        }
    };
    let mut i = 0;
    while i < toks.len() {
        let opens_list = ["FROM", "JOIN", "UPDATE", "INTO"].iter().any(|w| toks[i].is(w));
        let continues_list = toks[i].is(",") && refs_in_from(&toks[..i]);
        if (opens_list || continues_list) && toks.get(i + 1).is_some_and(|t| t.is("(")) {
            let Some(close) = matching_paren(toks, i + 1) else { break };
            let k = if toks.get(close + 1).is_some_and(|t| t.is("AS")) { close + 2 } else { close + 1 };
            if is_ident(k)
                && let Some(table) = find_table(tables, &toks[k].text)
            {
                refs.tables.push(table);
                refs.aliases.insert(toks[k].text.to_lowercase(), table);
            }
            i = close + 1;
            continue;
        }
        if (opens_list || continues_list) && is_ident(i + 1) {
            let mut j = i + 1;
            let mut name = toks[j].text.clone();
            if toks.get(j + 1).is_some_and(|t| t.is(".")) && is_ident(j + 2) {
                toks[j + 2].text.clone_into(&mut name);
                j += 2;
            }
            if let Some(table) = find_table(tables, &name) {
                refs.tables.push(table);
                refs.aliases.insert(name.to_lowercase(), table);
                add_alias(&mut refs, j + 1, table);
            }
            i = j;
        }
        i += 1;
    }
    refs
}

/// The tables in scope at `cursor`: those of the innermost subquery around
/// it first, then the enclosing query's (for correlated references).
fn scoped_tables<'a>(toks: &[Tok], cursor: usize, tables: &'a [TableRef<'a>]) -> Referenced<'a> {
    let mut open: Vec<usize> = Vec::new();
    for (i, t) in toks.iter().enumerate().take_while(|(_, t)| t.range.start < cursor) {
        if t.is("(") {
            open.push(i);
        } else if t.is(")") {
            open.pop();
        }
    }
    let inner = open.iter().rev().find(|&&o| toks.get(o + 1).is_some_and(|t| t.is("SELECT") || t.is("WITH")));
    let Some(&o) = inner else { return referenced_tables(toks, tables) };
    let close = matching_paren(toks, o).unwrap_or(toks.len());
    let mut refs = referenced_tables(&toks[o + 1..close], tables);
    // The enclosing query, without this subquery.
    let outer: Vec<Tok> = toks[..o].iter().chain(toks.get(close + 1..).unwrap_or_default()).cloned().collect();
    let more = referenced_tables(&outer, tables);
    for table in more.tables {
        if !refs.tables.iter().any(|t| std::ptr::eq(*t, table)) {
            refs.tables.push(table);
        }
    }
    for (alias, table) in more.aliases {
        refs.aliases.entry(alias).or_insert(table);
    }
    refs
}

/// A CTE or aliased subquery, usable like a table.
struct Derived {
    name: String,
    columns: Vec<ColumnInfo>,
    /// A CTE (offered after FROM), not a subquery alias.
    cte: bool,
}

/// CTEs (`WITH name [(cols)] AS (…)`) and subqueries in FROM/JOIN with an
/// alias, with the columns they produce.
fn derived_tables(toks: &[Tok], tables: &[TableRef]) -> Vec<Derived> {
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.is("WITH") {
            let mut j = if toks.get(i + 1).is_some_and(|t| t.is("RECURSIVE")) { i + 2 } else { i + 1 };
            while let Some(name) = toks.get(j).filter(|t| t.is_ident()) {
                let mut k = j + 1;
                let mut listed = None;
                if toks.get(k).is_some_and(|t| t.is("(")) {
                    let close = matching_paren(toks, k).unwrap_or(toks.len());
                    listed = Some(
                        toks[k + 1..close].iter().filter(|t| t.is_ident()).map(|t| t.text.clone()).collect::<Vec<_>>(),
                    );
                    k = close + 1;
                }
                if !toks.get(k).is_some_and(|t| t.is("AS")) {
                    break;
                }
                k += 1;
                while toks.get(k).is_some_and(|t| t.is("NOT") || t.is("MATERIALIZED")) {
                    k += 1;
                }
                if !toks.get(k).is_some_and(|t| t.is("(")) {
                    break;
                }
                let close = matching_paren(toks, k);
                let body = &toks[k + 1..close.unwrap_or(toks.len())];
                let columns = match listed {
                    Some(names) => names.into_iter().map(|n| derived_column(n, "")).collect(),
                    None => projected_columns(body, tables),
                };
                out.push(Derived { name: name.text.clone(), columns, cte: true });
                match close {
                    Some(c) if toks.get(c + 1).is_some_and(|t| t.is(",")) => j = c + 2,
                    _ => break,
                }
            }
        } else if (t.is("FROM") || t.is("JOIN") || (t.is(",") && refs_in_from(&toks[..i])))
            && toks.get(i + 1).is_some_and(|t| t.is("("))
            && toks.get(i + 2).is_some_and(|t| t.is("SELECT") || t.is("WITH"))
            && let Some(close) = matching_paren(toks, i + 1)
        {
            let k = if toks.get(close + 1).is_some_and(|t| t.is("AS")) { close + 2 } else { close + 1 };
            if let Some(alias) = toks.get(k).filter(|t| t.is_ident()) {
                let columns = projected_columns(&toks[i + 2..close], tables);
                out.push(Derived { name: alias.text.clone(), columns, cte: false });
            }
        }
    }
    out
}

fn derived_column(name: String, data_type: &str) -> ColumnInfo {
    ColumnInfo { name, data_type: data_type.to_string(), nullable: true, primary_key: false }
}

/// The output columns of a `SELECT …` body: aliases, plain column names,
/// and `*` / `t.*` expanded from the catalog. Unnamed expressions are left
/// out.
fn projected_columns(body: &[Tok], tables: &[TableRef]) -> Vec<ColumnInfo> {
    let Some(select) = body.iter().position(|t| t.is("SELECT")) else { return Vec::new() };
    // FROM of this level only (a nested subquery's FROM is inside parens).
    let refs = referenced_tables(body, tables);
    let type_of = |name: &str| {
        refs.tables
            .iter()
            .flat_map(|t| t.columns)
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .map_or(String::new(), |c| c.data_type.clone())
    };
    let mut i = select + 1;
    while body.get(i).is_some_and(|t| t.is("DISTINCT") || t.is("ALL")) {
        i += 1;
    }
    if body.get(i).is_some_and(|t| t.is("ON")) && body.get(i + 1).is_some_and(|t| t.is("(")) {
        i = matching_paren(body, i + 1).map_or(body.len(), |c| c + 1);
    }
    // Items between top-level commas, up to the end of the select list.
    let mut items: Vec<Vec<&Tok>> = vec![Vec::new()];
    let mut depth = 0usize;
    for t in &body[i..] {
        if depth == 0 && ["FROM", "WHERE", "GROUP", "ORDER", "LIMIT", "HAVING", "UNION", "INTO"].iter().any(|w| t.is(w))
        {
            break;
        }
        if t.is("(") {
            depth += 1;
        } else if t.is(")") {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 && t.is(",") {
            items.push(Vec::new());
        } else if let Some(item) = items.last_mut() {
            item.push(t);
        }
    }
    let mut out = Vec::new();
    for item in items {
        let n = item.len();
        let Some(last) = item.last() else { continue };
        if last.is("*") {
            if n == 1 {
                out.extend(refs.tables.iter().flat_map(|t| t.columns.iter().cloned()));
            } else if n == 3
                && item[1].is(".")
                && let Some(t) = refs.aliases.get(&item[0].text.to_lowercase())
            {
                out.extend(t.columns.iter().cloned());
            }
            continue;
        }
        if !last.is_ident() {
            continue;
        }
        let name = last.text.clone();
        if n == 1 || (n == 3 && item[1].is(".")) {
            let ty = type_of(&name);
            out.push(derived_column(name, &ty));
        } else if n >= 2 && !item[n - 2].is(".") {
            // `expr AS name` or `expr name`.
            out.push(derived_column(name, ""));
        }
    }
    out
}

/// In `INSERT INTO t (a, b, |`: the columns already listed.
fn insert_columns_listed(toks: &[Tok], cursor: usize) -> Vec<String> {
    let before: Vec<&Tok> = toks.iter().take_while(|t| t.range.end <= cursor).collect();
    let Some(open) = before.iter().rposition(|t| t.is("(")) else { return Vec::new() };
    if before[open + 1..].iter().any(|t| t.is(")")) {
        return Vec::new();
    }
    // `INTO name (` or `INTO schema.name (`.
    let into = match open {
        o if o >= 2 && before[o - 2].is("INTO") => true,
        o if o >= 4 && before[o - 2].is(".") && before[o - 4].is("INTO") => true,
        _ => false,
    };
    if !into {
        return Vec::new();
    }
    before[open + 1..].iter().filter(|t| t.is_ident()).map(|t| t.text.clone()).collect()
}

/// Whether the nearest clause keyword before a comma is FROM.
fn refs_in_from(toks: &[Tok]) -> bool {
    toks.iter()
        .rev()
        .map(|t| t.text.to_ascii_uppercase())
        .find(|w| {
            ["FROM", "SELECT", "WHERE", "SET", "BY", "ON", "VALUES", "JOIN", "ORDER", "GROUP"].contains(&w.as_str())
        })
        .is_some_and(|w| w == "FROM")
}

fn is_keyword(word: &str) -> bool {
    KEYWORDS.binary_search(&word.to_ascii_uppercase().as_str()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: ty.into(), nullable: true, primary_key: false }
    }

    struct Fixture {
        users: Vec<ColumnInfo>,
        orders: Vec<ColumnInfo>,
        audit: Vec<ColumnInfo>,
    }

    fn fixture() -> Fixture {
        Fixture {
            users: vec![col("id", "integer"), col("email", "text"), col("UserName", "text")],
            orders: vec![col("id", "integer"), col("user_id", "integer"), col("total", "numeric")],
            audit: vec![col("event", "text")],
        }
    }

    fn tables(f: &Fixture) -> Vec<TableRef<'_>> {
        vec![
            TableRef { schema: "public", name: "users", columns: &f.users },
            TableRef { schema: "public", name: "orders", columns: &f.orders },
            TableRef { schema: "logs", name: "audit", columns: &f.audit },
        ]
    }

    /// Completes at the `|` marker.
    fn labels(text: &str) -> (usize, Vec<String>) {
        let f = fixture();
        let t = tables(&f);
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        let (len, cands) = complete(&text, cursor, Engine::Postgres, &t);
        (len, cands.into_iter().map(|c| c.label).collect())
    }

    #[test]
    fn tables_after_from_and_join() {
        let (len, l) = labels("select * from us|");
        assert_eq!((len, l[0].as_str()), (2, "users"));
        let (_, l) = labels("select * from users u join |");
        assert!(l.contains(&"orders".to_string()) && l.contains(&"logs.audit".to_string()));
        assert!(l.contains(&"logs.".to_string()));
        let (_, l) = labels("select * from users, o|");
        assert_eq!(l[0], "orders");
    }

    #[test]
    fn columns_of_referenced_tables() {
        let (_, l) = labels("select e| from users");
        assert_eq!(l[0], "email");
        // Columns from both joined tables come first; keywords after.
        let (_, l) = labels("select t| from users join orders on true");
        assert_eq!(l[0], "total");
        assert!(l.contains(&"then".to_string()), "keywords follow columns: {l:?}");
    }

    #[test]
    fn qualified_by_alias_table_or_schema() {
        let (len, l) = labels("select o.| from orders o");
        assert_eq!((len, l), (0, vec!["id".to_string(), "total".into(), "user_id".into()]));
        let (_, l) = labels("select users.em| from users");
        assert_eq!(l, ["email"]);
        let (_, l) = labels("select * from logs.|");
        assert_eq!(l, ["audit"]);
        let (_, l) = labels("select * from orders AS x where x.u|");
        assert_eq!(l, ["user_id"]);
    }

    #[test]
    fn mixed_case_names_are_quoted_on_insert() {
        let f = fixture();
        let t = tables(&f);
        let (_, c) = complete("select userna from users", 13, Engine::Postgres, &t);
        assert_eq!(c[0].insert, "\"UserName\"");
    }

    #[test]
    fn all_columns_before_from_is_written() {
        let f = fixture();
        let t = tables(&f);
        let (_, c) = complete("select tot", 10, Engine::Postgres, &t);
        assert_eq!((c[0].label.as_str(), c[0].detail.as_str()), ("total", "numeric · orders"));
    }

    #[test]
    fn keywords_follow_the_prefix_case() {
        let (_, l) = labels("sel|");
        assert_eq!(l[0], "select");
        let (_, l) = labels("SEL|");
        assert_eq!(l[0], "SELECT");
    }

    #[test]
    fn other_statements_dont_leak_tables() {
        // `orders` isn't referenced by the second statement, so `total`
        // shows up only as an any-table column, labelled with its table.
        let f = fixture();
        let t = tables(&f);
        let text = "select * from orders; select tot";
        let (_, c) = complete(text, text.len(), Engine::Postgres, &t);
        assert_eq!(c[0].detail, "numeric · orders");
        let (_, c) = complete("select tot from orders", 10, Engine::Postgres, &t);
        assert_eq!(c[0].detail, "numeric");
    }

    #[test]
    fn unterminated_quoted_name_doesnt_crash() {
        // Found by the property tests: `"` alone used to panic.
        let f = fixture();
        let t = tables(&f);
        let _ = complete("select * from \"", 15, Engine::Postgres, &t);
        let _ = complete("select * from `", 15, Engine::MySql, &t);
    }

    #[test]
    fn nothing_for_an_exact_keyword() {
        let (_, l) = labels("select|");
        assert!(!l.contains(&"select".to_string()));
    }

    #[test]
    fn cte_names_and_columns() {
        let (_, l) = labels("with recent as (select id, total as amount from orders) select * from rec|");
        assert_eq!(l[0], "recent");
        let (_, l) = labels("with recent as (select id, total as amount from orders) select | from recent");
        assert!(l.starts_with(&["amount".to_string(), "id".into()]), "{l:?}");
        let (_, l) = labels("with r(a, b) as (select 1, 2) select r.| from r");
        assert_eq!(l, ["a", "b"]);
        // `*` expands from the catalog; plain columns keep their type.
        let f = fixture();
        let t = tables(&f);
        let text = "with u as (select * from users) select u.em from u";
        let (_, c) = complete(text, text.rfind(" from u").unwrap(), Engine::Postgres, &t);
        assert_eq!((c[0].label.as_str(), c[0].detail.as_str()), ("email", "text"));
    }

    #[test]
    fn subquery_aliases_and_scope() {
        let (_, l) = labels("select s.| from (select user_id, count(*) n from orders group by user_id) s");
        assert_eq!(l, ["n", "user_id"]);
        // Outside the subquery only its alias is in scope, not `orders`.
        let (_, l) = labels("select t| from (select id from orders) s");
        assert!(!l.contains(&"total".to_string()), "{l:?}");
        // Inside it, its own tables come first, and the outer ones too.
        let (_, l) = labels("select * from users u where exists (select 1 from orders o where o.user_id = u.| )");
        assert_eq!(l, ["email", "id", "UserName"]);
        let (_, l) = labels("select * from users where id in (select u| from orders)");
        assert_eq!(l[0], "user_id");
    }

    #[test]
    fn insert_column_list_skips_listed_columns() {
        let (_, l) = labels("insert into orders (|");
        assert_eq!(&l[..3], ["id", "total", "user_id"]);
        let (_, l) = labels("insert into orders (id, |");
        assert_eq!(&l[..2], ["total", "user_id"]);
    }
}
