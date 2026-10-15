//! The models the gateway talks to, configured in `config.toml`:
//!
//! ```toml
//! [[models]]
//! name = "claude"
//! api = "anthropic"
//! model = "claude-opus-5-5"
//!
//! [[models]]
//! name = "local"
//! api = "openai"                     # OpenAI's format: GPT, Ollama, llama.cpp…
//! url = "http://localhost:11434/v1"
//! model = "qwen3:8b"
//! ```
//!
//! The first is the default. Keys come from the OS keychain
//! (`podelis gateway key <name>`), else `key_command`, else the API's
//! usual environment variable; a model on this machine needs none. Only
//! the gateway reads them, and they're never logged or sent back.
//!
//! Two clients, our own and small (`anthropic`, `openai`): the gateway
//! decides what leaves the machine, so every byte sent is visible here.

mod anthropic;
mod openai;
mod sse;

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

/// How many times a request is tried again after a busy or failing
/// server (before any of its answer came).
const RETRIES: u32 = 2;
/// At most this long between two pieces of an answer.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A server on this machine answers at once, or isn't running.
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Room for an answer unless the request or the config says otherwise.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    /// Anthropic's Messages API.
    Anthropic,
    /// OpenAI's Chat Completions format, which local servers speak too.
    #[serde(rename = "openai")]
    OpenAi,
}

impl Api {
    fn default_url(self) -> &'static str {
        match self {
            Api::Anthropic => "https://api.anthropic.com",
            Api::OpenAi => "https://api.openai.com/v1",
        }
    }

    fn key_env(self) -> &'static str {
        match self {
            Api::Anthropic => "ANTHROPIC_API_KEY",
            Api::OpenAi => "OPENAI_API_KEY",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Api::Anthropic => "anthropic",
            Api::OpenAi => "openai",
        }
    }
}

/// One `[[models]]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// What it's called here (`podelis gateway key <name>`).
    pub name: String,
    pub api: Api,
    /// The provider's model ID.
    pub model: String,
    /// The API's address; the provider's own when unset.
    #[serde(default)]
    pub url: Option<String>,
    /// A command that prints the key, as a connection's password command.
    #[serde(default)]
    pub key_command: Option<String>,
    /// Anthropic: `low` … `max`; "" sends none (for models without it).
    /// Default `high`: writing SQL is work where depth pays.
    #[serde(default)]
    pub effort: Option<String>,
    /// The most an answer may be, in tokens.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl ModelConfig {
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or_else(|| self.api.default_url()).trim_end_matches('/')
    }

    /// The keychain account holding its key.
    pub fn keychain_account(name: &str) -> String {
        format!("model:{name}")
    }
}

/// A conversation to continue.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Prompt {
    #[serde(default)]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Message {
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
}

/// A model's answer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// The provider's model that answered (a fallback may answer instead).
    pub model: String,
    /// Why it stopped: `end_turn`, `max_tokens`, `refusal`… as the provider
    /// says it.
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Reply {
    pub fn to_json(&self, name: &str) -> Value {
        json!({
            "text": self.text,
            "model": name,
            "provider_model": self.model,
            "stop_reason": self.stop_reason,
            "usage": { "input_tokens": self.input_tokens, "output_tokens": self.output_tokens },
        })
    }
}

/// Why a request failed, said for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError {
    pub message: String,
    /// Worth trying again (busy, rate-limited, a server or network error).
    pub retry: bool,
    /// How long the provider asked to wait first.
    pub retry_after: Option<Duration>,
}

impl ModelError {
    fn fatal(message: impl Into<String>) -> Self {
        ModelError { message: message.into(), retry: false, retry_after: None }
    }

    fn transient(message: impl Into<String>) -> Self {
        ModelError { message: message.into(), retry: true, retry_after: None }
    }
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The model to use: `name`, or the first configured.
pub fn pick<'a>(models: &'a [ModelConfig], name: Option<&str>) -> Result<&'a ModelConfig, ModelError> {
    match name {
        Some(name) => models.iter().find(|m| m.name == name).ok_or_else(|| {
            let known: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
            ModelError::fatal(format!("no model `{name}` in config.toml (there are: {})", known.join(", ")))
        }),
        None => models.first().ok_or_else(|| {
            ModelError::fatal("no models are set up: add a [[models]] entry to config.toml (see the docs)")
        }),
    }
}

/// Whether `url` points at this machine, where plain HTTP is fine and no
/// key may be needed.
fn is_local(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Whether the model runs on this machine: what it's shown never leaves.
pub fn on_this_machine(config: &ModelConfig) -> bool {
    url::Url::parse(config.url()).is_ok_and(|u| is_local(&u))
}

/// Checks the model's address: https, unless it's on this machine (a key
/// must never travel in the clear).
fn checked_url(config: &ModelConfig) -> Result<url::Url, ModelError> {
    let url = url::Url::parse(config.url())
        .map_err(|e| ModelError::fatal(format!("model `{}`: its url isn't valid: {e}", config.name)))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_local(&url) => Ok(url),
        _ => Err(ModelError::fatal(format!(
            "model `{}`: {} must use https (plain http only to this machine)",
            config.name,
            config.url()
        ))),
    }
}

/// Where a model's key came from (for `models`, never the key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    Command,
    Env,
    /// None needed: the model is on this machine.
    Local,
    Missing,
}

impl KeySource {
    pub fn label(self) -> &'static str {
        match self {
            KeySource::Keychain => "keychain",
            KeySource::Command => "key_command",
            KeySource::Env => "environment",
            KeySource::Local => "none needed",
            KeySource::Missing => "missing",
        }
    }
}

/// The model's key and where it came from. Blocks (the keychain, the
/// command): call off the async runtime.
#[cfg(test)]
pub fn key(config: &ModelConfig) -> Result<(Option<String>, KeySource), ModelError> {
    key_with(config, run_key_command)
}

/// Runs a model's `key_command` (blocks).
pub fn run_key_command(config: &ModelConfig, command: &str) -> Result<String, ModelError> {
    let words = crate::db::password_command::split(command)
        .map_err(|e| ModelError::fatal(format!("model `{}`: key_command: {e}", config.name)))?;
    let env = [("PODELIS_MODEL", config.name.clone())];
    crate::db::password_command::run_with(&words, &env)
        .map_err(|e| ModelError::fatal(format!("model `{}`: key_command: {e:#}", config.name)))
}

/// As [`key`], with `run` getting the key from a `key_command` (the
/// gateway keeps what it printed, so a password manager isn't asked on
/// every question).
pub fn key_with(
    config: &ModelConfig,
    run: impl FnOnce(&ModelConfig, &str) -> Result<String, ModelError>,
) -> Result<(Option<String>, KeySource), ModelError> {
    // A keychain that can't be read (none on a server, or locked) isn't
    // fatal: the key may come from elsewhere, or not be needed at all.
    match crate::store::get_password(&ModelConfig::keychain_account(&config.name)) {
        Ok(stored) if !stored.is_empty() => return Ok((Some(stored), KeySource::Keychain)),
        Ok(_) => {}
        Err(e) => tracing::info!(model = %config.name, error = %format!("{e:#}"), "model: keychain not readable"),
    }
    if let Some(command) = config.key_command.as_deref().filter(|c| !c.trim().is_empty()) {
        return Ok((Some(run(config, command)?), KeySource::Command));
    }
    if let Some(key) = std::env::var(config.api.key_env()).ok().filter(|k| !k.is_empty()) {
        // The API's own variable is for the API's own address only.
        if config.url.is_none() {
            return Ok((Some(key), KeySource::Env));
        }
    }
    Ok((None, if on_this_machine(config) { KeySource::Local } else { KeySource::Missing }))
}

/// The HTTPS clients, made once: each keeps its connections, so asking
/// again (or retrying) reuses one to the provider. Local servers get a
/// shorter connect timeout: they answer at once or aren't running.
fn http(local: bool) -> Result<&'static reqwest::Client, ModelError> {
    static CLIENTS: std::sync::OnceLock<(reqwest::Client, reqwest::Client)> = std::sync::OnceLock::new();
    if CLIENTS.get().is_none() {
        let make = |timeout| {
            reqwest::Client::builder()
                .connect_timeout(timeout)
                .read_timeout(READ_TIMEOUT)
                .user_agent(concat!("podelis/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| ModelError::fatal(format!("couldn't set up HTTPS: {e}")))
        };
        let _ = CLIENTS.set((make(LOCAL_CONNECT_TIMEOUT)?, make(CONNECT_TIMEOUT)?));
    }
    let (near, far) = CLIENTS.get().ok_or_else(|| ModelError::fatal("couldn't set up HTTPS"))?;
    Ok(if local { near } else { far })
}

/// Where the model's key would come from, without running its
/// `key_command` (a password manager would ask, just to list models).
pub fn key_source(config: &ModelConfig) -> KeySource {
    if crate::store::get_password(&ModelConfig::keychain_account(&config.name)).is_ok_and(|k| !k.is_empty()) {
        return KeySource::Keychain;
    }
    if config.key_command.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return KeySource::Command;
    }
    if config.url.is_none() && std::env::var(config.api.key_env()).is_ok_and(|k| !k.is_empty()) {
        return KeySource::Env;
    }
    if on_this_machine(config) { KeySource::Local } else { KeySource::Missing }
}

/// Sends `prompt` to the model and returns its answer, calling `on_text`
/// with each piece as it arrives. Tries again on a busy or failing
/// server while nothing has arrived yet.
pub async fn complete(
    config: &ModelConfig,
    api_key: Option<&str>,
    prompt: &Prompt,
    mut on_text: impl FnMut(&str),
) -> Result<Reply, ModelError> {
    let url = checked_url(config)?;
    if api_key.is_none() && !is_local(&url) {
        return Err(ModelError::fatal(format!(
            "model `{0}` has no key: `podelis gateway key {0}` stores one in the keychain \
             (or set key_command, or {1})",
            config.name,
            config.api.key_env()
        )));
    }
    let client = http(is_local(&url))?;
    let mut attempt = 0;
    loop {
        let mut sent_text = false;
        let result = match config.api {
            Api::Anthropic => {
                anthropic::send(client, config, api_key, prompt, |t| {
                    sent_text = true;
                    on_text(t);
                })
                .await
            }
            Api::OpenAi => {
                openai::send(client, config, api_key, prompt, |t| {
                    sent_text = true;
                    on_text(t);
                })
                .await
            }
        };
        match result {
            Err(e) if e.retry && !sent_text && attempt < RETRIES => {
                attempt += 1;
                let wait =
                    e.retry_after.unwrap_or(Duration::from_secs(u64::from(attempt))).min(Duration::from_secs(60));
                tracing::info!(model = %config.name, attempt, wait_ms = wait.as_millis(), error = %e, "model: trying again");
                tokio::time::sleep(wait).await;
            }
            other => return other,
        }
    }
}

/// An HTTP error response as a `ModelError`: the provider's message, and
/// whether trying again could help.
fn http_error(status: reqwest::StatusCode, retry_after: Option<Duration>, body: &str) -> ModelError {
    let said = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect());
    let what = match status.as_u16() {
        401 => "the key was refused",
        403 => "not allowed for this key",
        404 => "not found (is the model's name right?)",
        429 => "rate-limited",
        529 => "the service is overloaded",
        500..=599 => "the service failed",
        _ => "the request was refused",
    };
    let retry = matches!(status.as_u16(), 408 | 409 | 429 | 500..=599);
    ModelError { message: format!("{what} ({status}): {said}"), retry, retry_after }
}

/// The `retry-after` header, in seconds.
pub(super) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs: f64 = headers.get("retry-after")?.to_str().ok()?.trim().parse().ok()?;
    // Clamped first: a huge value can't make a `Duration` (it would panic).
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs.min(60.0)))
}

/// Sends `body` and checks the status; the response to stream from.
async fn post(request: reqwest::RequestBuilder, body: &Value) -> Result<reqwest::Response, ModelError> {
    let request = request.header(reqwest::header::CONTENT_TYPE, "application/json").body(body.to_string());
    let response = request.send().await.map_err(|e| {
        let message = format!("couldn't reach the model: {}", without_url(&e));
        if e.is_connect() || e.is_timeout() { ModelError::transient(message) } else { ModelError::fatal(message) }
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let wait = retry_after(response.headers());
    let text = response.text().await.unwrap_or_default();
    Err(http_error(status, wait, &text))
}

/// A reqwest error said without its URL (which can carry a query).
fn without_url(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    if let Some(url) = e.url() {
        text = text.replace(url.as_str(), &format!("{}://{}", url.scheme(), url.host_str().unwrap_or("?")));
    }
    // The cause says what went wrong (DNS, refused, TLS…).
    match std::error::Error::source(e) {
        Some(source) => format!("{text}: {source}"),
        None => text,
    }
}

/// Reads an SSE response, handing each event to `on_event`; it returns
/// `Ok(true)` once the stream is complete. False: the stream ended
/// without saying it was (the caller decides whether the answer is whole).
async fn read_events(
    response: reqwest::Response,
    mut on_event: impl FnMut(&sse::Event) -> Result<bool, ModelError>,
) -> Result<bool, ModelError> {
    use futures_util::StreamExt as _;
    let mut parser = sse::Parser::default();
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| ModelError::transient(format!("the answer broke off: {}", without_url(&e))))?;
        for event in parser.push(&chunk) {
            if on_event(&event)? {
                return Ok(true);
            }
        }
    }
    // A last event the server didn't end with a blank line.
    match parser.finish() {
        Some(event) => on_event(&event),
        None => Ok(false),
    }
}

/// A stream that ended before it said it was complete.
fn cut_short() -> ModelError {
    ModelError::transient("the answer ended before it was complete")
}

#[cfg(test)]
mod tests;
