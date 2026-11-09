//! The query screen: the editor, the results and the navigator, each a
//! folder with its keys, its mouse and its state; and what's the screen's
//! own: which pane gets a key (`keys`) or a click (`mouse`), the popups over
//! it (`overlay`), the panes' layout (`panes`), running queries (`run`),
//! their parameters (`params`), watch mode (`watch`) and the tabs (`tabs`).
//! Then what's done from it: Ctrl+K (`palette`) and the panes' menus
//! (`menu`), writes reviewed (`review`) or in a sandbox (`sandbox`), asking
//! the AI (`ask`), comparing (`compare`), pipes and viewers (`pipes`), and
//! the connection's details (`info`).

pub mod ask;
pub(in crate::app) mod compare;
pub mod editor;
mod info;
mod keys;
pub(in crate::app) mod menu;
mod mouse;
pub mod navigator;
pub mod overlay;
pub mod palette;
pub mod panes;
pub mod params;
pub(in crate::app) mod pipes;
pub mod results;
pub(in crate::app) mod review;
mod run;
pub(in crate::app) mod sandbox;
pub mod tabs;
pub mod watch;

pub use results::export::{Dest, Format};
pub use run::{Run, RunPurpose};
