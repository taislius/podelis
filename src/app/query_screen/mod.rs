//! The query screen (the editor, the navigator and the results): its keys,
//! each pane's own (`editor_keys`, `results_keys`, `navigator_keys`) and the
//! screen's (`keys`: what goes to which pane, sizes and zoom, focus, copy and
//! paste); running queries and their state (`run`), plans, exports.

mod editor_keys;
pub(in crate::app) mod export;
mod keys;
mod navigator_keys;
mod plan;
mod results_keys;
mod run;

pub use export::{Dest, Format};
pub(super) use navigator_keys::step_selectable;
pub use run::{Run, RunPurpose};
