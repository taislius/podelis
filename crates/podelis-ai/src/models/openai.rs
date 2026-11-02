//! OpenAI's Chat Completions format (`POST …/chat/completions`), streamed:
//! OpenAI itself, and local servers that speak it (Ollama, llama.cpp,
//! LM Studio, vLLM).

use serde_json::{Value, json};

use super::sse::Event;
use super::{ModelConfig, ModelError, Prompt, Reply};

/// The request's body. No token limit unless asked for: servers name the
/// field differently, and a local model has no bill to cap.
pub(super) fn body(config: &ModelConfig, prompt: &Prompt) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = prompt.system.as_deref().filter(|s| !s.is_empty()) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    messages.extend(prompt.messages.iter().map(|m| {
        if let Some(id) = &m.answers {
            json!({ "role": "tool", "tool_call_id": id, "content": m.content })
        } else if !m.calls.is_empty() {
            let calls: Vec<Value> = m
                .calls
                .iter()
                .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.input.to_string() } }))
                .collect();
            json!({ "role": m.role, "content": m.content, "tool_calls": calls })
        } else {
            json!({ "role": m.role, "content": m.content })
        }
    }));
    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(max) = prompt.max_tokens.or(config.max_tokens) {
        body["max_tokens"] = json!(max);
    }
    if !prompt.tools.is_empty() {
        let tools: Vec<Value> = prompt
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input } }))
            .collect();
        body["tools"] = json!(tools);
        if prompt.no_more_calls {
            body["tool_choice"] = json!("none");
        }
    }
    body
}

pub(super) async fn send(
    client: &reqwest::Client,
    config: &ModelConfig,
    api_key: Option<&str>,
    prompt: &Prompt,
    mut on_text: impl FnMut(&str),
) -> Result<Reply, ModelError> {
    let mut request = client.post(format!("{}/chat/completions", config.url())).header("accept", "text/event-stream");
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = super::post(request, &body(config, prompt)).await?;
    let mut answer = Answer::default();
    let done = super::read_events(response, |event| answer.take(event, &mut on_text)).await?;
    answer.finish_calls();
    // Some servers end the stream after the last chunk without `[DONE]`:
    // a finish reason says the answer was whole.
    if !done && answer.reply.stop_reason.is_empty() {
        return Err(super::cut_short());
    }
    Ok(answer.reply)
}

#[derive(Default)]
pub(super) struct Answer {
    pub reply: Reply,
    /// Tool calls as they stream, by their index: id, name, arguments so far.
    calls: Vec<(String, String, String)>,
}

impl Answer {
    /// Takes one event; true once the answer is complete (`[DONE]`).
    pub fn take(&mut self, event: &Event, on_text: &mut impl FnMut(&str)) -> Result<bool, ModelError> {
        if event.data.trim() == "[DONE]" {
            return Ok(true);
        }
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return Ok(false);
        };
        if let Some(error) = data.get("error") {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("the stream failed");
            return Err(ModelError { message: message.to_string(), retry: false, retry_after: None });
        }
        if let Some(model) = data.get("model").and_then(Value::as_str) {
            model.clone_into(&mut self.reply.model);
        }
        if let Some(choice) = data.pointer("/choices/0") {
            if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str) {
                self.reply.text.push_str(text);
                on_text(text);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.reply.stop_reason = reason.to_string();
            }
            for call in choice.pointer("/delta/tool_calls").and_then(Value::as_array).into_iter().flatten() {
                let i = usize::try_from(call.get("index").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0);
                if self.calls.len() <= i {
                    self.calls.resize(i + 1, Default::default());
                }
                let (id, name, args) = &mut self.calls[i];
                if let Some(v) = call.get("id").and_then(Value::as_str) {
                    v.clone_into(id);
                }
                if let Some(v) = call.pointer("/function/name").and_then(Value::as_str) {
                    name.push_str(v);
                }
                if let Some(v) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    args.push_str(v);
                }
            }
        }
        if let Some(usage) = data.get("usage").filter(|u| u.is_object()) {
            self.reply.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
            self.reply.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        }
        Ok(false)
    }

    /// The tool calls, whole, once the stream has ended.
    pub fn finish_calls(&mut self) {
        self.reply.calls = std::mem::take(&mut self.calls)
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| super::ToolCall {
                id,
                name,
                input: serde_json::from_str(&args).unwrap_or_else(|_| json!({})),
            })
            .collect();
    }
}
