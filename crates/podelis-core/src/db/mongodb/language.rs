//! MongoDB's query language, as mongosh writes it: `db.<collection>.<method>(…)`
//! with chained calls (`.sort({…}).limit(10)`), `db.getCollection("…")` for
//! a name that isn't a plain word, and `db.runCommand({…})`. Arguments are
//! relaxed JSON (`driver.rs` reads them). Statements end at a `;`, or at a
//! line that starts a new `db.` outside brackets.

use std::ops::Range;

use crate::language::QueryLanguage;
use crate::language::statements::{Kind, TxnOp};
use crate::language::{Token, TokenKind};

pub struct Mongo;

/// Methods on a collection that only read.
const READS: &[&str] = &[
    "aggregate", "count", "countDocuments", "distinct", "estimatedDocumentCount", "explain", "find", "findOne",
    "getIndexes", "stats",
];
/// … that change documents.
const WRITES: &[&str] = &[
    "bulkWrite", "deleteMany", "deleteOne", "findOneAndDelete", "findOneAndReplace", "findOneAndUpdate",
    "insertMany", "insertOne", "replaceOne", "updateMany", "updateOne",
];
/// … that change the collection or its indexes.
const SCHEMA: &[&str] = &["createIndex", "createIndexes", "drop", "dropIndex", "dropIndexes", "renameCollection"];
/// Commands (`db.runCommand({<name>: …})`) that only read.
const READ_COMMANDS: &[&str] = &[
    "aggregate", "buildInfo", "collStats", "count", "dbStats", "distinct", "explain", "find", "hello",
    "listCollections", "listIndexes", "ping", "serverStatus",
];
/// … that change documents.
const WRITE_COMMANDS: &[&str] = &["delete", "findAndModify", "insert", "update"];
/// … that change collections or indexes.
const SCHEMA_COMMANDS: &[&str] =
    &["collMod", "create", "createIndexes", "drop", "dropDatabase", "dropIndexes", "renameCollection"];

/// The words coloured as keywords: `db`, its helpers and the methods, upper
/// case and sorted (`crate::language::is_keyword` searches them).
const KEYWORDS: &[&str] = &[
    "ABORTTRANSACTION", "AGGREGATE", "BULKWRITE", "COMMITTRANSACTION", "COUNT", "COUNTDOCUMENTS", "CREATECOLLECTION", "CREATEINDEX", "CREATEINDEXES", "DB",
    "DELETEMANY", "DELETEONE", "DISTINCT", "DROP", "DROPINDEX", "DROPINDEXES", "ESTIMATEDDOCUMENTCOUNT", "EXPLAIN",
    "FALSE", "FIND", "FINDONE", "FINDONEANDDELETE", "FINDONEANDREPLACE", "FINDONEANDUPDATE", "GETCOLLECTION",
    "GETINDEXES", "INSERTMANY", "INSERTONE", "ISODATE", "LIMIT", "NULL", "OBJECTID", "PROJECTION",
    "RENAMECOLLECTION", "REPLACEONE", "RUNCOMMAND", "SKIP", "SORT", "STARTTRANSACTION", "STATS", "TRUE", "UPDATEMANY", "UPDATEONE",
];

/// What a statement calls: the collection (`None` for `db.runCommand` and
/// `db.<helper>`), its method, and the methods chained after it.
#[derive(Debug, PartialEq, Eq)]
pub struct Call<'a> {
    pub collection: Option<String>,
    pub method: &'a str,
    /// Where the method's arguments are (inside its parentheses).
    pub args: Range<usize>,
    /// `.sort(…)`, `.limit(…)`, `.skip(…)`, `.projection(…)` after it,
    /// each with where its arguments are.
    pub chain: Vec<(&'a str, Range<usize>)>,
}

/// The token after `i` that isn't blank or a comment.
fn next_code(tokens: &[Token], i: usize) -> Option<usize> {
    (i..tokens.len()).find(|&j| !matches!(tokens[j].kind, TokenKind::Whitespace | TokenKind::Comment))
}

/// The end of the bracketed run that opens at token `i` (its closing
/// token's index), or the last token.
fn closing(text: &str, tokens: &[Token], i: usize) -> usize {
    let mut depth = 0i32;
    for (j, t) in tokens.iter().enumerate().skip(i) {
        if t.kind == TokenKind::Punct {
            match &text[t.range.clone()] {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
    }
    tokens.len().saturating_sub(1)
}

/// Reads what `statement` calls, if it's a call Podėlis knows the shape of.
pub fn call(statement: &str) -> Option<Call<'_>> {
    let tokens = tokenize(statement);
    let text = |i: usize| &statement[tokens[i].range.clone()];
    let word = |i: usize| (tokens[i].kind == TokenKind::Word).then(|| text(i));
    let punct = |i: usize, p: &str| tokens[i].kind == TokenKind::Punct && text(i) == p;
    // `(` … `)` after token `i`: where the arguments are, and the token after.
    let args_at = |i: usize| -> Option<(Range<usize>, usize)> {
        let open = next_code(&tokens, i)?;
        if !punct(open, "(") {
            return None;
        }
        let close = closing(statement, &tokens, open);
        Some((tokens[open].range.end..tokens[close].range.start, close + 1))
    };
    let mut i = next_code(&tokens, 0)?;
    if word(i) != Some("db") {
        return None;
    }
    i = next_code(&tokens, i + 1)?;
    if !punct(i, ".") {
        return None;
    }
    i = next_code(&tokens, i + 1)?;
    let first = word(i)?;
    let (collection, method, args, mut after) = match first {
        "runCommand" | "adminCommand" | "createCollection" | "getCollectionNames" | "stats" | "startTransaction"
        | "commitTransaction" | "abortTransaction" => {
            let (args, after) = args_at(i + 1)?;
            (None, first, args, after)
        }
        "getCollection" => {
            let (name, after) = args_at(i + 1)?;
            let name = statement[name].trim().trim_matches(['"', '\'']).to_string();
            let dot = next_code(&tokens, after)?;
            if !punct(dot, ".") {
                return None;
            }
            let m = next_code(&tokens, dot + 1)?;
            let (args, after) = args_at(m + 1)?;
            (Some(name), word(m)?, args, after)
        }
        name => {
            let dot = next_code(&tokens, i + 1)?;
            if !punct(dot, ".") {
                return None;
            }
            let m = next_code(&tokens, dot + 1)?;
            let (args, after) = args_at(m + 1)?;
            (Some(name.to_string()), word(m)?, args, after)
        }
    };
    let mut chain = Vec::new();
    while let Some(dot) = next_code(&tokens, after) {
        if !punct(dot, ".") {
            break;
        }
        let m = next_code(&tokens, dot + 1)?;
        let (args, next) = args_at(m + 1)?;
        chain.push((word(m)?, args));
        after = next;
    }
    Some(Call { collection, method, args, chain })
}

/// The first key of the document a command is (`{find: "t", …}` → `find`),
/// read by tokens as the arguments' reader reads it: past comments, a key
/// in quotes decoded (`"\u0066ind"` is `find`), and only a key a `:`
/// follows (`{"find:x": …}` names no `find`). None if it isn't one.
fn command_name(args: &str) -> Option<String> {
    let tokens = tokenize(args);
    let text = |i: usize| &args[tokens[i].range.clone()];
    let open = next_code(&tokens, 0)?;
    if !(tokens[open].kind == TokenKind::Punct && text(open) == "{") {
        return None;
    }
    let key = next_code(&tokens, open + 1)?;
    let name = match tokens[key].kind {
        TokenKind::Word => text(key).to_string(),
        TokenKind::String | TokenKind::QuotedIdent => string_value(text(key)),
        _ => return None,
    };
    let colon = next_code(&tokens, key + 1)?;
    (tokens[colon].kind == TokenKind::Punct && text(colon) == ":").then_some(name)
}

/// A string's text (between its quotes) as the server gets it: `\n`, `\t`,
/// `\r`, `\uXXXX`, and any other character after `\` as itself. The one
/// decoder: the arguments' reader (`args.rs`) and the checks here and of
/// agents' calls read a string alike, so a key can't hide behind an escape
/// one of them reads otherwise.
pub fn unescape(body: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                // Four digits, nothing else (`from_str_radix` takes a `+`).
                let digits = hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit());
                let code = u32::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| digits)
                    .ok_or_else(|| "a \\u escape isn't four hex digits".to_string())?;
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => return Err("a string ends in a lone \\".into()),
        }
    }
    Ok(out)
}

/// A string token's text, unquoted and decoded (as written if it can't be).
fn string_value(token: &str) -> String {
    let body = token.get(1..token.len().saturating_sub(1)).unwrap_or_default();
    unescape(body).unwrap_or_else(|_| body.to_string())
}

/// `args`'s code, as the server reads it: no blanks or comments, strings
/// decoded (`"\u0024out"` is `$out`), so a stage or a key can't hide from
/// the checks below behind any of them.
fn plain(args: &str) -> String {
    tokenize(args)
        .into_iter()
        .filter_map(|t| {
            let text = &args[t.range];
            match t.kind {
                TokenKind::Whitespace | TokenKind::Comment => None,
                TokenKind::String => Some(string_value(text)),
                _ => Some(text.to_string()),
            }
        })
        .collect()
}

/// Whether it writes its result somewhere (`$out`, `$merge`).
fn writes_out(args: &str) -> bool {
    let plain = plain(args);
    plain.contains("$out") || plain.contains("$merge")
}

/// Whether a write's filter (its first argument) may match every document:
/// none, `{}` (whatever blanks and comments are in it), or one that names
/// no field (`filters::names_no_field`).
fn empty_filter(args: &str) -> bool {
    super::filters::first_names_no_field(args).unwrap_or_else(|| empty_filter_by_tokens(args))
}

/// [`empty_filter`] for arguments Podėlis can't read as values: by tokens.
fn empty_filter_by_tokens(args: &str) -> bool {
    let mut depth = 0i32;
    let mut first = Vec::new();
    for t in tokenize(args) {
        let text = &args[t.range];
        match (t.kind, text) {
            (TokenKind::Whitespace | TokenKind::Comment, _) => continue,
            (TokenKind::Punct, "," ) if depth == 0 => break,
            (TokenKind::Punct, "(" | "[" | "{") => depth += 1,
            (TokenKind::Punct, ")" | "]" | "}") => depth -= 1,
            _ => {}
        }
        first.push(text);
    }
    first.is_empty() || first == ["{", "}"]
}

/// Whether a `delete` command (`db.runCommand({delete: …, deletes: […]})`)
/// has a delete whose filter (`q`) names no field; by its text when it
/// can't be read.
fn deletes_everything(args: &str) -> bool {
    super::filters::a_delete_names_no_field(args).unwrap_or_else(|| plain(args).contains("q:{}"))
}

/// A statement's words for agents' checks: words as written, and strings
/// decoded (a key may be quoted: `{"$where": …}`, `{"\u0024where": …}`).
pub fn words(statement: &str) -> Vec<String> {
    tokenize(statement)
        .into_iter()
        .filter_map(|t| {
            let text = &statement[t.range];
            match t.kind {
                TokenKind::Word => Some(text.to_string()),
                TokenKind::String => Some(string_value(text)),
                _ => None,
            }
        })
        .collect()
}

impl QueryLanguage for Mongo {
    fn segments(&self, text: &str) -> Vec<(Range<usize>, Option<Range<usize>>)> {
        let tokens = tokenize(text);
        let mut segments = Vec::new();
        let (mut start, mut code): (usize, Option<Range<usize>>) = (0, None);
        let mut depth = 0i32;
        let mut line_start = true;
        for t in &tokens {
            let s = &text[t.range.clone()];
            // A new `db.` at the start of a line, outside brackets, starts a
            // new statement (mongosh needs no `;`).
            let new_statement = depth == 0 && line_start && t.kind == TokenKind::Word && s == "db" && code.is_some();
            if new_statement {
                segments.push((start..t.range.start, code.take()));
                start = t.range.start;
            }
            match t.kind {
                TokenKind::Semicolon if depth == 0 => {
                    segments.push((start..t.range.end, code.take()));
                    start = t.range.end;
                    line_start = false;
                    continue;
                }
                TokenKind::Whitespace => {
                    if s.contains('\n') {
                        line_start = true;
                    }
                    continue;
                }
                TokenKind::Comment => continue,
                TokenKind::Punct => match s {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = (depth - 1).max(0),
                    _ => {}
                },
                _ => {}
            }
            line_start = false;
            code = Some(code.map_or(t.range.clone(), |c| c.start..t.range.end));
        }
        if start < text.len() || segments.is_empty() {
            segments.push((start..text.len(), code));
        }
        segments
    }

    fn tokenize(&self, text: &str) -> Vec<Token> {
        tokenize(text)
    }

    fn classify(&self, statement: &str) -> (Kind, Option<&'static str>) {
        let Some(call) = call(statement) else { return (Kind::Session, None) };
        let args = &statement[call.args.clone()];
        let named = |list: &[&str], name: &str| list.contains(&name);
        match (call.collection.is_some(), call.method) {
            (false, "runCommand" | "adminCommand") => {
                let name = command_name(args).unwrap_or_default();
                let name = name.as_str();
                if named(READ_COMMANDS, name) && !writes_out(args) {
                    (Kind::Read, None)
                } else if name == "aggregate" {
                    (Kind::Write, Some("writes its result into a collection ($out/$merge)"))
                } else if name == "delete" && deletes_everything(args) {
                    (Kind::Write, Some("a delete whose filter names no field (it may match every document)"))
                } else if named(WRITE_COMMANDS, name) {
                    (Kind::Write, None)
                } else if named(SCHEMA_COMMANDS, name) {
                    (Kind::Ddl, name.starts_with("drop").then_some("drops what it names"))
                } else {
                    // `shutdown`, `dropAllUsersFromDatabase`…: what Podėlis
                    // doesn't know, it doesn't let run unasked.
                    (Kind::Ddl, Some("a command Podėlis doesn't know: check what it does"))
                }
            }
            (false, "createCollection") => (Kind::Ddl, None),
            (false, "startTransaction") => (Kind::Transaction(TxnOp::Begin), None),
            (false, "commitTransaction") => (Kind::Transaction(TxnOp::Commit), None),
            (false, "abortTransaction") => (Kind::Transaction(TxnOp::Rollback), None),
            (false, _) => (Kind::Read, None),
            (true, "aggregate") if writes_out(args) => {
                (Kind::Write, Some("writes its result into a collection ($out/$merge)"))
            }
            (true, m) if named(READS, m) => (Kind::Read, None),
            (true, m @ ("deleteMany" | "updateMany")) if empty_filter(args) => {
                (Kind::Write, Some(if m == "deleteMany" {
                    "deleteMany whose filter names no field (it may match every document)"
                } else {
                    "updateMany whose filter names no field (it may match every document)"
                }))
            }
            (true, m) if named(WRITES, m) => (Kind::Write, None),
            (true, "drop") => (Kind::Ddl, Some("drops the collection")),
            (true, m) if named(SCHEMA, m) => (Kind::Ddl, None),
            (true, _) => (Kind::Session, None),
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        KEYWORDS
    }

    /// Podėlis's own statements for the session's transaction (mongosh
    /// has them on a session object): a server takes them only as a
    /// replica set or a sharded cluster's member.
    fn transaction(&self, op: TxnOp) -> Option<&'static str> {
        Some(match op {
            TxnOp::Begin => "db.startTransaction()",
            TxnOp::Commit => "db.commitTransaction()",
            TxnOp::Rollback => "db.abortTransaction()",
        })
    }

    fn words(&self, statement: &str) -> Vec<String> {
        words(statement)
    }

    /// The method: a collection's (`find`), else the database's
    /// (`runCommand`).
    fn verb<'a>(&self, statement: &'a str) -> Option<&'a str> {
        call(statement).map(|c| c.method)
    }

    fn explain(&self, statement: &str) -> Option<String> {
        super::plans::explain_command(statement)
    }

    fn format_plan(&self, columns: &[String], rows: &[Vec<Option<String>>]) -> Option<crate::db::Plan> {
        super::plans::format_plan(columns, rows)
    }
}

/// Tokens covering every byte: strings in either quotes, `//` and `/* */`
/// comments, numbers, words (with `$` and `_`), `;`, and punctuation.
pub fn tokenize(text: &str) -> Vec<Token> {
    let b = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        // A vertical tab too: JavaScript's blank space, and `trim`'s.
        let blank = |c: u8| c.is_ascii_whitespace() || c == 0x0b;
        let kind = if blank(c) {
            while i < b.len() && blank(b[i]) {
                i += 1;
            }
            TokenKind::Whitespace
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            TokenKind::Comment
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            TokenKind::Comment
        } else if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            TokenKind::String
        } else if c.is_ascii_digit() || (c == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            TokenKind::Number
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || !c.is_ascii() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || !b[i].is_ascii()) {
                i += 1;
            }
            TokenKind::Word
        } else if c == b';' {
            i += 1;
            TokenKind::Semicolon
        } else {
            i += 1;
            TokenKind::Punct
        };
        tokens.push(Token { kind, range: start..i });
    }
    tokens
}

#[cfg(test)]
mod tests {

    /// The one decoder: what the checks read is what the arguments' reader
    /// reads, a key escaped or not.
    #[test]
    fn a_strings_escapes_read_one_way() {
        assert_eq!(unescape(r"\u0024where"), Ok("$where".into()));
        assert_eq!(unescape(r"a\nb\'c\\"), Ok("a\nb'c\\".into()));
        assert!(unescape(r"\u00").is_err() && unescape("x\\").is_err());
        let words = words(r#"db.t.find({"\u0024where": 1, '\$out': 2})"#);
        assert!(words.contains(&"$where".to_string()) && words.contains(&"$out".to_string()), "{words:?}");
        #[cfg(feature = "mongodb")]
        {
            let values = super::super::args::values(r#"{"\u0024where": 1, '\$out': 2}"#).unwrap();
            let keys: Vec<String> = values[0].as_document().unwrap().keys().cloned().collect();
            assert_eq!(keys, ["$where", "$out"]);
        }
    }

    use super::*;

    fn kind(statement: &str) -> (Kind, Option<&'static str>) {
        Mongo.classify(statement)
    }

    #[test]
    fn reads_writes_and_schema_by_method() {
        assert_eq!(kind("db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(5)"), (Kind::Read, None));
        assert_eq!(kind("db.getCollection('my orders').countDocuments({})"), (Kind::Read, None));
        assert_eq!(kind("db.users.insertOne({name: 'Ada'})"), (Kind::Write, None));
        assert_eq!(
            kind("db.users.deleteMany({})").1,
            Some("deleteMany whose filter names no field (it may match every document)")
        );
        assert_eq!(kind("db.users.deleteMany({x: 1})"), (Kind::Write, None));
        assert_eq!(kind("db.users.aggregate([{$match: {}}, {$out: 'copy'}])").0, Kind::Write);
        assert_eq!(kind("db.users.aggregate([{$group: {_id: '$city'}}])"), (Kind::Read, None));
        assert_eq!(kind("db.users.drop()").0, Kind::Ddl);
        assert_eq!(kind("db.users.createIndex({email: 1})"), (Kind::Ddl, None));
        assert_eq!(kind("db.runCommand({find: 'users', filter: {}})"), (Kind::Read, None));
        assert_eq!(kind("db.runCommand({\"delete\": 'users', deletes: []})").0, Kind::Write);
        assert_eq!(kind("db.runCommand({dropDatabase: 1})").0, Kind::Ddl);
        // The key as the server reads it (found in review, 2026-11-09).
        assert_eq!(kind(r#"db.runCommand({"\u0066ind": 't'})"#), (Kind::Read, None));
        assert_eq!(kind("db.runCommand({/* c */ find: 't'})"), (Kind::Read, None));
        assert_ne!(kind(r#"db.runCommand({"find:x": 't'})"#).0, Kind::Read);
        assert_eq!(kind(r#"db.runCommand({"\u0064ropDatabase": 1})"#).0, Kind::Ddl);
        assert_eq!(kind("db.createCollection('logs')").0, Kind::Ddl);
        assert_eq!(kind("show dbs").0, Kind::Session, "not a call Podėlis knows");
        // An escape can't hide a stage that writes.
        assert_eq!(kind(r#"db.t.aggregate([{"\u0024out": "copy"}])"#).0, Kind::Write);
        assert_eq!(kind(r#"db.runCommand({aggregate: 't', pipeline: [{"\$merge": 'x'}]})"#).0, Kind::Write);
        // Commands it doesn't know, and a delete of everything, are asked about.
        assert!(kind("db.adminCommand({shutdown: 1})").1.is_some());
        assert!(kind("db.runCommand({dropAllUsersFromDatabase: 1})").1.is_some());
        assert_eq!(kind("db.runCommand({renameCollection: 'a.b', to: 'a.c'})").0, Kind::Ddl);
        assert!(kind("db.runCommand({delete: 'users', deletes: [{q: {}, limit: 0}]})").1.is_some());
    }

    #[test]
    fn a_call_and_its_chain() {
        let s = "db.users.find({a: 1}, {name: 1}).sort({name: -1}).skip(10).limit(5)";
        let c = call(s).unwrap();
        assert_eq!((c.collection.as_deref(), c.method, &s[c.args.clone()]), (Some("users"), "find", "{a: 1}, {name: 1}"));
        let chain: Vec<(&str, &str)> = c.chain.iter().map(|(m, a)| (*m, &s[a.clone()])).collect();
        assert_eq!(chain, [("sort", "{name: -1}"), ("skip", "10"), ("limit", "5")]);
        let g = call("db.getCollection(\"a b\").find()").unwrap();
        assert_eq!((g.collection.as_deref(), g.method), (Some("a b"), "find"));
    }

    #[test]
    fn statements_end_at_semicolons_or_a_new_db_line() {
        let text = "db.a.find()\ndb.b.find({x: [1,\n 2]});\n// note\ndb.c.insertOne({s: 'db.x; y'})";
        let parts: Vec<&str> = Mongo.segments(text).into_iter().filter_map(|(_, c)| c).map(|c| &text[c]).collect();
        assert_eq!(parts, ["db.a.find()", "db.b.find({x: [1,\n 2]})", "db.c.insertOne({s: 'db.x; y'})"]);
    }

    #[test]
    fn its_words_are_sorted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(KEYWORDS.iter().all(|k| k.chars().all(|c| c.is_ascii_uppercase())));
    }
}
