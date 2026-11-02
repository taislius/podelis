//! The machine around Podėlis: the clipboard, database containers, crash reports, the debug log, installed programs and terminal prompts.

pub(crate) mod clipboard;
pub(crate) mod containers;
pub(crate) mod crash;
pub(crate) mod environment;
pub(crate) mod logging;
pub(crate) mod programs;
pub(crate) mod tty;
