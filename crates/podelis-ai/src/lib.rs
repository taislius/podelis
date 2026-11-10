//! Podėlis's AI, without the process that serves it (the gateway, in the
//! app's crate): the models (`models`: `[[models]]` entries, their keys,
//! the Anthropic and OpenAI clients), what a model is shown of a database
//! for a question (`context`), a project's facts (`memory`), migrations read
//! into facts (`migrations`), the database's ontology (`ontology`: the
//! catalog and the facts as one typed graph), and a model's SQL checked against the schema
//! (`names`), and what it says to models (`prompts`, from `prompts/`). Below the app: it knows the databases (`podelis-core`), never
//! the app, its config files or the gateway's protocol.

// The core's modules, under the names the code uses.
use podelis_core::{db, sql};

pub mod context;
pub mod memory;
pub mod migrations;
pub mod models;
pub mod names;
pub mod ontology;
pub mod prompts;

/// Snapshot tests keep their `.snap` files in the crate's
/// `testdata/snapshots/`, not beside the code: hold this while asserting.
#[cfg(test)]
pub(crate) fn snapshots_in_testdata() -> insta::internals::SettingsBindDropGuard {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/snapshots"));
    settings.bind_to_scope()
}
