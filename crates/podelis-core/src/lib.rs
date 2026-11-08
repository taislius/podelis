//! Podėlis's lower layers: the databases (`db`, ports and adapters, one
//! folder per engine), SQL text and its analysis without a connection
//! (`sql`), and the words it says (`i18n`, with `t!` and `tn!`). Nothing
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
pub mod i18n;
pub mod db;
pub mod language;
pub mod sql;

/// Snapshot tests keep their `.snap` files in the crate's
/// `testdata/snapshots/`, not beside the code: hold this while asserting.
#[cfg(test)]
pub(crate) fn snapshots_in_testdata() -> insta::internals::SettingsBindDropGuard {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/snapshots"));
    settings.bind_to_scope()
}
