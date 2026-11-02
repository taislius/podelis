//! Query parameters: `:name` placeholders (every engine), `$1` (Postgres),
//! `?` (MySQL, SQLite) and SQLite's `?NNN`, `@name` and `$name`, filled in
//! as literals before the query runs.

use std::ops::Range;

use crate::db::{Engine, sql_literal};
use crate::sql::{self, TokenKind};

/// One parameter and every place it appears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// As written (`:customer_id`, `$1`, SQLite's `?2`), or `?1`, `?2`… for
    /// `?`s.
    pub name: String,
    pub ranges: Vec<Range<usize>>,
}

/// The parameters in `text`, in order of first appearance. Placeholders
/// inside strings, quoted names and comments don't count, nor do
/// Postgres `::type` casts.
pub fn find(text: &str, engine: Engine) -> Vec<Param> {
    let tokens = sql::tokenize(text, engine);
    let b = text.as_bytes();
    let mut params: Vec<Param> = Vec::new();
    let mut add = |name: String, range: Range<usize>| match params.iter_mut().find(|p| p.name == name) {
        Some(p) => p.ranges.push(range),
        None => params.push(Param { name, ranges: vec![range] }),
    };
    // A bare `?` is numbered after the highest number so far, as SQLite
    // does: `?3, ?` is ?3 and ?4.
    let mut questions = 0;
    let placeholders = engine.dialect().placeholders();
    // The token right after `i`, if it touches it.
    let touching = |i: usize| tokens.get(i + 1).filter(|n| n.range.start == tokens[i].range.end);
    let is_name = |s: &str| s.bytes().next().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
    for (i, tok) in tokens.iter().enumerate() {
        let word = &text[tok.range.clone()];
        match tok.kind {
            TokenKind::Punct if word == ":" => {
                let Some(next) = tokens.get(i + 1) else { continue };
                let first = b[next.range.start];
                let starts_name = next.kind == TokenKind::Word && (first.is_ascii_alphabetic() || first == b'_');
                // `x::int`, `arr[1:n]` and `a:b` are not placeholders
                // (tokens are contiguous, so the previous one touches it).
                let glued_to_prev = i.checked_sub(1).is_some_and(|p| {
                    matches!(tokens[p].kind, TokenKind::Word | TokenKind::Number | TokenKind::QuotedIdent)
                        || &text[tokens[p].range.clone()] == ":"
                });
                if starts_name && !glued_to_prev {
                    add(format!(":{}", &text[next.range.clone()]), tok.range.start..next.range.end);
                }
            }
            TokenKind::Word
                if placeholders.dollar
                    && word.len() > 1
                    && word.starts_with('$')
                    && word[1..].bytes().all(|c| c.is_ascii_digit()) =>
            {
                add(word.to_string(), tok.range.clone());
            }
            TokenKind::Punct if word == "?" && placeholders.question => {
                let numbered = touching(i)
                    .filter(|n| placeholders.sqlite && n.kind == TokenKind::Number)
                    .and_then(|n| Some((text[n.range.clone()].parse::<u32>().ok()?, n.range.end)));
                if let Some((n, end)) = numbered {
                    questions = questions.max(n);
                    add(format!("?{n}"), tok.range.start..end);
                } else {
                    questions += 1;
                    add(format!("?{questions}"), tok.range.clone());
                }
            }
            TokenKind::Punct if word == "@" && placeholders.sqlite => {
                if let Some(next) = touching(i).filter(|n| n.kind == TokenKind::Word && is_name(&text[n.range.clone()]))
                {
                    add(format!("@{}", &text[next.range.clone()]), tok.range.start..next.range.end);
                }
            }
            TokenKind::Word
                if placeholders.sqlite && word.len() > 1 && word.starts_with('$') && is_name(&word[1..]) =>
            {
                add(word.to_string(), tok.range.clone());
            }
            _ => {}
        }
    }
    params
}

/// SQL for a typed value: `NULL` and numbers as they are, `=expr` as raw
/// SQL, anything else as a quoted string.
pub fn literal(value: &str, engine: Engine) -> String {
    let trimmed = value.trim();
    if let Some(raw) = trimmed.strip_prefix('=') {
        return raw.trim().to_string();
    }
    if trimmed.eq_ignore_ascii_case("null") || is_number(trimmed) {
        return trimmed.to_string();
    }
    sql_literal(value, engine)
}

fn is_number(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    // No leading zeros ("007" is probably a code, not a number).
    let (int, frac) = digits.split_once('.').map_or((digits, None), |(i, f)| (i, Some(f)));
    !int.is_empty()
        && int.bytes().all(|c| c.is_ascii_digit())
        && (int == "0" || !int.starts_with('0'))
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()))
}

/// `text` with the parameters replaced, and where each replacement went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filled {
    pub sql: String,
    /// The text with its placeholders.
    pub original: String,
    /// (range in `sql`, range in the original text), in order.
    edits: Vec<(Range<usize>, Range<usize>)>,
}

impl Filled {
    /// Maps a byte offset in the filled SQL back to the original text (an
    /// offset inside a substituted value maps to its placeholder's start).
    pub fn to_original(&self, offset: usize) -> usize {
        let mut shift: isize = 0;
        for (new, old) in &self.edits {
            if offset < new.start {
                break;
            }
            if offset < new.end {
                return old.start;
            }
            shift = old.end as isize - new.end as isize;
        }
        offset.saturating_add_signed(shift)
    }
}

impl Filled {
    /// `prefix` (e.g. `BEGIN;`) put before `base` (the text as typed, or
    /// with parameters already filled in), keeping offsets mappable.
    pub fn prefixed(base: Option<Filled>, text: &str, prefix: &str) -> Filled {
        let base =
            base.unwrap_or_else(|| Filled { sql: text.to_string(), original: text.to_string(), edits: Vec::new() });
        let n = prefix.len();
        let mut edits = vec![(0..n, 0..0)];
        edits.extend(base.edits.into_iter().map(|(new, old)| (new.start + n..new.end + n, old)));
        Filled { sql: format!("{prefix}{}", base.sql), original: base.original, edits }
    }
}

/// Substitutes `values` (one per parameter, in `params` order).
pub fn fill(text: &str, params: &[Param], values: &[String], engine: Engine) -> Filled {
    let mut spots: Vec<(Range<usize>, String)> = params
        .iter()
        .zip(values)
        .flat_map(|(p, v)| {
            let lit = literal(v, engine);
            p.ranges.iter().map(move |r| (r.clone(), lit.clone()))
        })
        .collect();
    spots.sort_by_key(|(r, _)| r.start);
    let mut sql = String::with_capacity(text.len());
    let mut edits = Vec::new();
    let mut pos = 0;
    for (range, lit) in spots {
        sql.push_str(&text[pos..range.start]);
        let start = sql.len();
        sql.push_str(&lit);
        edits.push((start..sql.len(), range.clone()));
        pos = range.end;
    }
    sql.push_str(&text[pos..]);
    Filled { sql, original: text.to_string(), edits }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str, engine: Engine) -> Vec<String> {
        find(text, engine).into_iter().map(|p| p.name).collect()
    }

    #[test]
    fn finds_named_placeholders_once_each() {
        let text = "select * from t where a = :id and b = :name or a = :id";
        assert_eq!(names(text, Engine::Postgres), [":id", ":name"]);
        assert_eq!(find(text, Engine::Postgres)[0].ranges.len(), 2);
    }

    #[test]
    fn skips_casts_strings_comments_and_slices() {
        let text = "select x::int, ':no', \":no\", arr[1:n] -- :no\n/* :no */ from t where a=:yes";
        assert_eq!(names(text, Engine::Postgres), [":yes"]);
    }

    #[test]
    fn positional_placeholders_depend_on_engine() {
        assert_eq!(names("select $1, $2, $1", Engine::Postgres), ["$1", "$2"]);
        assert_eq!(names("select data ? 'k' from t", Engine::Postgres), Vec::<String>::new());
        assert_eq!(names("select ?, ?", Engine::MySql), ["?1", "?2"]);
        assert_eq!(names("select '?', ?", Engine::Sqlite), ["?1"]);
        assert_eq!(names("select $1", Engine::MySql), Vec::<String>::new());
    }

    /// SQLite's numbered and named forms, numbered as SQLite numbers them,
    /// and filled whole: `?1` with 7 is `7`, not `71`.
    #[test]
    fn sqlite_placeholders_in_every_form() {
        let text = "select * from t where a = ?1 and b = ?2 and c = @x and d = $y and e = :z and f = ?1";
        assert_eq!(names(text, Engine::Sqlite), ["?1", "?2", "@x", "$y", ":z"]);
        let params = find(text, Engine::Sqlite);
        let values: Vec<String> = ["7", "8", "9", "s", "10"].map(String::from).to_vec();
        assert_eq!(
            fill(text, &params, &values, Engine::Sqlite).sql,
            "select * from t where a = 7 and b = 8 and c = 9 and d = 's' and e = 10 and f = 7"
        );
        // A bare `?` comes after the highest number so far.
        assert_eq!(names("select ?3, ?, ?", Engine::Sqlite), ["?3", "?4", "?5"]);
        assert_eq!(names("select ?, ?1", Engine::Sqlite), ["?1"]);
        // Not placeholders: MySQL's variables, names in strings, `$` alone.
        assert_eq!(names("select @x, @@version, ?", Engine::MySql), ["?1"]);
        assert_eq!(names("select '@x', '$y', \"?1\", 5 $ 2", Engine::Sqlite), Vec::<String>::new());
    }

    #[test]
    fn values_become_literals() {
        assert_eq!(literal("42", Engine::Postgres), "42");
        assert_eq!(literal("-1.5", Engine::Postgres), "-1.5");
        assert_eq!(literal("007", Engine::Postgres), "'007'");
        assert_eq!(literal("NULL", Engine::Postgres), "NULL");
        assert_eq!(literal("O'Brien", Engine::Postgres), "'O''Brien'");
        assert_eq!(literal("= now() ", Engine::Postgres), "now()");
        assert_eq!(literal("", Engine::Postgres), "''");
        assert_eq!(literal("1.", Engine::Postgres), "'1.'");
    }

    #[test]
    fn prefixes_keep_offsets() {
        let text = "update t set a = :v where id = 1";
        let params = find(text, Engine::Postgres);
        let filled = fill(text, &params, &["7".into()], Engine::Postgres);
        let wrapped = Filled::prefixed(Some(filled), text, "BEGIN;\n");
        assert_eq!(wrapped.sql, "BEGIN;\nupdate t set a = 7 where id = 1");
        assert_eq!(wrapped.original, text);
        assert_eq!(wrapped.to_original(wrapped.sql.find("where").unwrap()), text.find("where").unwrap());
        assert_eq!(wrapped.to_original(2), 0);
        let plain = Filled::prefixed(None, "delete from t", "BEGIN;\n");
        assert_eq!(plain.to_original(plain.sql.len()), "delete from t".len());
    }

    #[test]
    fn fills_every_occurrence_and_maps_offsets_back() {
        let text = "select :a, :bb, :a from t";
        let params = find(text, Engine::Sqlite);
        let filled = fill(text, &params, &["x".into(), "12345".into()], Engine::Sqlite);
        assert_eq!(filled.sql, "select 'x', 12345, 'x' from t");
        let from = filled.sql.find("from").unwrap();
        assert_eq!(filled.to_original(from), text.find("from").unwrap());
        assert_eq!(filled.to_original(0), 0);
        assert_eq!(filled.to_original(filled.sql.find("12345").unwrap() + 2), text.find(":bb").unwrap());
    }
}
