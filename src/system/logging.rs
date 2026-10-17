//! Opt-in debug log: `PODELIS_LOG=debug podelis` writes what happens (the
//! app's events, and the database driver's) to `podelis.log` in the config
//! folder. The value is a tracing filter, e.g. `info` or `podelis=trace`.

use std::path::PathBuf;

/// Starts logging if `PODELIS_LOG` is set. Returns the log's path.
pub fn init() -> Option<PathBuf> {
    let filter = std::env::var("PODELIS_LOG").ok().filter(|v| !v.trim().is_empty())?;
    let path = crate::config::store::config_dir().join("podelis.log");
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&path).ok()?;
    let filter = tracing_subscriber::EnvFilter::try_new(&filter).ok()?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .try_init()
        .ok()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "podelis started");
    Some(path)
}
