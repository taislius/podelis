//! The windows that open over the query screen, each with its state and
//! keys (what's drawn is `ui`'s): the project's memory, the settings, the
//! activity monitor, agents' activity, the sandbox's steps, the library,
//! and a cell's full value.

pub mod activity;
pub mod agents;
pub mod cell;
pub mod library;
pub mod memory;
pub mod sandbox;
pub mod settings;
