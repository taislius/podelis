//! What Podėlis says to models, kept as text in `prompts/` (its README says
//! how to change them): compiled in, so a prompt always matches the code
//! that sends it, and every word sent to a provider is in this repository.
//!
//! A file reads as Markdown does: a single line break is a space, a blank
//! line a paragraph break, a ```` ``` ```` block as written. Its `{name}`s
//! are filled once, in one pass, so a `{name}` in what's filled in (the
//! schema's text, a user's query) stays as it is.

use std::sync::LazyLock;

macro_rules! prompt {
    ($name:ident, $file:literal) => {
        static $name: LazyLock<String> = LazyLock::new(|| unwrap(include_str!(concat!("../prompts/", $file))));
    };
}

prompt!(ASK, "ask.md");
prompt!(EXPLAIN, "explain.md");
prompt!(EXPLAIN_QUESTION, "explain_question.md");
prompt!(LOOKUPS, "lookups.md");
prompt!(NAMES_FEEDBACK, "names_feedback.md");
prompt!(FIND_TABLES, "find_tables.md");
prompt!(DESCRIBE_TABLE, "describe_table.md");

/// The system prompt for writing SQL for {engine} from `context`.
pub fn ask(engine: &str, context: &str) -> String {
    fill(&ASK, &[("engine", engine), ("context", context)])
}

/// The system prompt for saying what a query for {engine} does.
pub fn explain(engine: &str, context: &str) -> String {
    fill(&EXPLAIN, &[("engine", engine), ("context", context)])
}

/// The message asking what `query` does.
pub fn explain_question(query: &str) -> String {
    fill(&EXPLAIN_QUESTION, &[("query", query)])
}

/// Added to the system prompt (after a blank line) when the model may look
/// for tables itself.
pub fn lookups() -> &'static str {
    &LOOKUPS
}

/// What goes back when the model's SQL names what the schema hasn't.
pub fn names_feedback(problems: &str) -> String {
    fill(&NAMES_FEEDBACK, &[("problems", problems)])
}

/// The `find_tables` tool's description.
pub fn find_tables_tool() -> &'static str {
    &FIND_TABLES
}

/// The `describe_table` tool's description.
pub fn describe_table_tool() -> &'static str {
    &DESCRIBE_TABLE
}

/// A file's text as it's sent: lines of a paragraph joined by spaces,
/// paragraphs a blank line apart, a fenced block's lines as they are.
fn unwrap(text: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        let fence = line.trim_start().starts_with("```");
        if fenced || fence {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
            out.push('\n');
            fenced ^= fence;
        } else if line.trim().is_empty() {
            let kept = out.trim_end_matches([' ', '\n']).len();
            out.truncate(kept);
            if !out.is_empty() {
                out.push_str("\n\n");
            }
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(line.trim());
        }
    }
    out.trim_end().to_string()
}

/// `template` with each `{name}` it has in `values` filled in, in one pass:
/// what's filled in isn't looked at again.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let name = &after[..close];
            values.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v, close))
        });
        if let Some((value, close)) = value {
            out.push_str(value);
            rest = &after[close + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The `{name}`s a template has (`{` then letters and `_`, then `}`).
#[cfg(test)]
fn placeholders(template: &str) -> Vec<&str> {
    let mut names: Vec<&str> = template
        .split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each file has exactly the `{name}`s its function fills: a misspelt
    /// one would reach the model as written.
    #[test]
    fn every_placeholder_is_filled() {
        assert_eq!(placeholders(&ASK), ["context", "engine"]);
        assert_eq!(placeholders(&EXPLAIN), ["context", "engine"]);
        assert_eq!(placeholders(&EXPLAIN_QUESTION), ["query"]);
        assert_eq!(placeholders(&NAMES_FEEDBACK), ["problems"]);
        for text in [&*LOOKUPS, &*FIND_TABLES, &*DESCRIBE_TABLE] {
            assert!(placeholders(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn files_read_as_markdown() {
        let text = "one\ntwo  \n\n\nthree\n```sql\nselect 1\n  from t\n```\nfour\n";
        assert_eq!(unwrap(text), "one two\n\nthree\n```sql\nselect 1\n  from t\n```\nfour");
    }

    #[test]
    fn filled_in_once() {
        // A `{name}` in what's filled in stays: the schema's text may have one.
        let filled = ask("PostgreSQL", "t: note TEXT -- e.g. {engine}");
        assert!(filled.starts_with("You write SQL for PostgreSQL. "), "{filled}");
        assert!(filled.ends_with("\n\nt: note TEXT -- e.g. {engine}"), "{filled}");
        assert_eq!(fill("{a} {b} {unknown} {", &[("a", "1"), ("b", "{a}")]), "1 {a} {unknown} {");
    }

    /// What models are sent, whole: a change to a prompt shows here.
    #[test]
    fn what_models_are_sent() {
        let _snapshots = crate::snapshots_in_testdata();
        insta::assert_snapshot!("ask", ask("PostgreSQL", "<the context>"));
        insta::assert_snapshot!("explain", explain("MySQL", "<the context>"));
        insta::assert_snapshot!("explain_question", explain_question("SELECT 1"));
        insta::assert_snapshot!("lookups", lookups());
        insta::assert_snapshot!("names_feedback", names_feedback("no table `x`"));
        insta::assert_snapshot!("find_tables", find_tables_tool());
        insta::assert_snapshot!("describe_table", describe_table_tool());
    }
}
