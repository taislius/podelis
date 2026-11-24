//! SQL, for every engine that speaks it: a small, forgiving lexer that
//! knows only enough to find statement boundaries (semicolons outside
//! strings and comments) and colour the editor, and never rejects input;
//! its keywords, parsing, analysis, lint, completion, scopes, parameters and
//! formatting. What every language shares (tokens, statements, colouring)
//! is in `language`, which asks this module for SQL's.

pub mod analyze;
pub mod complete;
pub mod lint;
pub mod params;
pub mod parse;
pub mod scope;
use std::ops::Range;

use crate::db::{Engine, LexerRules};
use crate::language::Language;
use crate::language::{Token, TokenKind, split_statements, tokenize};

/// SQL's tokens, by the engine's lexer rules.
pub(crate) fn tokenize_sql(text: &str, rules: LexerRules) -> Vec<Token> {
    let b = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let kind = if is_space(c) {
            while i < b.len() && is_space(b[i]) {
                i += 1;
            }
            TokenKind::Whitespace
        } else if (c == b'-' && b.get(i + 1) == Some(&b'-') && line_comment_ok(b, i, rules))
            || (c == b'#' && rules.hash_comments)
        {
            i = skip_to_eol(b, i);
            TokenKind::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = skip_block_comment(b, i, rules.nested_comments);
            TokenKind::Comment
        } else if c == b'\'' {
            let backslash = rules.backslash_escapes || is_escape_string_prefix(b, i);
            i = skip_quoted(b, i, b'\'', backslash);
            TokenKind::String
        } else if c == b'"' {
            if rules.double_quoted_strings {
                i = skip_quoted(b, i, b'"', true);
                TokenKind::String
            } else {
                i = skip_quoted(b, i, b'"', false);
                TokenKind::QuotedIdent
            }
        } else if c == b'`' && rules.backtick_identifiers {
            i = skip_quoted(b, i, b'`', false);
            TokenKind::QuotedIdent
        } else if c == b'$' && rules.dollar_quotes && dollar_tag_len(b, i).is_some() {
            i = skip_dollar_quoted(b, i);
            TokenKind::String
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            i = skip_number(b, i);
            TokenKind::Number
        } else if is_word_byte(c) {
            while i < b.len() && is_word_byte(b[i]) {
                i += 1;
            }
            TokenKind::Word
        } else if c == b';' {
            i += 1;
            TokenKind::Semicolon
        } else if is_vertical_terminator(b, i) {
            // The mysql client's `\G`: ends the statement, and its result
            // shows one column per line.
            i += 2;
            TokenKind::Semicolon
        } else {
            i += 1;
            TokenKind::Punct
        };
        tokens.push(Token { kind, range: start..i });
    }
    tokens
}

/// `\G` at `i`, not followed by more of a word.
fn is_vertical_terminator(b: &[u8], i: usize) -> bool {
    b[i] == b'\\' && b.get(i + 1) == Some(&b'G') && !b.get(i + 2).is_some_and(|&c| is_word_byte(c))
}

/// Whether a statement in `text` ends with `\G` (show its result one
/// column per line).
pub fn ends_vertical(text: &str, engine: Engine) -> bool {
    tokenize(text, engine).iter().any(|t| t.kind == TokenKind::Semicolon && &text[t.range.clone()] == "\\G")
}

/// `text` for the server: each `\\G` becomes `; ` (or spaces at the end),
/// the same length, so error positions still point into the editor.
pub fn without_vertical(text: &str, engine: Engine) -> String {
    let tokens = tokenize(text, engine);
    let mut out = text.to_string();
    for (i, t) in tokens.iter().enumerate() {
        if t.kind != TokenKind::Semicolon || &text[t.range.clone()] != "\\G" {
            continue;
        }
        let more = tokens[i + 1..]
            .iter()
            .any(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment | TokenKind::Semicolon));
        out.replace_range(t.range.clone(), if more { "; " } else { "  " });
    }
    out
}

/// ASCII whitespace as `str::trim` sees it (which, unlike
/// `u8::is_ascii_whitespace`, includes vertical tab).
fn is_space(c: u8) -> bool {
    c.is_ascii_whitespace() || c == 0x0b
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn line_comment_ok(b: &[u8], i: usize, rules: LexerRules) -> bool {
    // MySQL requires whitespace (or end of input) after `--`.
    !rules.dash_comment_needs_space || b.get(i + 2).is_none_or(|&c| is_space(c))
}

fn is_escape_string_prefix(b: &[u8], quote: usize) -> bool {
    quote > 0 && matches!(b[quote - 1], b'e' | b'E') && (quote < 2 || !is_word_byte(b[quote - 2]))
}

fn skip_to_eol(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(b: &[u8], mut i: usize, nested: bool) -> usize {
    // Postgres block comments nest; MySQL's don't.
    let mut depth = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            if depth == 0 || nested {
                depth += 1;
            }
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    i
}

fn skip_quoted(b: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    i += 1;
    while i < b.len() {
        if backslash && b[i] == b'\\' {
            i += 2;
        } else if b[i] == quote {
            if b.get(i + 1) == Some(&quote) {
                i += 2; // doubled quote is an escaped quote
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// Length of a `$tag$` opener at `i` (including both dollars), if there is one.
fn dollar_tag_len(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && is_word_byte(b[i - 1]) {
        return None; // part of an identifier like `foo$bar`
    }
    let mut j = i + 1;
    if b.get(j).is_some_and(u8::is_ascii_digit) {
        return None; // `$1` is a parameter
    }
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] >= 0x80) {
        j += 1;
    }
    (b.get(j) == Some(&b'$')).then_some(j + 1 - i)
}

fn skip_dollar_quoted(b: &[u8], i: usize) -> usize {
    let len = dollar_tag_len(b, i).expect("called where a dollar quote starts");
    let tag = &b[i..i + len];
    let body = i + len;
    b[body..].windows(len).position(|w| w == tag).map_or(b.len(), |p| body + p + len)
}

fn skip_number(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

/// A statement's byte range within the text, including its terminating `;`
/// if any, excluding surrounding whitespace and comments.
pub(crate) fn segments(text: &str, engine: Engine) -> Vec<(Range<usize>, Option<Range<usize>>)> {
    if has_delimiter_lines(text, engine) {
        return delimited(text, engine).segments;
    }
    // Each segment: (full range up to and including `;`, code range if any).
    let mut segments = Vec::new();
    let mut seg_start = 0;
    let mut code: Option<Range<usize>> = None;
    for tok in tokenize(text, engine) {
        match tok.kind {
            TokenKind::Whitespace | TokenKind::Comment => {}
            TokenKind::Semicolon => {
                segments.push((seg_start..tok.range.end, code.take()));
                seg_start = tok.range.end;
            }
            _ => {
                code = Some(match code {
                    Some(c) => c.start..tok.range.end,
                    None => tok.range.clone(),
                });
            }
        }
    }
    if seg_start < text.len() || segments.is_empty() {
        segments.push((seg_start..text.len(), code));
    }
    // Unicode spaces (e.g. U+00A0) lex as words but aren't code.
    for (_, code) in &mut segments {
        if code.as_ref().is_some_and(|c| text[c.clone()].trim().is_empty()) {
            *code = None;
        }
    }
    segments
}

/// A script read with its `DELIMITER` lines: its statements, the lines
/// themselves, and where each statement's own terminator (`$$`, `//`) is.
struct Delimited {
    segments: Vec<(Range<usize>, Option<Range<usize>>)>,
    lines: Vec<Range<usize>>,
    terminators: Vec<Range<usize>>,
}

/// Whether `text` has `word` anywhere, in any case (ASCII), without a
/// lowercased copy of it.
pub fn contains_ignoring_case(text: &str, word: &str) -> bool {
    !word.is_empty() && text.as_bytes().windows(word.len()).any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
}

/// Whether `text` has a `DELIMITER` line its engine reads (the mysql
/// client's command, not the server's).
pub fn uses_delimiter(text: &str, engine: Engine) -> bool {
    has_delimiter_lines(text, engine)
}

/// Whether `text` may have a `DELIMITER` line its engine reads (a quick
/// look: `delimited` reads it properly).
fn has_delimiter_lines(text: &str, engine: Engine) -> bool {
    engine.dialect().is_some_and(crate::db::SqlDialect::has_delimiter_command)
        // Most scripts haven't the word at all: one pass over the bytes,
        // before any line is looked at.
        && contains_ignoring_case(text, "delimiter")
        && text.lines().any(|l| {
            let mut words = l.split_whitespace();
            words.next().is_some_and(|w| w.eq_ignore_ascii_case("delimiter")) && words.next().is_some()
        })
}

/// A `DELIMITER x` command starting at `at`: where its line ends (before
/// the line break) and `x`.
fn delimiter_command(text: &str, at: usize) -> Option<(usize, &str)> {
    let end = text[at..].find(['\n', '\r']).map_or(text.len(), |n| at + n);
    let mut words = text[at..end].split_whitespace();
    let (Some(command), Some(terminator)) = (words.next(), words.next()) else { return None };
    command.eq_ignore_ascii_case("delimiter").then_some((end, terminator))
}

/// Whether only spaces are between the line's start and `at`.
fn starts_line(text: &str, at: usize) -> bool {
    text[..at].bytes().rev().take_while(|&b| b != b'\n' && b != b'\r').all(|b| b == b' ' || b == b'\t')
}

/// `text`'s statements as the mysql client reads them: a `DELIMITER x`
/// line (one of its own, not sent) makes `x` end statements until the
/// next, and `;` is then part of a statement (a routine's body).
fn delimited(text: &str, engine: Engine) -> Delimited {
    let mut out = Delimited { segments: Vec::new(), lines: Vec::new(), terminators: Vec::new() };
    let mut seg_start = 0;
    let mut code: Option<Range<usize>> = None;
    let mut terminator = ";".to_string();
    let mut skip_to = 0;
    let extend = |code: &mut Option<Range<usize>>, r: Range<usize>| {
        *code = Some(match code.take() {
            Some(c) => c.start..r.end,
            None => r,
        });
    };
    for tok in tokenize(text, engine) {
        if tok.range.start < skip_to {
            continue;
        }
        // At a line's start, as the mysql client reads it: between
        // statements; in one (left without its terminator) only as the
        // whole line, `DELIMITER $$`, which then ends it (else it's a name:
        // `select\ndelimiter from t`).
        if tok.kind == TokenKind::Word
            && starts_line(text, tok.range.start)
            && let Some((end, new)) = delimiter_command(text, tok.range.start)
            && (code.is_none() || text[tok.range.start..end].split_whitespace().count() == 2)
        {
            if code.is_some() {
                out.segments.push((seg_start..tok.range.start, code.take()));
                // Sent ended: the line, blanked, starts with a `;`.
                out.terminators.push(tok.range.start..tok.range.start + 1);
                seg_start = tok.range.start;
            }
            out.segments.push((seg_start..end, None));
            out.lines.push(tok.range.start..end);
            terminator = new.to_string();
            seg_start = end;
            skip_to = end;
            continue;
        }
        match tok.kind {
            TokenKind::Whitespace | TokenKind::Comment => {}
            TokenKind::Semicolon if terminator == ";" => {
                out.segments.push((seg_start..tok.range.end, code.take()));
                seg_start = tok.range.end;
            }
            // The mysql client's `\G` ends a statement whatever the
            // terminator (and is sent as one: `without_delimiters`).
            TokenKind::Semicolon if &text[tok.range.clone()] == "\\G" => {
                out.segments.push((seg_start..tok.range.end, code.take()));
                out.terminators.push(tok.range.clone());
                seg_start = tok.range.end;
            }
            // A name or a string may hold the terminator's characters.
            TokenKind::String | TokenKind::QuotedIdent => extend(&mut code, tok.range.clone()),
            _ if terminator == ";" => extend(&mut code, tok.range.clone()),
            // From within the token on (`END$$` is one word, `//` two, and
            // `x$$SELECT` two statements' ends and starts).
            _ => {
                let mut at = tok.range.start;
                // In the token, and as far past it as a terminator that
                // starts in it reaches: not the rest of the script, for
                // each token.
                let reach = (tok.range.end + terminator.len() - 1).min(text.len());
                let find = |at: usize| {
                    text.as_bytes()
                        .get(at..reach)?
                        .windows(terminator.len())
                        .position(|w| w == terminator.as_bytes())
                        .map(|k| at + k)
                        .filter(|&p| p < tok.range.end)
                };
                while let Some(found) = find(at) {
                    if found > at {
                        extend(&mut code, at..found);
                    }
                    let end = found + terminator.len();
                    push_as_the_server_splits(&mut out.segments, text, engine, seg_start..end, code.take());
                    out.terminators.push(found..end);
                    seg_start = end;
                    at = end;
                }
                if at < tok.range.end {
                    extend(&mut code, at..tok.range.end);
                }
                // A terminator past this token (`//`) covers the next ones.
                skip_to = skip_to.max(at);
            }
        }
    }
    if seg_start < text.len() || out.segments.is_empty() {
        out.segments.push((seg_start..text.len(), code));
    }
    for (_, code) in &mut out.segments {
        if code.as_ref().is_some_and(|c| text[c.clone()].trim().is_empty()) {
            *code = None;
        }
    }
    out
}

/// A statement ended by a `DELIMITER` terminator, as the server will read
/// it: it gets the text with each terminator made `;`, and runs every
/// statement in it. Only a stored program's definition (a procedure,
/// function, trigger or event, whose body is one `BEGIN … END` block or one
/// statement), or MariaDB's `BEGIN NOT ATOMIC … END`, keeps its `;`s;
/// anything else is as many statements as its top-level `;`s make: so
/// `SELECT 1; DROP TABLE t` under `$$` is two, each checked for what it
/// does (a write isn't hidden behind the SELECT before it).
fn push_as_the_server_splits(
    segments: &mut Vec<(Range<usize>, Option<Range<usize>>)>,
    text: &str,
    engine: Engine,
    full: Range<usize>,
    code: Option<Range<usize>>,
) {
    let Some(code) = code else { return segments.push((full, None)) };
    let base = code.start;
    let tokens: Vec<Token> = tokenize(&text[code], engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .collect();
    let word = |i: usize| -> Option<String> {
        tokens
            .get(i)
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| text[base + t.range.start..base + t.range.end].to_ascii_lowercase())
    };
    let is_semicolon = |i: usize| tokens.get(i).is_some_and(|t| t.kind == TokenKind::Semicolon);
    // Where the first statement ends (an index into `tokens`, past it).
    let first_end = stored_program_end(&tokens, &word, &is_semicolon);
    // The statements' code ranges, in the segment.
    let mut parts: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    let cut = |i: usize, start: &mut Option<usize>, parts: &mut Vec<Range<usize>>| {
        if let Some(s) = start.take() {
            parts.push(base + tokens[s].range.start..base + tokens[i - 1].range.end);
        }
    };
    for i in 0..tokens.len() {
        let inside_first = first_end.is_some_and(|end| i < end);
        if is_semicolon(i) && !inside_first {
            cut(i, &mut start, &mut parts);
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        if first_end == Some(i + 1) {
            cut(i + 1, &mut start, &mut parts);
        }
    }
    cut(tokens.len(), &mut start, &mut parts);
    // Full ranges: each from where the last ended, the last to the segment's end.
    let mut at = full.start;
    let n = parts.len();
    for (k, part) in parts.into_iter().enumerate() {
        let to = if k + 1 == n { full.end } else { part.end };
        segments.push((at..to, Some(part.clone())));
        at = to;
    }
    if n == 0 {
        segments.push((full, None));
    }
}

/// Where a stored program's definition at the segment's start ends (an
/// index past its last token): after its body's `BEGIN … END`, or its one
/// statement's `;`. `None` when it isn't one.
fn stored_program_end(
    tokens: &[Token],
    word: &dyn Fn(usize) -> Option<String>,
    is_semicolon: &dyn Fn(usize) -> bool,
) -> Option<usize> {
    let w = |i: usize| word(i).unwrap_or_default();
    // MariaDB's anonymous block, a body of its own.
    let body_at = if w(0) == "begin" && w(1) == "not" && w(2) == "atomic" {
        0
    } else {
        if w(0) != "create" {
            return None;
        }
        // CREATE [OR REPLACE] [DEFINER = user] [AGGREGATE] PROCEDURE|FUNCTION|
        // TRIGGER|EVENT, word by word: not any CREATE with such a word in it
        // (a column named `event`), which would hide what follows.
        let mut i = 1;
        if w(i) == "or" && w(i + 1) == "replace" {
            i += 2;
        }
        if w(i) == "definer" {
            // `= 'u'@'%'`, `= u@h`, `= CURRENT_USER`, `= CURRENT_USER()`.
            i += 1;
            let user_ends = (i..tokens.len().min(i + 8))
                .find(|&j| matches!(w(j).as_str(), "procedure" | "function" | "trigger" | "event" | "aggregate"))?;
            i = user_ends;
        }
        if w(i) == "aggregate" {
            i += 1;
        }
        if !matches!(w(i).as_str(), "procedure" | "function" | "trigger" | "event") {
            return None;
        }
        let kind = i;
        // The body: the first BEGIN after its name and parameters, or one
        // statement (to its `;`) where there's none.
        match (kind + 1..tokens.len()).find(|&i| w(i) == "begin" || is_semicolon(i)) {
            Some(i) if w(i) == "begin" => i,
            Some(semicolon) => return Some(semicolon),
            None => return Some(tokens.len()),
        }
    };
    // BEGIN … END, nested; a CASE's END (or END CASE) closes the CASE, and
    // END IF, END LOOP, END WHILE, END REPEAT close those, not a block.
    let (mut blocks, mut cases) = (0usize, 0usize);
    let mut i = body_at;
    while i < tokens.len() {
        match w(i).as_str() {
            "begin" => blocks += 1,
            "case" => cases += 1,
            "end" => match w(i + 1).as_str() {
                "if" | "loop" | "while" | "repeat" => i += 1,
                "case" => {
                    cases = cases.saturating_sub(1);
                    i += 1;
                }
                _ if cases > 0 => cases -= 1,
                _ => {
                    blocks = blocks.saturating_sub(1);
                    if blocks == 0 {
                        return Some(i + 1);
                    }
                }
            },
            _ => {}
        }
        i += 1;
    }
    // Unbalanced: not read as one; split at each `;` (nothing hides then).
    None
}

/// `text` for the server: its `DELIMITER` lines blanked and each
/// statement's own terminator made `;` (the server reads a routine's
/// body, `;` in it and all, as one statement), the same length, so error
/// positions still point into the editor. As it is without such lines.
pub fn without_delimiters(text: &str, engine: Engine) -> std::borrow::Cow<'_, str> {
    if !has_delimiter_lines(text, engine) {
        return std::borrow::Cow::Borrowed(text);
    }
    let script = delimited(text, engine);
    let mut out = text.as_bytes().to_vec();
    for line in &script.lines {
        out[line.clone()].fill(b' ');
    }
    for end in &script.terminators {
        out[end.clone()].fill(b' ');
        out[end.start] = b';';
    }
    // Only whole characters were replaced, each by ASCII.
    std::borrow::Cow::Owned(String::from_utf8(out).unwrap_or_else(|_| text.to_string()))
}

/// Whether a statement in `text` sets a password: `CREATE`/`ALTER USER` or
/// `ROLE` with `PASSWORD` or `IDENTIFIED`, MySQL's `GRANT … IDENTIFIED BY`
/// and `SET PASSWORD`. Such statements are kept out of the history, as
/// the `mysql` client does. Words in strings and quoted names don't count,
/// so reading a column called `password` isn't one.
pub fn sets_password(text: &str, engine: Engine) -> bool {
    split_statements(text, engine).into_iter().any(|statement| {
        let words: Vec<String> = tokenize(statement, engine)
            .into_iter()
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| statement[t.range].to_ascii_lowercase())
            .collect();
        let has = |w: &str| words.iter().any(|x| x == w);
        match words.first().map(String::as_str) {
            Some("create" | "alter") => {
                words.get(1).is_some_and(|w| w == "user" || w == "role") && (has("password") || has("identified"))
            }
            Some("grant") => has("identified"),
            Some("set") => words.get(1).is_some_and(|w| w == "password"),
            _ => false,
        }
    })
}

/// `text` laid out for reading, or `None` if that can't be done safely.
/// Strings and quoted names (with a prefix such as `E` or `U&` touching
/// them) and block comments are set aside before formatting and put back
/// as they were: the formatter would rewrite inside `$$` bodies and split
/// `U&'…'`. If anything but spacing still differs afterwards, it's refused.
pub fn format(text: &str, engine: Engine) -> Option<String> {
    let Language::Sql(dialect) = engine.backend().language() else { return None };
    let tokens = tokenize(text, engine);
    let mut kept: Vec<Range<usize>> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let block_comment = t.kind == TokenKind::Comment && text[t.range.clone()].starts_with("/*");
        if !matches!(t.kind, TokenKind::String | TokenKind::QuotedIdent) && !block_comment {
            continue;
        }
        let mut start = t.range.start;
        for prev in tokens[..i].iter().rev() {
            let prefix = prev.kind == TokenKind::Word || &text[prev.range.clone()] == "&";
            if prev.range.end != start || !prefix || block_comment {
                break;
            }
            start = prev.range.start;
        }
        // A prefix already kept with an earlier string: `'a''b'` is one token anyway.
        while kept.last().is_some_and(|k| k.end > start) {
            kept.pop();
        }
        kept.push(start..t.range.end);
    }
    let mut tag = String::from("podelis_kept_");
    while text.contains(&tag) {
        tag.insert(0, 'x');
    }
    let placeholder = |n: usize| format!("{tag}{n}_");
    let mut masked = String::with_capacity(text.len());
    let mut at = 0;
    for (n, range) in kept.iter().enumerate() {
        masked.push_str(&text[at..range.start]);
        masked.push_str(&placeholder(n));
        at = range.end;
    }
    masked.push_str(&text[at..]);

    let options = sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(2),
        max_inline_arguments: Some(80),
        max_inline_top_level: Some(80),
        dialect: dialect.formatter(),
        ..sqlformat::FormatOptions::default()
    };
    let mut formatted = sqlformat::format(&masked, &sqlformat::QueryParams::None, &options);
    for (n, range) in kept.iter().enumerate().rev() {
        let name = placeholder(n);
        if formatted.matches(&name).count() != 1 {
            return None;
        }
        formatted = formatted.replacen(&name, &text[range.clone()], 1);
    }
    (significant(text, engine) == significant(&formatted, engine)).then_some(formatted)
}

/// What formatting must keep: every token but whitespace, and comments
/// without trailing spaces.
fn significant(text: &str, engine: Engine) -> Vec<(TokenKind, &str)> {
    tokenize(text, engine)
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace)
        .map(|t| {
            let s = &text[t.range];
            (t.kind, if t.kind == TokenKind::Comment { s.trim_end() } else { s })
        })
        .collect()
}

/// The words SQL has on every engine, for highlighting and completion: the
/// ones people type as SQL, not every word an engine reserves (that would
/// mark names like `name` or `data`). Each engine adds its own
/// (`Dialect::keywords`). Upper case, sorted.
pub const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DATABASE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FOREIGN",
    "FROM",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
];

#[cfg(test)]
mod tests {
    /// What the server runs as several statements is several: a write isn't
    /// hidden behind the read before it, under a DELIMITER's terminator.
    #[test]
    fn a_delimited_block_is_as_many_statements_as_the_server_runs() {
        let my = Engine::MySql;
        assert_eq!(
            split_statements("DELIMITER $$\nSELECT 1; DROP TABLE users$$", my),
            ["SELECT 1", "DROP TABLE users"]
        );
        assert_eq!(
            split_statements("DELIMITER //\nSELECT * FROM t; DELETE FROM t//", my),
            ["SELECT * FROM t", "DELETE FROM t"]
        );
        // A routine's body is one: CASE's END, END IF, a block in it.
        let routine = "CREATE PROCEDURE p() BEGIN IF x THEN SELECT CASE WHEN a THEN 1 END; END IF; \
                       BEGIN SELECT 2; END; END";
        assert_eq!(
            split_statements(&format!("DELIMITER $$\n{routine}; DROP TABLE t$$"), my),
            [routine, "DROP TABLE t"],
            "what follows its END is another statement"
        );
        // A body of one statement (no BEGIN) ends at its `;`.
        assert_eq!(
            split_statements("DELIMITER $$\nCREATE PROCEDURE p() SELECT 1; DROP TABLE t$$", my),
            ["CREATE PROCEDURE p() SELECT 1", "DROP TABLE t"]
        );
        // Not every CREATE with such a word: a column named `event` (and one
        // named `begin`) doesn't hide the DROP after it.
        assert_eq!(
            split_statements("DELIMITER $$\nCREATE TABLE t (event INT, begin INT); DROP TABLE users$$", my),
            ["CREATE TABLE t (event INT, begin INT)", "DROP TABLE users"]
        );
        // A body that doesn't balance isn't one statement either.
        assert_eq!(
            split_statements("DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; DROP TABLE users$$", my),
            ["CREATE PROCEDURE p() BEGIN SELECT 1", "DROP TABLE users"]
        );
        // MariaDB's anonymous block, and a definer.
        assert_eq!(split_statements("DELIMITER $$\nBEGIN NOT ATOMIC SELECT 1; SELECT 2; END$$", my).len(), 1);
        let definer = "CREATE DEFINER = `u`@`%` FUNCTION f() RETURNS INT BEGIN RETURN 1; END";
        assert_eq!(split_statements(&format!("DELIMITER $$\n{definer}$$"), my), [definer]);
        let replaced =
            "CREATE OR REPLACE DEFINER = CURRENT_USER() AGGREGATE FUNCTION f() RETURNS INT BEGIN RETURN 1; END";
        assert_eq!(split_statements(&format!("DELIMITER $$\n{replaced}$$"), my), [replaced]);
        // The text sent is the same: only how it's read changed.
        let script = "DELIMITER $$\nSELECT 1; DROP TABLE users$$";
        assert_eq!(without_delimiters(script, my), "            \nSELECT 1; DROP TABLE users; ");
    }

    #[test]
    fn mysql_scripts_with_delimiter_lines_keep_routine_bodies_whole() {
        let my = Engine::MySql;
        let script = "DROP PROCEDURE IF EXISTS p;\nDELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT ';$$'; END$$\nDELIMITER ;\nCALL p();";
        assert_eq!(
            split_statements(script, my),
            ["DROP PROCEDURE IF EXISTS p", "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT ';$$'; END", "CALL p()"]
        );
        let sent = without_delimiters(script, my);
        assert_eq!(sent.len(), script.len(), "the same length: errors point into the editor");
        assert_eq!(
            sent,
            "DROP PROCEDURE IF EXISTS p;\n            \nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT ';$$'; END; \n           \nCALL p();"
        );
        // `//` and a terminator after a name, as mysqldump writes them.
        let dump = "delimiter //\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1//\ndelimiter ;";
        assert_eq!(split_statements(dump, my), ["CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1"]);
        // Not a MySQL script, or no such line: as it is.
        assert!(matches!(without_delimiters("select 1; select 2", my), std::borrow::Cow::Borrowed(_)));
        assert_eq!(split_statements("DELIMITER $$\nselect 1$$", Engine::Postgres).len(), 1);
        // Terminators inside one word, and the next statement after them.
        assert_eq!(split_statements("DELIMITER $$\nSELECT x$$SELECT 2$$", my), ["SELECT x", "SELECT 2"]);
        // After a statement left without its `;`: it ends there.
        let unended = "SELECT 1\nDELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$";
        assert_eq!(split_statements(unended, my), ["SELECT 1", "CREATE PROCEDURE p() BEGIN SELECT 1; END"]);
        assert_eq!(
            without_delimiters(unended, my),
            "SELECT 1\n;           \nCREATE PROCEDURE p() BEGIN SELECT 1; END; "
        );
        // `\G` under another terminator ends its statement, sent as `;`.
        let vertical = "DELIMITER $$\nselect * from t\\G\nselect 2$$";
        assert_eq!(split_statements(vertical, my), ["select * from t", "select 2"]);
        assert_eq!(without_delimiters(vertical, my), "            \nselect * from t; \nselect 2; ");
        // Tabs, as some editors write it.
        assert_eq!(split_statements("DELIMITER\t$$\nCREATE PROCEDURE p() BEGIN SELECT 2; END$$", my).len(), 1);
        // A column named `delimiter` isn't the command.
        assert_eq!(
            split_statements("select\ndelimiter from t; select 2", my),
            ["select\ndelimiter from t", "select 2"]
        );
    }

    use super::*;
    use crate::language::*;

    const PG: Engine = Engine::Postgres;
    const MY: Engine = Engine::MySql;

    /// SQL's words and each engine's: upper case and sorted (they're
    /// searched), and an engine adds only words SQL's list hasn't.
    #[test]
    fn keywords_are_sorted_and_each_engine_adds_its_own() {
        let upper_sorted =
            |list: &[&str]| list.windows(2).all(|w| w[0] < w[1]) && list.iter().all(|k| *k == k.to_ascii_uppercase());
        assert!(upper_sorted(KEYWORDS));
        for engine in Engine::ALL {
            let own = engine.dialect().map_or(&[][..], |d| d.keywords());
            assert!(upper_sorted(own), "{engine:?}: {own:?}");
            assert!(own.iter().all(|k| !KEYWORDS.contains(k)), "{engine:?} repeats SQL's words: {own:?}");
            for k in own {
                assert!(is_keyword(k, engine) && is_keyword(&k.to_ascii_lowercase(), engine), "{engine:?}: {k}");
            }
        }
        assert!(is_keyword("select", MY) && is_keyword("TRANSACTION", MY) && !is_keyword("TRANSACTIONS", MY));
    }

    /// An engine's own words are its, not an engine's without them:
    /// `ILIKE` is PostgreSQL's, not MySQL's; `PRAGMA` SQLite's, not
    /// PostgreSQL's; `DESCRIBE` MySQL's, not SQLite's.
    #[test]
    fn engine_words_are_only_their_engines() {
        let on = |word: &str| Engine::ALL.into_iter().filter(|e| is_keyword(word, *e)).collect::<Vec<_>>();
        let (pg, my, lite) = (Engine::Postgres, Engine::MySql, Engine::Sqlite);
        assert!(on("ilike").contains(&pg) && !on("ilike").contains(&my));
        assert!(on("PRAGMA").contains(&lite) && !on("PRAGMA").contains(&pg));
        assert!(on("describe").contains(&my) && !on("describe").contains(&lite));
        assert_eq!(on("select").len(), Engine::ALL.into_iter().filter(|e| e.dialect().is_some()).count());
    }

    #[test]
    fn blank_unicode_or_vertical_tab_isnt_a_statement() {
        assert!(split_statements("\u{b}--", Engine::Postgres).is_empty());
        assert_eq!(split_statements("\u{a0};select 1", Engine::Postgres), ["select 1"]);
    }

    #[test]
    fn splits_simple_statements() {
        assert_eq!(split_statements("select 1; select 2;", PG), ["select 1", "select 2"]);
        assert_eq!(split_statements("select 1", PG), ["select 1"]);
        // The mysql client's \G ends a statement too, outside strings.
        let my = Engine::MySql;
        assert_eq!(split_statements("select 1\\G select '\\G'\\G;", my), ["select 1", "select '\\G'"]);
        assert_eq!(split_statements("select a\\Gb", my), ["select a\\Gb"]);
        assert!(ends_vertical("select 1\\G", my) && !ends_vertical("select '\\G'", my));
        assert_eq!(without_vertical("select 1\\G select 2\\G -- x", my), "select 1;  select 2   -- x");
        assert_eq!(statement_at("select 1\\G\nselect 2", 3, my), Some("select 1"));
        assert_eq!(split_statements("  -- nothing\n ; ", PG), Vec::<&str>::new());
    }

    #[test]
    fn ignores_semicolons_in_strings_and_comments() {
        let sql = "select ';' -- ;\n, \"a;b\" /* ; */ from t; select 2";
        assert_eq!(split_statements(sql, PG), ["select ';' -- ;\n, \"a;b\" /* ; */ from t", "select 2"]);
    }

    #[test]
    fn leading_comment_is_not_part_of_statement() {
        assert_eq!(split_statements("-- hi\nselect 1", PG), ["select 1"]);
    }

    #[test]
    fn dollar_quotes() {
        let sql = "create function f() returns int as $body$ select 1; $body$ language sql; select $1";
        assert_eq!(split_statements(sql, PG).len(), 2);
        assert_eq!(split_statements("select $$a;b$$", PG), ["select $$a;b$$"]);
    }

    #[test]
    fn nested_block_comments_in_postgres_only() {
        assert_eq!(split_statements("/* a /* b */ ; */ select 1", PG), ["select 1"]);
        assert_eq!(split_statements("/* a /* b */ ; select 1", MY), ["select 1"]);
    }

    #[test]
    fn backslash_escapes() {
        // MySQL: backslash escapes the quote.
        assert_eq!(split_statements(r"select 'a\';b'; select 2", MY).len(), 2);
        // Postgres standard strings: backslash is literal, so the string ends at \'.
        assert_eq!(split_statements(r"select 'a\'; select 2", PG).len(), 2);
        // Postgres E'' strings honour backslashes.
        assert_eq!(split_statements(r"select E'a\';b'; select 2", PG).len(), 2);
    }

    #[test]
    fn mysql_comments() {
        assert_eq!(split_statements("# x;\nselect 1", MY), ["select 1"]);
        assert_eq!(split_statements("select 1--1;", MY), ["select 1--1"]);
    }

    #[test]
    fn statement_at_cursor() {
        let sql = "select 1;\nselect 2;\n\nselect 3";
        assert_eq!(statement_at(sql, 0, PG), Some("select 1"));
        assert_eq!(statement_at(sql, 9, PG), Some("select 1")); // right after `;`
        assert_eq!(statement_at(sql, 12, PG), Some("select 2"));
        assert_eq!(statement_at(sql, 20, PG), Some("select 2")); // blank line
        assert_eq!(statement_at(sql, sql.len(), PG), Some("select 3"));
        assert_eq!(statement_at("  ;  select 9", 0, PG), Some("select 9"));
        assert_eq!(statement_at("", 0, PG), None);
        // The position counts only real statements, picked the same way.
        let at = |offset| Statements::of(sql, PG).position(offset);
        assert_eq!([at(0), at(9), at(12), at(20), at(sql.len())], [1, 1, 2, 2, 3].map(|n| Some((n, 3))));
        assert_eq!(Statements::of("  ;  select 9; -- done", PG).position(0), Some((1, 1)));
        assert_eq!(Statements::of("select ';'; select 2", PG).position(0), Some((1, 2)), "a `;` in a string");
        assert_eq!(Statements::of(" -- nothing", PG).position(0), None);
    }

    #[test]
    fn statements_that_set_passwords() {
        let my = Engine::MySql;
        for (engine, sql) in [
            (PG, "alter user bob password 'hunter2'"),
            (PG, "ALTER ROLE bob WITH ENCRYPTED PASSWORD 'x'"),
            (PG, "create role app login password 'x'"),
            (PG, "select 1; create user mapping for bob server s options (user 'b', password 'x')"),
            (my, "CREATE USER 'bob'@'%' IDENTIFIED BY 'pw'"),
            (my, "alter user current_user() identified by 'pw'"),
            (my, "grant all on *.* to bob identified by 'pw'"),
            (my, "SET PASSWORD FOR 'bob'@'%' = 'pw'"),
        ] {
            assert!(sets_password(sql, engine), "{sql}");
        }
        for (engine, sql) in [
            (PG, "select password from users where name = 'bob'"),
            (PG, "update users set password = 'x' where id = 1"),
            (PG, "create table users (id int, password text)"),
            (PG, "create table user (password text)"),
            (PG, "alter table users add column password text"),
            (PG, "create user bob"),
            (my, "select 'alter user x identified by y'"),
            (my, "set names utf8mb4"),
        ] {
            assert!(!sets_password(sql, engine), "{sql}");
        }
    }

    #[test]
    fn formatting_keeps_strings_and_names_as_written() {
        let kept = |sql: &str, part: &str| {
            let out = format(sql, PG).unwrap_or_else(|| panic!("refused {sql:?}"));
            assert!(out.contains(part), "{part:?} changed in\n{out}");
            out
        };
        // These came out changed before: the values, a function body, `U&'…'`.
        kept(
            "insert into notes values ($$two  spaces\n  and an indented line$$, $q$it's   here$q$)",
            "$$two  spaces\n  and an indented line$$",
        );
        kept("insert into notes values ($q$it's   here$q$)", "$q$it's   here$q$");
        kept(
            "create function f() returns int language plpgsql as $$ begin return 1; end $$",
            "$$ begin return 1; end $$",
        );
        kept("select U&'d\\0061t', E'a\\nb' from t", "U&'d\\0061t'");
        kept("select \"Full  Name\" from t /* a  comment\n   kept */ where x = 1", "/* a  comment\n   kept */");
        let out = kept("select a, b from t where c = 'x  y' and d = 1", "'x  y'");
        assert!(out.contains("\nfrom t\nwhere"), "still laid out: {out}");
        let my = format("select `a  b`, _utf8mb4'x  y' from `t`", Engine::MySql).unwrap();
        assert!(my.contains("`a  b`") && my.contains("_utf8mb4'x  y'"), "{my}");
    }

    #[test]
    fn highlight_splits_multiline_tokens() {
        let lines = highlight_lines("select /* a\nb */ 'x'", PG);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0], ("select", Highlight::Keyword));
        assert_eq!(lines[0][2], ("/* a", Highlight::Comment));
        assert_eq!(lines[1][0], ("b */", Highlight::Comment));
        assert_eq!(lines[1][2], ("'x'", Highlight::String));
    }

    #[test]
    fn highlight_functions_and_tables() {
        let lines = highlight_lines("select count(*) from public.users u join orders on true", PG);
        let kinds: Vec<_> = lines[0].iter().filter(|(_, h)| *h != Highlight::Plain).copied().collect();
        assert_eq!(
            kinds,
            [
                ("select", Highlight::Keyword),
                ("count", Highlight::Function),
                ("from", Highlight::Keyword),
                ("public", Highlight::Table),
                ("users", Highlight::Table),
                ("join", Highlight::Keyword),
                ("orders", Highlight::Table),
                ("on", Highlight::Keyword),
                ("true", Highlight::Keyword),
            ]
        );
    }

    #[test]
    fn unicode_is_preserved() {
        let sql = "select 'žąsis'; select ėė";
        assert_eq!(split_statements(sql, PG), ["select 'žąsis'", "select ėė"]);
    }
}
