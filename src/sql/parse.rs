//! A script split into statements and parsed once, for everything that
//! reads the syntax tree: syntax hints, lint warnings, the risk check and
//! cell editing. See docs/architecture.md ("SQL checks").

use std::ops::Range;

use sqlparser::ast::Statement;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::Span;

use crate::db::Engine;
use crate::sql;

pub struct ParsedScript<'a> {
    pub engine: Engine,
    pub statements: Vec<ParsedStatement<'a>>,
}

pub struct ParsedStatement<'a> {
    pub text: &'a str,
    /// Byte offset of `text` in the script.
    pub offset: usize,
    /// `sqlparser`'s tree, or why it couldn't build one. Its dialects are
    /// approximations, so an error here may be valid SQL for the server.
    pub ast: Result<Vec<Statement>, ParserError>,
}

/// Splits `script` with our own lexer and parses each statement with the
/// engine's `sqlparser` dialect.
pub fn parse(script: &str, engine: Engine) -> ParsedScript<'_> {
    let dialect = engine.dialect().parser();
    let statements = sql::split_statements(script, engine)
        .into_iter()
        .map(|text| ParsedStatement {
            text,
            offset: text.as_ptr() as usize - script.as_ptr() as usize,
            ast: Parser::parse_sql(&*dialect, text),
        })
        .collect();
    ParsedScript { engine, statements }
}

impl ParsedStatement<'_> {
    /// The statement's tree, if it is exactly one statement `sqlparser`
    /// understands.
    pub fn single(&self) -> Option<&Statement> {
        match &self.ast {
            Ok(parsed) if parsed.len() == 1 => parsed.first(),
            _ => None,
        }
    }

    /// A node's span as a byte range in the script, if the parser recorded
    /// one.
    pub fn range(&self, span: Span) -> Option<Range<usize>> {
        if span.start.line == 0 {
            return None;
        }
        let start = line_col_offset(self.text, span.start.line as usize, span.start.column as usize)?;
        let end = line_col_offset(self.text, span.end.line as usize, span.end.column as usize).unwrap_or(start);
        Some(self.offset + start..self.offset + end.max(start))
    }

    /// The parse error's message and its byte offset in the script.
    pub fn error(&self) -> Option<(String, Option<usize>)> {
        let Err(e) = &self.ast else { return None };
        let raw = e.to_string();
        let raw = raw.strip_prefix("sql parser error: ").unwrap_or(&raw);
        Some(match raw.rfind(" at Line: ") {
            Some(i) => {
                let offset = parse_line_col(&raw[i + " at Line: ".len()..])
                    .and_then(|(line, col)| line_col_offset(self.text, line, col))
                    .map(|o| self.offset + o);
                (raw[..i].to_string(), offset)
            }
            None => (raw.to_string(), None),
        })
    }
}

/// Parses "3, Column: 14" into (3, 14).
fn parse_line_col(s: &str) -> Option<(usize, usize)> {
    let (line, rest) = s.split_once(", Column: ")?;
    let col: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((line.trim().parse().ok()?, col.parse().ok()?))
}

/// Byte offset of a 1-based (line, char column) position in `text`.
pub fn line_col_offset(text: &str, line: usize, col: usize) -> Option<usize> {
    let mut offset = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line {
            let in_line = l.char_indices().nth(col.saturating_sub(1)).map_or(l.len(), |(b, _)| b);
            return Some(offset + in_line);
        }
        offset += l.len() + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::ast::Spanned;

    #[test]
    fn statements_keep_their_offsets() {
        let script = "select 1;\n  select * fro t; delete from t";
        let parsed = parse(script, Engine::Postgres);
        let offsets: Vec<usize> = parsed.statements.iter().map(|s| s.offset).collect();
        assert_eq!(offsets, [0, script.find("select *").unwrap(), script.find("delete").unwrap()]);
        assert!(parsed.statements[0].single().is_some());
        assert!(parsed.statements[1].single().is_none());
    }

    #[test]
    fn spans_become_script_ranges() {
        let script = "select 1;\nselect žž,\n  bad_name from t";
        let parsed = parse(script, Engine::Postgres);
        let stmt = &parsed.statements[1];
        let Some(Statement::Query(q)) = stmt.single() else { panic!() };
        let sqlparser::ast::SetExpr::Select(select) = &*q.body else { panic!() };
        let range = stmt.range(select.projection[1].span()).unwrap();
        assert_eq!(&script[range], "bad_name");
    }

    #[test]
    fn errors_point_into_the_script() {
        let script = "select 1;\nselect * fro t";
        let (message, offset) = parse(script, Engine::Postgres).statements[1].error().unwrap();
        assert!(message.contains("Expected"), "{message}");
        assert!(offset.unwrap() >= script.find("select *").unwrap());
    }

    #[test]
    fn line_col_to_offset() {
        assert_eq!(line_col_offset("ab\ncdž", 2, 3), Some(5));
        assert_eq!(line_col_offset("ab", 1, 1), Some(0));
        assert_eq!(line_col_offset("ab", 3, 1), None);
    }
}
