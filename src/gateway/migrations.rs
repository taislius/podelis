//! A repository's migration files as facts for the project's memory: when
//! each table was created and its columns added, dropped or renamed (a
//! fact per table, its history), and what the migrations' authors wrote
//! about them: a `--` comment right above a statement, `COMMENT ON`.
//!
//! No model reads them: the files are parsed here, so this costs nothing
//! and sends nothing anywhere. The SQL is read loosely (words, not a full
//! parse), so any dialect's migrations work.

use std::collections::BTreeMap;
use std::path::Path;

use crate::db::Engine;
use crate::sql::{self, TokenKind};

/// A fact learned, before it's stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Learned {
    pub subject: String,
    pub relation: String,
    pub value: String,
}

/// The migration files in `folder` (and its subfolders): `.sql`, in the
/// order of their file names (not their folders'), which is the order
/// migration tools run them in.
pub fn read_folder(folder: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("couldn't read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            // Not followed: a link back up (common in vendored trees) would
            // make the walk endless.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql")) {
                // Unreadable or not UTF-8: left out, not fatal.
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let name = path.strip_prefix(folder).unwrap_or(&path).to_string_lossy().into_owned();
                    files.push((name, text));
                }
            }
        }
    }
    if files.is_empty() {
        return Err(format!("no .sql files in {}", folder.display()));
    }
    let file_name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    files.sort_by(|a, b| file_name(&a.0).cmp(&file_name(&b.0)).then(a.0.cmp(&b.0)));
    Ok(files)
}

/// When a migration was written, from its name: `20240301…`, `2024_03_01…`
/// or `2024-03-01…` at the start of the file's name.
fn date_of(name: &str) -> Option<String> {
    let file = name.rsplit('/').next().unwrap_or(name);
    let digits: String = file.chars().take(10).filter(char::is_ascii_digit).take(8).collect();
    if digits.len() < 8 {
        return None;
    }
    let (y, m, d) = (&digits[..4], &digits[4..6], &digits[6..8]);
    let ok = ("1990".."2100").contains(&y) && ("01"..="12").contains(&m) && ("01"..="31").contains(&d);
    ok.then(|| format!("{y}-{m}-{d}"))
}

/// What happened to a table, in order.
#[derive(Default)]
struct History {
    events: Vec<String>,
}

/// The facts in `files` (name, SQL), oldest first.
pub fn learn(files: &[(String, String)]) -> Vec<Learned> {
    let mut histories: BTreeMap<String, History> = BTreeMap::new();
    let mut notes: Vec<Learned> = Vec::new();
    for (name, text) in files {
        let when = date_of(name).map_or_else(|| name.clone(), |d| format!("{d}, {name}"));
        for (comment, words, strings) in statements(text) {
            let said = |what: String, histories: &mut BTreeMap<String, History>, table: &str| {
                histories.entry(table.to_lowercase()).or_default().events.push(format!("{what} ({when})"));
            };
            let w: Vec<&str> = words.iter().map(String::as_str).collect();
            let upper: Vec<String> = w.iter().map(|s| s.to_ascii_uppercase()).collect();
            let up: Vec<&str> = upper.iter().map(String::as_str).collect();
            let mut subject: Option<String> = None;
            match up.as_slice() {
                ["CREATE", rest @ ..] if rest.contains(&"TABLE") => {
                    if let Some(table) = name_after(&w, &up, "TABLE") {
                        said("created".into(), &mut histories, &table);
                        subject = Some(table);
                    }
                }
                ["DROP", "TABLE", ..] => {
                    if let Some(table) = name_after(&w, &up, "TABLE") {
                        said("dropped".into(), &mut histories, &table);
                    }
                }
                ["ALTER", "TABLE", ..] => {
                    if let Some(table) = name_after(&w, &up, "TABLE") {
                        let changes = alter_changes(&w, &up);
                        if let [(_, column)] = changes.as_slice() {
                            subject = Some(format!("{table}.{column}"));
                        } else {
                            subject = Some(table.clone());
                        }
                        for (what, column) in changes {
                            said(format!("{column} {what}"), &mut histories, &table);
                        }
                    }
                }
                ["COMMENT", "ON", kind, ..] if matches!(*kind, "TABLE" | "COLUMN") => {
                    if let (Some(target), Some(text)) = (w.get(3), strings.last()) {
                        notes.push(Learned {
                            subject: last_parts(target, if *kind == "TABLE" { 1 } else { 2 }),
                            relation: "is".into(),
                            value: text.clone(),
                        });
                    }
                }
                _ => {}
            }
            if let (Some(subject), Some(comment)) = (subject, comment) {
                notes.push(Learned { subject, relation: "note".into(), value: comment });
            }
        }
    }
    let mut learned: Vec<Learned> = histories
        .into_iter()
        .filter(|(_, h)| !h.events.is_empty())
        .map(|(table, h)| Learned { subject: table, relation: "history".into(), value: h.events.join("; ") })
        .collect();
    learned.extend(notes);
    learned
}

/// `schema.table` → `table`; with `keep` 2, `schema.table.column` →
/// `table.column`. Quotes go.
fn last_parts(name: &str, keep: usize) -> String {
    let parts: Vec<&str> =
        name.split('.').map(|p| p.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')).collect();
    parts[parts.len().saturating_sub(keep)..].join(".")
}

/// The table named after `keyword` (skipping `IF [NOT] EXISTS`, `ONLY`),
/// without its schema.
fn name_after(words: &[&str], upper: &[&str], keyword: &str) -> Option<String> {
    let mut i = upper.iter().position(|w| *w == keyword)? + 1;
    while upper.get(i).is_some_and(|w| matches!(*w, "IF" | "NOT" | "EXISTS" | "ONLY")) {
        i += 1;
    }
    words.get(i).map(|n| last_parts(n, 1))
}

/// An `ALTER TABLE`'s column changes: (`added` / `dropped` / `renamed to
/// x`, column). Several clauses (`ADD a …, ADD b …`) give several.
fn alter_changes(words: &[&str], upper: &[&str]) -> Vec<(String, String)> {
    let mut changes = Vec::new();
    let skip = |mut i: usize| {
        while upper.get(i).is_some_and(|w| matches!(*w, "COLUMN" | "IF" | "NOT" | "EXISTS")) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < upper.len() {
        match upper[i] {
            "ADD"
                if !matches!(
                    upper.get(i + 1),
                    Some(&("CONSTRAINT" | "PRIMARY" | "FOREIGN" | "UNIQUE" | "INDEX" | "KEY" | "CHECK"))
                ) =>
            {
                let at = skip(i + 1);
                if let Some(column) = words.get(at) {
                    changes.push(("added".to_string(), last_parts(column, 1)));
                }
                i = at;
            }
            "DROP"
                if !matches!(
                    upper.get(i + 1),
                    Some(&("CONSTRAINT" | "INDEX" | "KEY" | "PRIMARY" | "FOREIGN" | "DEFAULT" | "NOT"))
                ) =>
            {
                let at = skip(i + 1);
                if let Some(column) = words.get(at) {
                    changes.push(("dropped".to_string(), last_parts(column, 1)));
                }
                i = at;
            }
            "RENAME" if upper.get(i + 1) != Some(&"TO") => {
                let at = skip(i + 1);
                if let (Some(from), Some(&"TO"), Some(to)) = (words.get(at), upper.get(at + 1), words.get(at + 2)) {
                    changes.push((format!("renamed to {}", last_parts(to, 1)), last_parts(from, 1)));
                }
                i = at + 2;
            }
            _ => {}
        }
        i += 1;
    }
    changes
}

/// Each statement of `text`: the `--` comment right above it (lines
/// joined), its words (names and keywords, quoted names with their
/// quotes), and its string literals' text.
fn statements(text: &str) -> Vec<(Option<String>, Vec<String>, Vec<String>)> {
    let tokens = sql::tokenize(text, Engine::Postgres);
    let mut out = Vec::new();
    let mut comment: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut gap_lines = 0;
    for t in &tokens {
        let s = &text[t.range.clone()];
        match t.kind {
            TokenKind::Comment if words.is_empty() => {
                // A blank line between them: the comment isn't about this one.
                if gap_lines > 1 {
                    comment.clear();
                }
                let line = s.trim_start_matches("--").trim_start_matches("/*").trim_end_matches("*/").trim();
                if !line.is_empty() {
                    comment.push(line.to_string());
                }
                gap_lines = 0;
            }
            TokenKind::Whitespace if words.is_empty() => gap_lines += s.matches('\n').count(),
            TokenKind::Word | TokenKind::QuotedIdent => words.push(s.to_string()),
            TokenKind::Punct if s == "." => {
                // `schema.table` stays one name.
                if let Some(last) = words.last_mut() {
                    last.push('.');
                }
            }
            TokenKind::String => strings.push(s.trim_matches(|c| c == '\'' || c == '"').replace("''", "'")),
            TokenKind::Semicolon => {
                if !words.is_empty() {
                    let note = (!comment.is_empty() && gap_lines <= 1).then(|| comment.join(" "));
                    out.push((note, std::mem::take(&mut words), std::mem::take(&mut strings)));
                }
                comment.clear();
                gap_lines = 0;
            }
            _ => {}
        }
    }
    if !words.is_empty() {
        let note = (!comment.is_empty() && gap_lines <= 1).then(|| comment.join(" "));
        out.push((note, words, strings));
    }
    // Rejoin `schema.` + `table` pieces split around the dot.
    out.into_iter()
        .map(|(c, ws, ss)| {
            let mut joined: Vec<String> = Vec::new();
            for w in ws {
                match joined.last_mut() {
                    Some(prev) if prev.ends_with('.') => prev.push_str(&w),
                    _ => joined.push(w),
                }
            }
            (c, joined, ss)
        })
        .collect()
}

#[cfg(test)]
mod tests;
