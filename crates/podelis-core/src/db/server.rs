//! Which servers' versions Podėlis supports, and what a connected one has.
//!
//! An engine keeps one driver for all its versions; where they differ, it
//! asks its own feature table (`mysql::server`'s, say) what the connected
//! server has, rather than comparing version numbers where it needs to
//! know. What range is supported (and tested: CI's `versions` job) is each
//! engine's [`Backend::support`](super::Backend::support); the app says on
//! connecting when a server is outside it.

/// A version: major, minor, patch.
pub type Version = (u32, u32, u32);

/// The first version in `text` (`8.0.36-log` → 8.0.36, `16beta1` → 16.0.0),
/// if it starts with one.
pub fn parse_version(text: &str) -> Option<Version> {
    let mut parts = text.trim().split(|c: char| !c.is_ascii_digit()).map(|p| p.parse::<u32>().ok());
    let major = parts.next().flatten()?;
    // `16beta1`: the part after a letter isn't a minor version.
    let dotted = |n: usize| text.trim().split('.').nth(n).is_some_and(|p| p.starts_with(|c: char| c.is_ascii_digit()));
    let minor = if dotted(1) { parts.next().flatten().unwrap_or(0) } else { 0 };
    let patch = if dotted(2) { parts.next().flatten().unwrap_or(0) } else { 0 };
    Some((major, minor, patch))
}

/// The versions Podėlis supports of a server (as people name it), and of
/// those, the oldest it still tries without supporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub server: &'static str,
    /// The oldest supported, as it's written (`13`, `8.0`).
    pub from: &'static str,
    pub oldest: Version,
    /// Older ones tried, not supported (MySQL 5.7, out of support itself).
    pub best_effort: Option<Version>,
}

/// How a connected server stands against what Podėlis supports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// Older than supported, but tried (and tested in CI as such).
    BestEffort {
        range: Range,
        version: String,
    },
    /// Older than anything Podėlis tries.
    TooOld {
        range: Range,
        version: String,
    },
    /// Another server that speaks the engine's protocol (Aurora, say),
    /// not tested with.
    Compatible {
        name: String,
        version: String,
    },
}

impl Range {
    /// How `version` (as the server gave it) stands.
    pub fn check(self, version: &str) -> Support {
        let Some(v) = parse_version(version) else { return Support::Supported };
        let said = version.split_whitespace().next().unwrap_or(version).to_string();
        if v >= self.oldest {
            Support::Supported
        } else if self.best_effort.is_some_and(|b| v >= b) {
            Support::BestEffort { range: self, version: said }
        } else {
            Support::TooOld { range: self, version: said }
        }
    }
}

impl Support {
    /// What to say about it on connecting; nothing when it's supported.
    pub fn note(&self) -> Option<String> {
        match self {
            Support::Supported => None,
            Support::BestEffort { range, version } => {
                Some(t!("server.best_effort", server = range.server, version = version, from = range.from))
            }
            Support::TooOld { range, version } => {
                Some(t!("server.too_old", server = range.server, version = version, from = range.from))
            }
            Support::Compatible { name, version } => Some(t!("server.compatible", name = name, version = version)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: Range = Range { server: "PostgreSQL", from: "13", oldest: (13, 0, 0), best_effort: None };
    const MY: Range = Range { server: "MySQL", from: "8.0", oldest: (8, 0, 0), best_effort: Some((5, 7, 0)) };

    #[test]
    fn versions_are_read_as_servers_write_them() {
        assert_eq!(parse_version("8.0.36-log"), Some((8, 0, 36)));
        assert_eq!(parse_version("17.2"), Some((17, 2, 0)));
        assert_eq!(parse_version("16beta1"), Some((16, 0, 0)));
        assert_eq!(parse_version("10.11.6-MariaDB-1:10.11.6+maria~ubu2204"), Some((10, 11, 6)));
        assert_eq!(parse_version("devel"), None);
    }

    #[test]
    fn a_server_stands_by_its_version() {
        assert_eq!(PG.check("17.2"), Support::Supported);
        assert!(matches!(PG.check("12.19"), Support::TooOld { .. }));
        assert_eq!(MY.check("8.4.3"), Support::Supported);
        assert!(matches!(MY.check("5.7.44-log"), Support::BestEffort { .. }));
        assert!(matches!(MY.check("5.6.51"), Support::TooOld { .. }));
        // A version that can't be read isn't said to be anything.
        assert_eq!(PG.check(""), Support::Supported);
        assert!(MY.check("5.7.44").note().is_some_and(|n| n.contains("5.7.44") && n.contains("8.0")));
    }
}
