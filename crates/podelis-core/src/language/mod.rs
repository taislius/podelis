//! What every query language shares, whichever one an engine speaks: its
//! tokens and statements, colouring and keywords, and what its statements
//! do (`statements`). Each entry point asks the engine's language: SQL's
//! is the `sql` module (with the engine's dialect), another's comes with its
//! engine (MongoDB's in `db/mongodb/language.rs`).

pub mod statements;

use std::ops::Range;

use crate::db::{Engine, Plan, SqlDialect};
use statements::{Kind, TxnOp};

/// An engine's query language: SQL (the `sql` module, which its dialect
/// fills in), or a language of its own.
#[derive(Clone, Copy)]
pub enum Language {
    Sql(&'static dyn SqlDialect),
    Other(&'static dyn QueryLanguage),
}

/// A query language other than SQL: what Podėlis needs of it to split a
/// script, colour it, and tell reads from writes (the `sql` module's
/// entry points ask it, so their callers needn't know which language).
pub trait QueryLanguage: Sync {
    /// `text`'s statements: each one's whole segment, and the code in it
    /// (`None` for one with only blanks and comments).
    fn segments(&self, text: &str) -> Vec<(Range<usize>, Option<Range<usize>>)>;
    /// Tokens covering every byte of `text`, on ASCII boundaries.
    fn tokenize(&self, text: &str) -> Vec<Token>;
    /// What a statement does (reads, writes, changes the schema), and why
    /// it needs a confirmation, if it does.
    fn classify(&self, statement: &str) -> (Kind, Option<&'static str>);
    /// Its words, upper case and sorted, for colouring and completion.
    fn keywords(&self) -> &'static [&'static str] {
        &[]
    }
    /// Its statement that begins, commits or rolls back a transaction on
    /// the session, as SQL's `BEGIN` / `COMMIT` / `ROLLBACK` (`None`: it
    /// has none). Whether the server takes it is the server's.
    fn transaction(&self, _op: TxnOp) -> Option<&'static str> {
        None
    }
    /// What a statement calls, the way an engine's agent constraints name
    /// it (`find`, `insertOne`): `None` if it calls nothing Podėlis knows.
    fn verb<'a>(&self, _statement: &'a str) -> Option<&'a str> {
        None
    }
    /// The statement that plans `statement` without running it, if it can
    /// be planned.
    fn explain(&self, _statement: &str) -> Option<String> {
        None
    }
    /// What the plan [`explain`](Self::explain)'s statement returned says.
    fn format_plan(&self, _columns: &[String], _rows: &[Vec<Option<String>>]) -> Option<Plan> {
        None
    }
}

/// The statement that begins (`read_only`: a read-only one, where the
/// language has that), commits or rolls back a transaction on `engine`'s
/// session; `None` where its language has none.
pub fn transaction(engine: Engine, op: TxnOp, read_only: bool) -> Option<String> {
    match engine.backend().language() {
        Language::Sql(dialect) => Some(match op {
            TxnOp::Begin => dialect.begin(read_only).to_string(),
            TxnOp::Commit => dialect.end(crate::db::TxEnd::Commit).to_string(),
            TxnOp::Rollback => dialect.end(crate::db::TxEnd::Rollback).to_string(),
        }),
        Language::Other(language) => language.transaction(op).map(str::to_string),
    }
}

/// What `statement` calls, as `engine`'s agent constraints name it: SQL's
/// first word, lower case (`select`); another language's own (`find`).
pub fn verb(engine: Engine, statement: &str) -> Option<String> {
    match engine.backend().language() {
        Language::Sql(_) => tokenize(statement, engine)
            .into_iter()
            .find(|t| t.kind == TokenKind::Word)
            .map(|t| statement[t.range].to_ascii_lowercase()),
        Language::Other(language) => language.verb(statement).map(str::to_string),
    }
}

/// The statement that plans `statement` on `engine` (of server `version`)
/// without running it; `None` where it can't be planned.
pub fn explain(engine: Engine, statement: &str, version: &str) -> Option<String> {
    match engine.backend().language() {
        Language::Sql(dialect) => Some(dialect.explain(statement.trim().trim_end_matches(';'), version)),
        Language::Other(language) => language.explain(statement),
    }
}

/// What a plan [`explain`]'s statement returned says, as Podėlis shows it.
pub fn format_plan(engine: Engine, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<Plan> {
    match engine.backend().language() {
        Language::Sql(dialect) => dialect.format_plan(columns, rows),
        Language::Other(language) => language.format_plan(columns, rows),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    Comment,
    String,
    QuotedIdent,
    Number,
    Word,
    Semicolon,
    Punct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// Splits `text` into tokens covering every byte. Token boundaries always
/// fall on ASCII bytes (or the ends of the text), so they are valid `str`
/// indices.
pub fn tokenize(text: &str, engine: Engine) -> Vec<Token> {
    match engine.backend().language() {
        Language::Sql(dialect) => crate::sql::tokenize_sql(text, dialect.lexer()),
        Language::Other(language) => language.tokenize(text),
    }
}

/// Byte range of the whole statement segment (between semicolons) that
/// contains `offset`.
pub fn statement_bounds(text: &str, offset: usize, engine: Engine) -> (usize, usize) {
    let segments = statement_segments(text, engine);
    segments
        .iter()
        .find(|(full, _)| offset <= full.end)
        .or(segments.last())
        .map_or((0, text.len()), |(full, _)| (full.start, full.end))
}

/// The non-empty statements in `text`, without their trailing semicolons.
pub fn split_statements(text: &str, engine: Engine) -> Vec<&str> {
    statement_ranges(text, engine).into_iter().map(|r| &text[r]).collect()
}

/// Where in `text` each of [`split_statements`]' statements is: byte ranges.
pub fn statement_ranges(text: &str, engine: Engine) -> Vec<Range<usize>> {
    statement_segments(text, engine).into_iter().filter_map(|(_, code)| code).collect()
}

/// The statement the cursor (a byte offset) is in. A cursor sitting right
/// after a `;`, or in blank space between statements, picks the statement
/// before it (falling back to the one after).
pub fn statement_at(text: &str, offset: usize, engine: Engine) -> Option<&str> {
    Statements::of(text, engine).at(offset).map(|r| &text[r])
}

/// The byte range of the statement the cursor (a byte offset) is in, as
/// `statement_at` picks it.
pub fn statement_range(text: &str, offset: usize, engine: Engine) -> Option<Range<usize>> {
    Statements::of(text, engine).at(offset)
}

/// A text's statements, to ask which one the cursor is in.
pub struct Statements {
    segments: Vec<(Range<usize>, Option<Range<usize>>)>,
}

impl Statements {
    pub fn of(text: &str, engine: Engine) -> Self {
        Statements { segments: statement_segments(text, engine) }
    }

    /// The segment holding the statement at `offset`, as `statement_at`
    /// picks it.
    fn pick(&self, offset: usize) -> Option<usize> {
        let segments = &self.segments;
        let idx = segments.iter().position(|(full, _)| offset <= full.end).unwrap_or(segments.len().saturating_sub(1));
        if segments[idx].1.as_ref().is_some_and(|code| offset >= code.start) {
            return Some(idx);
        }
        (0..idx).rev().chain(idx..segments.len()).find(|&i| segments[i].1.is_some())
    }

    /// The byte range of the statement at `offset`.
    fn at(&self, offset: usize) -> Option<Range<usize>> {
        self.segments[self.pick(offset)?].1.clone()
    }

    /// Which statement is at `offset`, and how many there are: `(3, 7)`.
    pub fn position(&self, offset: usize) -> Option<(usize, usize)> {
        let idx = self.pick(offset)?;
        let count =
            |segments: &[(Range<usize>, Option<Range<usize>>)]| segments.iter().filter(|s| s.1.is_some()).count();
        Some((count(&self.segments[..=idx]), count(&self.segments)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    /// A name followed by `(`.
    Function,
    /// A table name (after FROM, JOIN, INTO, UPDATE, TABLE), possibly
    /// schema-qualified.
    Table,
}

/// Each line's highlighted fragments.
pub type HighlightedLines<'a> = Vec<Vec<(&'a str, Highlight)>>;

/// A letter, digit or `_`: part of a word, for SQL's analysis and the
/// editor's word motions alike.
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Splits `text` into lines of highlighted fragments.
pub fn highlight_lines(text: &str, engine: Engine) -> HighlightedLines<'_> {
    highlight_lines_and_starts(text, engine).0
}

/// Like `highlight_lines`, plus whether each line starts outside any
/// comment, string or quoted name (where highlighting can restart).
pub fn highlight_lines_and_starts(text: &str, engine: Engine) -> (HighlightedLines<'_>, Vec<bool>) {
    let tokens = tokenize(text, engine);
    let significant: Vec<usize> =
        (0..tokens.len()).filter(|&i| !matches!(tokens[i].kind, TokenKind::Whitespace | TokenKind::Comment)).collect();
    // Position of each token in `significant`, to look at neighbours.
    let mut rank = vec![usize::MAX; tokens.len()];
    for (r, &i) in significant.iter().enumerate() {
        rank[i] = r;
    }
    let sig = |r: usize| significant.get(r).map(|&i| (&tokens[i], &text[tokens[i].range.clone()]));
    let is_table_word = |w: &str| ["FROM", "JOIN", "INTO", "UPDATE", "TABLE"].iter().any(|k| w.eq_ignore_ascii_case(k));

    let mut lines = vec![Vec::new()];
    let mut clean_starts = vec![true];
    for (i, tok) in tokens.iter().enumerate() {
        let frag = &text[tok.range.clone()];
        let hl = match tok.kind {
            TokenKind::Comment => Highlight::Comment,
            TokenKind::String => Highlight::String,
            TokenKind::Number => Highlight::Number,
            TokenKind::Word | TokenKind::QuotedIdent if tok.kind == TokenKind::Word && is_keyword(frag, engine) => {
                Highlight::Keyword
            }
            TokenKind::Word | TokenKind::QuotedIdent => {
                let r = rank[i];
                let next = sig(r + 1).map(|(_, f)| f);
                let prev = r.checked_sub(1).and_then(sig).map(|(_, f)| f);
                // In `FROM schema.table`, `table` is three tokens after FROM.
                let qualified = prev == Some(".") && r >= 3 && sig(r - 3).is_some_and(|(_, f)| is_table_word(f));
                if next == Some("(") {
                    Highlight::Function
                } else if prev.is_some_and(is_table_word) || qualified {
                    Highlight::Table
                } else {
                    Highlight::Plain
                }
            }
            _ => Highlight::Plain,
        };
        for (i, part) in frag.split('\n').enumerate() {
            if i > 0 {
                lines.push(Vec::new());
                clean_starts.push(tok.kind == TokenKind::Whitespace);
            }
            if !part.is_empty() {
                lines.last_mut().expect("there's always a line").push((part, hl));
            }
        }
    }
    (lines, clean_starts)
}

/// Whether `word` is a keyword on `engine`: SQL's own, or the engine's.
pub fn is_keyword(word: &str, engine: Engine) -> bool {
    // Upper case and sorted lists, so no upper-cased copy of the word.
    let found =
        |list: &[&str]| list.binary_search_by(|k| k.bytes().cmp(word.bytes().map(|b| b.to_ascii_uppercase()))).is_ok();
    match engine.backend().language() {
        Language::Sql(dialect) => found(crate::sql::KEYWORDS) || found(dialect.keywords()),
        Language::Other(language) => found(language.keywords()),
    }
}

/// Every keyword on `engine`, for completion: SQL's, then the engine's.
pub fn keywords(engine: Engine) -> impl Iterator<Item = &'static str> {
    let (shared, own): (&[&str], _) = match engine.backend().language() {
        Language::Sql(dialect) => (crate::sql::KEYWORDS, dialect.keywords()),
        Language::Other(language) => (&[], language.keywords()),
    };
    shared.iter().chain(own).copied()
}

/// Each statement of `text`: its whole segment, and the code in it.
fn statement_segments(text: &str, engine: Engine) -> Vec<(Range<usize>, Option<Range<usize>>)> {
    match engine.backend().language() {
        Language::Sql(_) => crate::sql::segments(text, engine),
        Language::Other(language) => language.segments(text),
    }
}
