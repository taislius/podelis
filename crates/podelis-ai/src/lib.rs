//! Podėlis's AI, without the process that serves it (the gateway, in the
//! app's crate): the models (`models`: `[[models]]` entries, their keys,
//! the Anthropic and OpenAI clients), what a model is shown of a database
//! for a question (`context`), a project's facts (`memory`), migrations read
//! into facts (`migrations`), and a model's SQL checked against the schema
//! (`names`), and what it says to models (`prompts`, from `prompts/`). Below the app: it knows the databases (`podelis-core`), never
//! the app, its config files or the gateway's protocol.

// The core's modules, under the names the code uses.
use podelis_core::{db, sql};

pub mod context;
pub mod memory;
pub mod migrations;
pub mod models;
pub mod names;
pub mod prompts;
