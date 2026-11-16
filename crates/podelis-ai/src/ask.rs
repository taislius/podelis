//! Asking a model for SQL over a schema, or (`explain`) what a query
//! does: the prompt, then the conversation until there's an answer. A
//! model with tools may first look up tables it wasn't shown (two rounds,
//! eight tables at most; never rows), and SQL naming what the schema
//! doesn't have goes back to it, at most twice. Nothing here runs SQL, or
//! decides what may leave the machine: the gateway does that before.

use serde_json::{Value, json};

use crate::context::Schema;
use crate::models::{self, ModelConfig, ModelError, Prompt};
use crate::prompts;

/// Times a model's SQL with unknown names goes back to it.
pub const NAME_RETRIES: usize = 2;

/// The most rounds of lookups (`find_tables`, `describe_table`) before the
/// model must answer, and the most tables it may describe.
pub const LOOKUP_ROUNDS: usize = 2;
pub const LOOKUP_TABLES: usize = 8;

/// What a model is asked: to write SQL for `question` over `context` (the
/// tables chosen for it), or (`explain`) to say what the SQL in `question`
/// does. A model that uses tools may look up more tables when it's asked
/// for SQL.
pub fn prompt(config: &ModelConfig, engine: &str, context: &str, question: &str, explain: bool) -> Prompt {
    let (mut system, asked) = if explain {
        (prompts::explain(engine, context), prompts::explain_question(question))
    } else {
        (prompts::ask(engine, context), question.to_string())
    };
    let tools = if config.uses_tools() && !explain {
        system.push_str("\n\n");
        system.push_str(prompts::lookups());
        lookup_tools()
    } else {
        Vec::new()
    };
    Prompt {
        system: Some(system),
        messages: vec![models::Message::new("user", asked)],
        max_tokens: None,
        tools,
        no_more_calls: false,
    }
}

/// The tools a model asked for SQL may call: they read the schema the
/// context comes from (names, columns, keys, comments), never rows.
fn lookup_tools() -> Vec<models::Tool> {
    vec![
        models::Tool {
            name: "find_tables",
            description: prompts::find_tables_tool(),
            input: json!({
                "type": "object",
                "properties": { "words": { "type": "string", "description": "words for what's needed, e.g. \"payments revenue\"" } },
                "required": ["words"],
            }),
        },
        models::Tool {
            name: "describe_table",
            description: prompts::describe_table_tool(),
            input: json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "the table, as `schema.table` or `table`" } },
                "required": ["name"],
            }),
        },
    ]
}

/// What came of a conversation: the SQL (if any) and the model's words,
/// names still unknown, its last reply, how many answers it took, the
/// tokens, and the tables it looked up.
#[derive(Debug)]
pub struct Answered {
    pub sql: Option<String>,
    pub said: String,
    pub problems: Vec<String>,
    pub reply: models::Reply,
    pub attempts: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub looked_at: Vec<String>,
}

/// Asks until there's an answer, handing each piece of the model's text to
/// `on_text` as it comes: lookups the model makes are answered (two rounds
/// at most), and SQL with names not in the schema goes back to it
/// ([`NAME_RETRIES`] times at most).
pub async fn converse(
    config: &ModelConfig,
    key: Option<&str>,
    prompt: &mut Prompt,
    schema: &Schema,
    explain: bool,
    mut on_text: impl FnMut(&str),
) -> Result<Answered, ModelError> {
    let (mut input_tokens, mut output_tokens, mut attempts, mut rounds) = (0, 0, 0, 0);
    let mut looked_at = Vec::new();
    loop {
        let reply = models::complete(config, key, prompt, &mut on_text).await?;
        input_tokens += reply.input_tokens;
        output_tokens += reply.output_tokens;
        // Lookups: answered from the schema, then the model goes on.
        if !reply.calls.is_empty() && !prompt.tools.is_empty() && !prompt.no_more_calls {
            rounds += 1;
            prompt.messages.push(models::Message {
                calls: reply.calls.clone(),
                ..models::Message::new("assistant", reply.text.clone())
            });
            for call in &reply.calls {
                let answer = look_up(schema, call, &mut looked_at);
                prompt.messages.push(models::Message::tool_answer(&call.id, answer));
            }
            prompt.no_more_calls = rounds >= LOOKUP_ROUNDS || looked_at.len() >= LOOKUP_TABLES;
            continue;
        }
        attempts += 1;
        // An explanation is words (its blocks quoted): no SQL to check.
        let (sql, said) =
            if explain { (None, reply.text.trim().to_string()) } else { crate::names::split_answer(&reply.text) };
        let problems =
            sql.as_deref().map(|sql| crate::names::problems(sql, schema.dialect, schema)).unwrap_or_default();
        if problems.is_empty() || attempts > NAME_RETRIES {
            return Ok(Answered { sql, said, problems, reply, attempts, input_tokens, output_tokens, looked_at });
        }
        // Back to the model, in the same conversation.
        prompt.messages.push(models::Message::new("assistant", reply.text.clone()));
        let feedback = prompts::names_feedback(&problems.join("; "));
        prompt.messages.push(models::Message::new("user", feedback));
    }
}

/// A lookup the model asked for, answered from the schema; the tables it
/// describes are noted in `looked_at` (at most [`LOOKUP_TABLES`]).
fn look_up(schema: &Schema, call: &models::ToolCall, looked_at: &mut Vec<String>) -> String {
    let arg = |name: &str| call.input.get(name).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    match call.name.as_str() {
        "find_tables" => {
            let found = crate::context::find(schema, &arg("words"));
            if found.is_empty() {
                return "No table matches those words.".into();
            }
            found.iter().map(|t| format!("{} ({})", t.name, t.why)).collect::<Vec<_>>().join("\n")
        }
        "describe_table" if looked_at.len() >= LOOKUP_TABLES => {
            "Enough tables looked up: write the query with what you have.".into()
        }
        "describe_table" => match crate::context::describe(schema, &arg("name")) {
            Some((name, text)) => {
                if !looked_at.contains(&name) {
                    looked_at.push(name);
                }
                text
            }
            None => format!("There's no table named {}: find_tables looks for one by words.", arg("name")),
        },
        other => format!("There's no tool named {other}."),
    }
}

/// The `FACT: subject | relation | value` lines a model ended its note
/// with (its assumptions, three at most), taken out of it: (the note
/// without them, the facts).
pub fn suggestions(said: &str) -> (String, Vec<(String, String, String)>) {
    let mut kept = Vec::new();
    let mut facts = Vec::new();
    for line in said.lines() {
        // `- FACT:`, `**FACT:**`, `_FACT:_`: markdown around it isn't part of it.
        let fact = line.trim().trim_start_matches(['-', '*', '_', '`', ' ']).strip_prefix("FACT:").map(|f| {
            let f = f.trim_start_matches(['*', '_', '`', ' ']);
            let parts: Vec<String> = f.trim().trim_end_matches('`').split('|').map(|p| p.trim().to_string()).collect();
            parts
        });
        match fact {
            Some(parts) if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) && facts.len() < 3 => {
                let [s, r, v]: [String; 3] = parts.try_into().unwrap_or_default();
                facts.push((s, r, v));
            }
            Some(_) => {}
            None => kept.push(line),
        }
    }
    (kept.join("\n").trim().to_string(), facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_lines_in_markdown_are_read() {
        let (said, facts) = suggestions("Counts.\n**FACT:** orders.status | means | 3 = cancelled\n_FACT:_ a | is | b");
        assert_eq!(said, "Counts.");
        let subjects: Vec<&str> = facts.iter().map(|(s, _, _)| s.as_str()).collect();
        assert_eq!(subjects, ["orders.status", "a"]);
    }

    #[test]
    fn at_most_three_facts_are_taken_and_broken_ones_are_dropped() {
        let note =
            "Done.\nFACT: a | is | 1\nFACT: only two | parts\nFACT: b | is | 2\nFACT: c | is | 3\nFACT: d | is | 4";
        let (said, facts) = suggestions(note);
        assert_eq!(said, "Done.");
        let subjects: Vec<&str> = facts.iter().map(|(s, _, _)| s.as_str()).collect();
        assert_eq!(subjects, ["a", "b", "c"]);
    }

    #[test]
    fn an_explanation_has_no_lookup_tools_and_sql_has_them_for_a_model_that_uses_tools() {
        let config: ModelConfig =
            serde_json::from_value(json!({ "name": "claude", "api": "anthropic", "model": "claude-sonnet-5-5" }))
                .unwrap();
        let asked = prompt(&config, "PostgreSQL", "CREATE TABLE t (id int);", "how many", false);
        let explained = prompt(&config, "PostgreSQL", "CREATE TABLE t (id int);", "select 1", true);
        assert_eq!(asked.tools.len(), usize::from(config.uses_tools()) * 2);
        assert!(explained.tools.is_empty());
        assert!(asked.system.as_deref().is_some_and(|s| s.contains("CREATE TABLE t")));
    }
}
