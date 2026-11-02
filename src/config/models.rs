//! The models in `config.toml`'s `[[models]]`: what each entry says. The
//! gateway (`gateway::models`) talks to them; the app and the settings
//! only read these.

use serde::Deserialize;

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
    pub(crate) fn default_url(self) -> &'static str {
        match self {
            Api::Anthropic => "https://api.anthropic.com",
            Api::OpenAi => "https://api.openai.com/v1",
        }
    }

    pub(crate) fn key_env(self) -> &'static str {
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
    /// Whether `ask` lets it look for more tables itself (`find_tables`,
    /// `describe_table`). Default: on for a provider's models, off for one
    /// on this machine (small models follow tool calls badly).
    #[serde(default)]
    pub tools: Option<bool>,
}

impl ModelConfig {
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or_else(|| self.api.default_url()).trim_end_matches('/')
    }

    /// Whether it may call tools (see `tools`).
    pub fn uses_tools(&self) -> bool {
        self.tools.unwrap_or_else(|| !on_this_machine(self))
    }

    /// The keychain account holding its key.
    pub fn keychain_account(name: &str) -> String {
        format!("model:{name}")
    }
}

/// Whether `url` points at this machine, where plain HTTP is fine and no
/// key may be needed.
pub(crate) fn is_local(url: &url::Url) -> bool {
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
