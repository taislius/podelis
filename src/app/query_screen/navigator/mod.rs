//! The navigator pane: its keys and its mouse, and a table's details (indexes, foreign
//! keys, definitions) loaded as it's expanded.

pub mod details;
mod keys;
mod mouse;

pub(in crate::app) use keys::step_selectable;
