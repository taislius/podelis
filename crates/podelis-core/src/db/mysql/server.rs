//! Which server answers for MySQL (MySQL itself, MariaDB, or another that
//! speaks its protocol), and what that one has: the engine asks
//! [`Server::has`] where versions differ, and the table below is the one
//! place that says from which version each one has what.

use crate::db::server::{Range, Support, Version, parse_version};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Server {
    MySql(Version),
    MariaDb(Version),
    /// Another server with a MySQL-like version (Aurora, TiDB…): taken to
    /// have what that MySQL version has.
    Compatible { name: &'static str, mysql: Version },
}

/// What differs between MySQL's versions (and MariaDB's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Feature {
    /// `EXPLAIN FORMAT=TREE`.
    ExplainTree,
    /// `EXPLAIN ANALYZE` (MariaDB: `ANALYZE <statement>`).
    ExplainAnalyze,
    /// `EXPLAIN FORMAT=TRADITIONAL` by that name (`explain_format` can make
    /// a bare EXPLAIN another format from then).
    ExplainTraditional,
    /// CHECK constraints kept and enforced, in `information_schema`.
    CheckConstraints,
}

/// From which MySQL and which MariaDB version each feature is there;
/// `None`: never.
const FEATURES: &[(Feature, Option<Version>, Option<Version>)] = &[
    (Feature::ExplainTree, Some((8, 0, 16)), None),
    (Feature::ExplainAnalyze, Some((8, 0, 18)), Some((10, 1, 0))),
    (Feature::ExplainTraditional, Some((8, 0, 32)), None),
    (Feature::CheckConstraints, Some((8, 0, 16)), Some((10, 2, 1))),
];

/// Servers that say they're MySQL, by a word in their version.
const COMPATIBLE: &[(&str, &str)] = &[("mysql_aurora", "Aurora MySQL"), ("tidb", "TiDB")];

/// What's supported: MySQL 8.0 on (5.7, out of support since 2023-10,
/// tried), MariaDB 10.6 on (the oldest of its long-term versions still
/// supported when this was written).
const MYSQL: Range = Range { server: "MySQL", from: "8.0", oldest: (8, 0, 0), best_effort: Some((5, 7, 0)) };
const MARIADB: Range = Range { server: "MariaDB", from: "10.6", oldest: (10, 6, 0), best_effort: None };

impl Server {
    /// The server by its `VERSION()`; one that can't be read is taken to
    /// be a current MySQL.
    pub(super) fn of(version: &str) -> Self {
        let lower = version.to_ascii_lowercase();
        let number = parse_version(version).unwrap_or((u32::MAX, 0, 0));
        if lower.contains("mariadb") {
            // `5.5.5-10.6.12-MariaDB`: before 11, some say MySQL 5.5.5 first.
            let own = lower.strip_prefix("5.5.5-").and_then(parse_version).unwrap_or(number);
            return Server::MariaDb(own);
        }
        match COMPATIBLE.iter().find(|(word, _)| lower.contains(word)) {
            Some(&(_, name)) => Server::Compatible { name, mysql: number },
            None => Server::MySql(number),
        }
    }

    /// Whether it has `feature`.
    pub(super) fn has(&self, feature: Feature) -> bool {
        let Some(&(_, mysql, mariadb)) = FEATURES.iter().find(|(f, _, _)| *f == feature) else { return false };
        match self {
            Server::MySql(v) | Server::Compatible { mysql: v, .. } => mysql.is_some_and(|from| *v >= from),
            Server::MariaDb(v) => mariadb.is_some_and(|from| *v >= from),
        }
    }

    pub(super) fn is_mariadb(&self) -> bool {
        matches!(self, Server::MariaDb(_))
    }

    /// How it stands against what's supported.
    pub(super) fn support(&self, version: &str) -> Support {
        match self {
            Server::MySql(_) => MYSQL.check(version),
            Server::MariaDb(v) => MARIADB.check(&format!("{}.{}.{}", v.0, v.1, v.2)),
            Server::Compatible { name, .. } => {
                Support::Compatible { name: (*name).to_string(), version: version.trim().to_string() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_server_has_what_its_version_has() {
        let my = |v| Server::of(v);
        assert!(my("8.0.36").has(Feature::ExplainTree) && my("8.0.36").has(Feature::ExplainAnalyze));
        assert!(!my("8.0.15").has(Feature::ExplainTree) && !my("5.7.44-log").has(Feature::CheckConstraints));
        assert!(!my("8.0.31").has(Feature::ExplainTraditional) && my("8.4.3").has(Feature::ExplainTraditional));
        let maria = my("10.11.6-MariaDB-1:10.11.6+maria~ubu2204");
        assert_eq!(maria, Server::MariaDb((10, 11, 6)));
        assert!(maria.has(Feature::ExplainAnalyze) && !maria.has(Feature::ExplainTree));
        assert_eq!(my("5.5.5-10.6.12-MariaDB"), Server::MariaDb((10, 6, 12)));
        assert!(!my("5.5.5-10.1.48-MariaDB").has(Feature::CheckConstraints));
        // Aurora and TiDB: what their MySQL version has.
        // As the version query writes Aurora's (its MySQL's, then its own).
        let aurora = my("8.0.28-mysql_aurora.3.04.0");
        assert_eq!(aurora, Server::Compatible { name: "Aurora MySQL", mysql: (8, 0, 28) });
        assert!(aurora.has(Feature::ExplainTree), "what MySQL 8.0.28 has");
        // Unreadable: a current MySQL.
        assert!(my("").has(Feature::ExplainTree));
    }

    #[test]
    fn what_is_supported_and_what_isnt() {
        assert_eq!(Server::of("8.4.3").support("8.4.3"), Support::Supported);
        assert!(matches!(Server::of("5.7.44").support("5.7.44"), Support::BestEffort { .. }));
        let old_maria = "10.4.34-MariaDB";
        assert!(matches!(Server::of(old_maria).support(old_maria), Support::TooOld { .. }));
        let tidb = "8.0.11-TiDB-v7.5.0";
        assert!(matches!(Server::of(tidb).support(tidb), Support::Compatible { .. }));
    }
}
