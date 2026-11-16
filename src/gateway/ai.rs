//! The gateway's methods about models: `models` lists them, and
//! `complete` asks one and streams its answer back as `complete.delta`
//! notifications before the response. The model asked and its key are
//! found here for `ask` too.
//!
//! The configuration is read from `config.toml` on each call, so an edit
//! counts without restarting the gateway.

use std::path::Path;

use podelis_ai::models::{self, ModelConfig, Prompt};

use super::call::Call;
use super::methods::{CompleteParams, CompleteResult, KeyWaiting, ModelInfo, ModelsResult, NoParams, Note, Usage};
use super::protocol::{RpcError, code};

/// The models in `config.toml`. A file that doesn't parse is said as it
/// is, not taken for one with no models (its defaults would have none).
pub(super) fn configured(config_dir: &Path) -> Result<Vec<ModelConfig>, RpcError> {
    crate::config::settings::Settings::load_strict(config_dir)
        .map(|settings| settings.models)
        .map_err(|e| RpcError::new(code::MODEL, format!("invalid {e}")))
}

/// `models`: the configured models, the default first, and whether each
/// has its key (never the key).
pub(super) async fn models(call: &mut Call<'_>, _: NoParams) -> Result<ModelsResult, RpcError> {
    let configs = configured(call.config_dir)?;
    let models = tokio::task::spawn_blocking(move || {
        configs
            .iter()
            .map(|m| ModelInfo {
                name: m.name.clone(),
                api: m.api.label().to_string(),
                model: m.model.clone(),
                url: m.url().to_string(),
                key: models::key_source(m).label().to_string(),
            })
            .collect()
    })
    .await
    .map_err(|e| RpcError::new(code::MODEL, format!("listing the models failed: {e}")))?;
    Ok(ModelsResult { models })
}

/// How long a key may take before the client is told it's waiting (for
/// the macOS keychain's Allow dialog, or a password manager).
const KEY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// The key for `config`. One that takes longer than [`KEY_PATIENCE`]
/// sends `key.waiting` to the client first, so it can say what it's
/// waiting for.
pub(super) async fn key_for(call: &mut Call<'_>, config: &ModelConfig) -> Result<Option<String>, RpcError> {
    let lookup = config.clone();
    let keys = call.caches.keys();
    let found = tokio::task::spawn_blocking(move || {
        models::key_with(&lookup, |config, command| {
            if let Some(key) = keys.get(&config.name, command) {
                return Ok(key);
            }
            let key = models::run_key_command(config, command)?;
            keys.keep(&config.name, command, &key);
            Ok(key)
        })
    });
    tokio::pin!(found);
    let found = tokio::select! {
        found = &mut found => found,
        () = tokio::time::sleep(KEY_PATIENCE) => {
            let waiting = KeyWaiting { id: call.id().clone(), model: config.name.clone() };
            call.notify(&Note::KeyWaiting(waiting)).await;
            found.await
        }
    };
    match found {
        Ok(Ok((key, _))) => Ok(key),
        Ok(Err(e)) => Err(RpcError::new(code::MODEL, e.message)),
        Err(e) => Err(RpcError::new(code::MODEL, e.to_string())),
    }
}

/// The model named, the default one when `None`.
pub(super) fn pick(config_dir: &Path, name: Option<&str>) -> Result<ModelConfig, RpcError> {
    let configs = configured(config_dir)?;
    models::pick(&configs, name).cloned().map_err(|e| RpcError::new(code::MODEL, e.message))
}

/// A model's failure, as the client is told it. A key the provider
/// refused isn't used again: the next call gets it afresh (the user may
/// have fixed it).
pub(super) fn model_failed(call: &Call<'_>, config: &ModelConfig, e: models::ModelError) -> RpcError {
    if e.key_refused {
        call.caches.keys().forget(&config.name);
    }
    RpcError::new(code::MODEL, e.message)
}

/// `complete`: the model asked directly, its answer streamed as
/// `complete.delta`. The log keeps the model and the tokens, never the
/// text.
pub(super) async fn complete(call: &mut Call<'_>, p: CompleteParams) -> Result<CompleteResult, RpcError> {
    if p.messages.is_empty() {
        return Err(RpcError::new(code::INVALID_PARAMS, "complete needs at least one message"));
    }
    // A blank name is the default model, as in `ask`.
    let name = super::methods::given(p.model.as_deref());
    call.log("model", name);
    let config = pick(call.config_dir, name)?;
    call.log("model", &config.name);
    let key = key_for(call, &config).await?;
    let prompt = Prompt {
        system: p.system,
        messages: p.messages.into_iter().map(|m| models::Message::new(&m.role, m.content)).collect(),
        max_tokens: p.max_tokens,
        tools: Vec::new(),
        no_more_calls: false,
    };
    let (pieces, received) = tokio::sync::mpsc::unbounded_channel::<String>();
    let asked = models::complete(&config, key.as_deref(), &prompt, move |text| {
        let _ = pieces.send(text.to_string());
    });
    let reply = call.relay(Note::CompleteDelta, received, asked).await.map_err(|e| model_failed(call, &config, e))?;
    call.log("input_tokens", reply.input_tokens);
    call.log("output_tokens", reply.output_tokens);
    call.log("stop_reason", &reply.stop_reason);
    Ok(CompleteResult {
        text: reply.text,
        model: config.name,
        provider_model: reply.model,
        stop_reason: reply.stop_reason,
        usage: Usage { input_tokens: reply.input_tokens, output_tokens: reply.output_tokens },
    })
}
