//! Anthropic's Messages API (`POST /v1/messages`), streamed.

use serde_json::{Value, json};

use super::sse::Event;
use super::{DEFAULT_MAX_TOKENS, ModelConfig, ModelError, Prompt, Reply};

const VERSION: &str = "2023-06-01";
/// If the model declines (a safety classifier, `stop_reason: refusal`),
/// the API runs the request again on a fallback model it picks by the
/// reason. For the models that support it, on Anthropic's own API.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const FALLBACK_MODELS: &[&str] = &["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

/// Whether a model takes `output_config.effort`: Opus from 4.5, Sonnet
/// from 4.6, Haiku from 5, and Fable and Mythos (Sonnet 4.5 and Haiku 4.5
/// refuse it).
fn takes_effort(model: &str) -> bool {
    const TAKE: &[&str] = &[
        "claude-opus-4-5",
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-opus-5",
        "claude-sonnet-4-6",
        "claude-sonnet-5",
        "claude-haiku-5",
        "claude-fable",
        "claude-mythos",
    ];
    TAKE.iter().any(|prefix| model.starts_with(prefix))
}

fn uses_fallbacks(config: &ModelConfig) -> bool {
    config.url.is_none() && FALLBACK_MODELS.contains(&config.model.as_str())
}

/// The request's body.
pub(super) fn body(config: &ModelConfig, prompt: &Prompt) -> Value {
    let max_tokens = prompt.max_tokens.or(config.max_tokens).unwrap_or(DEFAULT_MAX_TOKENS);
    let messages = messages(prompt);
    let mut body = json!({
        "model": config.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if let Some(system) = prompt.system.as_deref().filter(|s| !s.is_empty()) {
        body["system"] = json!(system);
    }
    // Set for the models that take it (their defaults differ: Opus 5.5's is
    // medium); older ones refuse it. One set in config.toml is always sent.
    let effort = config.effort.as_deref().unwrap_or(if takes_effort(&config.model) { "high" } else { "" });
    if !effort.is_empty() {
        body["output_config"] = json!({ "effort": effort });
    }
    if uses_fallbacks(config) {
        body["fallbacks"] = json!("default");
    }
    if !prompt.tools.is_empty() {
        let tools: Vec<Value> = prompt
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input }))
            .collect();
        body["tools"] = json!(tools);
        if prompt.no_more_calls {
            body["tool_choice"] = json!({ "type": "none" });
        }
    }
    body
}

/// The conversation in the API's terms: a turn with tool calls is text and
/// `tool_use` blocks; tools' answers to one turn go together in the next
/// user turn, as `tool_result` blocks.
fn messages(prompt: &Prompt) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in &prompt.messages {
        if let Some(id) = &m.answers {
            let block = json!({ "type": "tool_result", "tool_use_id": id, "content": m.content });
            match out.last_mut() {
                Some(last) if last["role"] == "user" && last["content"].is_array() => {
                    if let Some(blocks) = last["content"].as_array_mut() {
                        blocks.push(block);
                    }
                }
                _ => out.push(json!({ "role": "user", "content": [block] })),
            }
        } else if !m.calls.is_empty() {
            let mut blocks = Vec::new();
            if !m.content.is_empty() {
                blocks.push(json!({ "type": "text", "text": m.content }));
            }
            blocks.extend(
                m.calls.iter().map(|c| json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.input })),
            );
            out.push(json!({ "role": m.role, "content": blocks }));
        } else {
            out.push(json!({ "role": m.role, "content": m.content }));
        }
    }
    out
}

pub(super) async fn send(
    client: &reqwest::Client,
    config: &ModelConfig,
    api_key: Option<&str>,
    prompt: &Prompt,
    mut on_text: impl FnMut(&str),
) -> Result<Reply, ModelError> {
    let mut request = client
        .post(format!("{}/v1/messages", config.url()))
        .header("anthropic-version", VERSION)
        .header("accept", "text/event-stream");
    if let Some(key) = api_key {
        request = request.header("x-api-key", key);
    }
    if uses_fallbacks(config) {
        request = request.header("anthropic-beta", FALLBACK_BETA);
    }
    let response = super::post(request, &body(config, prompt)).await?;
    let mut answer = Answer::default();
    if !super::read_events(response, |event| answer.take(event, &mut on_text)).await? {
        return Err(super::cut_short());
    }
    Ok(answer.reply)
}

/// The answer as its events come.
#[derive(Default)]
pub(super) struct Answer {
    pub reply: Reply,
    /// The tool call being streamed: its input's JSON so far.
    call_input: String,
}

impl Answer {
    /// Takes one event; true once the answer is complete.
    pub fn take(&mut self, event: &Event, on_text: &mut impl FnMut(&str)) -> Result<bool, ModelError> {
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return Ok(false);
        };
        let kind = data["type"].as_str().unwrap_or(event.name.as_str());
        match kind {
            "message_start" => {
                self.reply.model =
                    data.pointer("/message/model").and_then(Value::as_str).unwrap_or_default().to_string();
                self.reply.input_tokens =
                    data.pointer("/message/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0);
            }
            "content_block_delta" if data.pointer("/delta/type") == Some(&json!("text_delta")) => {
                let text = data.pointer("/delta/text").and_then(Value::as_str).unwrap_or_default();
                self.reply.text.push_str(text);
                on_text(text);
            }
            // A tool call: its id and name, then its input in pieces.
            "content_block_start" if data.pointer("/content_block/type") == Some(&json!("tool_use")) => {
                let field = |name: &str| {
                    data.pointer(&format!("/content_block/{name}"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                self.reply.calls.push(super::ToolCall { id: field("id"), name: field("name"), input: json!({}) });
                self.call_input.clear();
            }
            "content_block_delta" if data.pointer("/delta/type") == Some(&json!("input_json_delta")) => {
                self.call_input
                    .push_str(data.pointer("/delta/partial_json").and_then(Value::as_str).unwrap_or_default());
            }
            "content_block_stop" if !self.call_input.is_empty() => {
                if let Some(call) = self.reply.calls.last_mut() {
                    call.input = serde_json::from_str(&self.call_input).unwrap_or_else(|_| json!({}));
                }
                self.call_input.clear();
            }
            "message_delta" => {
                if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.reply.stop_reason = reason.to_string();
                }
                // Cumulative: the last one counts.
                if let Some(n) = data.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    self.reply.output_tokens = n;
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                let error_type = data.pointer("/error/type").and_then(Value::as_str).unwrap_or("error");
                let message = data.pointer("/error/message").and_then(Value::as_str).unwrap_or("the stream failed");
                let retry = matches!(error_type, "overloaded_error" | "api_error" | "rate_limit_error");
                return Err(ModelError { message: format!("{error_type}: {message}"), retry, retry_after: None });
            }
            // ping, content_block_start/stop, thinking and other blocks.
            _ => {}
        }
        Ok(false)
    }
}
