//! Podėlis's lower layers: the databases (`db`, ports and adapters, one
//! folder per engine), SQL text and its analysis without a connection
//! (`sql`), and the words it says (`lang`, with `t!` and `tn!`). Nothing
//! here knows of the app, the gateway or the command line.

// Built with fewer database engines than all of them (Cargo features), code
// only the others use goes unused, and with one engine its own pool can't
// be another's: that's expected there. `every_engine` is build.rs's, from
// the engines it finds, so no list of them here.
#![cfg_attr(
    not(every_engine),
    allow(
        dead_code,
        unused_imports,
        irrefutable_let_patterns,
        clippy::allow_attributes,
        reason = "which of these fire depends on the engines built, so not `expect`"
    )
)]

#[macro_use]
pub mod lang;
pub mod db;
pub mod sql;
