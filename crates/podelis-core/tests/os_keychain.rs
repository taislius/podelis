//! `use_os_keychain` really leaves the tests' in-memory keychain: in a
//! process of its own (this test binary), as switching in the shared one
//! would send every other test's passwords to the real keychain. It only
//! reads an account that doesn't exist there.

// The in-memory keychain exists only with `test-support` (as when the
// workspace's tests are built).
#![cfg(feature = "test-support")]

use podelis_core::db::keychain::{get_password, store_password, use_os_keychain};

#[test]
fn the_program_leaves_the_in_memory_keychain() {
    // Asked to use the real keychain throughout: nothing to compare.
    if std::env::var("PODELIS_TEST_KEYCHAIN").as_deref() == Ok("1") {
        return;
    }
    let account =
        format!("podelis-test-{}-{}", std::process::id(), std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos());
    store_password(&account, "in memory").unwrap();
    assert_eq!(get_password(&account).unwrap(), "in memory");
    use_os_keychain();
    // The OS keychain hasn't it (or there's none here, as on CI's Linux).
    assert_ne!(get_password(&account).ok().as_deref(), Some("in memory"));
}
