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
    messages.extend(prompt.messages.iter().map(|m| json!({ "role": m.role, "content": m.content })));
    let mut body = json!({
        "model": config.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(max) = prompt.max_tokens.or(config.max_tokens) {
        body["max_tokens"] = json!(max);
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
        }
        if let Some(usage) = data.get("usage").filter(|u| u.is_object()) {
            self.reply.input_tokens = usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
            self.reply.output_tokens = usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        }
        Ok(false)
    }
}
