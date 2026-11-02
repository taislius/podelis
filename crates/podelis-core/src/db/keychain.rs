//! The OS keychain (macOS Keychain, the Secret Service elsewhere): saved
//! connections' passwords and models' keys, under the service `podelis`.
//! Tests (this crate's, and the app's through `test-support`) keep them in
//! memory instead, unless `PODELIS_TEST_KEYCHAIN=1`.

use anyhow::{Context, Result};

const KEYRING_SERVICE: &str = "podelis";

fn keyring_entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(|e| match e {
        // keyring's wording for "there's no Secret Service to talk to".
        // Every Unix but macOS (Linux, the BSDs) uses the Secret Service.
        keyring::Error::NoDefaultStore if !cfg!(target_os = "macos") => {
            anyhow::anyhow!(
                "there's no OS keychain here: no Secret Service is running (GNOME Keyring, KeePassXC, KWallet)"
            )
        }
        keyring::Error::NoDefaultStore => anyhow::anyhow!("there's no OS keychain here"),
        e => anyhow::Error::new(e).context("failed to access OS keychain"),
    })
}

pub fn set_password(account: &str, password: &str) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(fake) = fake_keychain() {
        fake.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(account.to_string(), password.to_string());
        return Ok(());
    }
    keyring_entry(account)?.set_password(password).context("failed to store password in OS keychain")
}

/// Tests keep passwords in memory, away from the real keychain (which may
/// also prompt), unless `PODELIS_TEST_KEYCHAIN=1` asks for it.
#[cfg(any(test, feature = "test-support"))]
fn fake_keychain() -> Option<&'static std::sync::Mutex<std::collections::HashMap<String, String>>> {
    static FAKE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        std::sync::OnceLock::new();
    (std::env::var("PODELIS_TEST_KEYCHAIN").as_deref() != Ok("1")).then(|| FAKE.get_or_init(Default::default))
}

/// The stored password; an empty string if there is none.
pub fn get_password(account: &str) -> Result<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(fake) = fake_keychain() {
        return Ok(fake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account)
            .cloned()
            .unwrap_or_default());
    }
    match keyring_entry(account)?.get_password() {
        Ok(p) => Ok(p),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(anyhow::Error::new(e).context("failed to read password from OS keychain")),
    }
}

/// Deletes the stored password; succeeds if there wasn't one.
pub fn delete_password(account: &str) -> Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(fake) = fake_keychain() {
        fake.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(account);
        return Ok(());
    }
    match keyring_entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::Error::new(e).context("failed to delete password from OS keychain")),
    }
}

/// Stores `password` (or removes the entry if it's empty).
pub fn store_password(account: &str, password: &str) -> Result<()> {
    if password.is_empty() { delete_password(account) } else { set_password(account, password) }
}
