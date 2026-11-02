//! What MySQL has only since some version, for saying so when a statement
//! that uses it fails on an older server (where it's "an error in your SQL
//! syntax", or a function that doesn't exist).

use super::Server;
use crate::db::Engine;
use crate::db::dialect::{TooNew, code_words, has_seq, starts_with};

type Version = (u32, u32, u32);

/// The first of `sql`'s features a MySQL `version` hasn't got.
pub(super) fn too_new(sql: &str, version: &str) -> Option<TooNew> {
    // Only a version that reads as MySQL's or MariaDB's own: `5.7.44`,
    // `8.0.36-log`, `10.11.6-MariaDB`. Aurora (`8.0.mysql_aurora.3.04.0`),
    // TiDB and the like have features of their own versions.
    let (number, rest) = version.split_once(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or((version, ""));
    let plain = number.split('.').filter(|p| !p.is_empty()).count() == 3
        && (rest.is_empty() || version[number.len()..].starts_with('-'))
        && !version.to_ascii_lowercase().contains("tidb");
    if !plain {
        return None;
    }
    let words = code_words(sql, Engine::MySql);
    let call = |name: &str| has_seq(&words, &[name, "("]);
    // MariaDB's ANALYZE <statement> isn't MySQL's in any version.
    let analyze_statement = ["select", "update", "delete", "insert", "replace", "with"]
        .iter()
        .any(|w| starts_with(&words, &["analyze", w]));
    let server = Server::of(version);
    if analyze_statement && matches!(server, Server::MySql(_)) {
        return Some(TooNew {
            feature: "ANALYZE <statement>",
            needs: "MariaDB (MySQL 8.0.18 or newer writes it EXPLAIN ANALYZE, Shift+F7)",
            server: format!("MySQL {version}"),
        });
    }
    let Server::MySql(v) = server else { return None };
    let features: [(&'static str, Version, &'static str, bool); 17] = [
        ("EXPLAIN ANALYZE", (8, 0, 18), "MySQL 8.0.18 or newer", starts_with(&words, &["explain", "analyze"])),
        (
            "EXPLAIN FORMAT=TREE",
            (8, 0, 16),
            "MySQL 8.0.16 or newer",
            has_seq(&words, &["format", "=", "tree"]),
        ),
        ("WITH (a common table expression)", (8, 0, 1), "MySQL 8.0 or newer", starts_with(&words, &["with"])),
        ("window functions (OVER)", (8, 0, 2), "MySQL 8.0 or newer", has_seq(&words, &["over", "("])),
        // As keywords, not a column of that name: before `(`, after FOR
        // UPDATE / SHARE, before a query.
        ("LATERAL", (8, 0, 14), "MySQL 8.0.14 or newer", has_seq(&words, &["lateral", "("])),
        ("JSON_TABLE", (8, 0, 4), "MySQL 8.0.4 or newer", call("json_table")),
        (
            "INTERSECT and EXCEPT",
            (8, 0, 31),
            "MySQL 8.0.31 or newer",
            ["intersect", "except"].iter().any(|w| {
                ["select", "(", "all", "distinct", "table", "values"].iter().any(|next| has_seq(&words, &[w, next]))
            }),
        ),
        ("VALUES ROW(…)", (8, 0, 19), "MySQL 8.0.19 or newer", has_seq(&words, &["values", "row", "("])),
        ("the TABLE statement", (8, 0, 19), "MySQL 8.0.19 or newer", starts_with(&words, &["table"])),
        ("RENAME COLUMN", (8, 0, 0), "MySQL 8.0 or newer", has_seq(&words, &["rename", "column"])),
        (
            "SKIP LOCKED and NOWAIT",
            (8, 0, 1),
            "MySQL 8.0 or newer",
            has_seq(&words, &["skip", "locked"])
                || has_seq(&words, &["update", "nowait"])
                || has_seq(&words, &["share", "nowait"]),
        ),
        ("roles", (8, 0, 0), "MySQL 8.0 or newer", has_seq(&words, &["create", "role"])),
        (
            "REGEXP_LIKE, REGEXP_REPLACE, REGEXP_SUBSTR, REGEXP_INSTR",
            (8, 0, 4),
            "MySQL 8.0.4 or newer",
            ["regexp_like", "regexp_replace", "regexp_substr", "regexp_instr"].iter().any(|f| call(f)),
        ),
        ("JSON_VALUE", (8, 0, 21), "MySQL 8.0.21 or newer", call("json_value")),
        (
            "JSON_ARRAYAGG and JSON_OBJECTAGG",
            (5, 7, 22),
            "MySQL 5.7.22 or newer",
            call("json_arrayagg") || call("json_objectagg"),
        ),
        (
            "UUID_TO_BIN and BIN_TO_UUID",
            (8, 0, 0),
            "MySQL 8.0 or newer",
            call("uuid_to_bin") || call("bin_to_uuid"),
        ),
        ("GROUPING()", (8, 0, 1), "MySQL 8.0 or newer", call("grouping")),
    ];
    let (feature, _, needs, _) = features.into_iter().find(|(_, since, _, used)| *used && v < *since)?;
    Some(TooNew { feature, needs, server: format!("MySQL {version}") })
}

#[cfg(test)]
mod tests {
    use super::too_new;

    #[test]
    fn what_an_old_mysql_hasnt_got() {
        let said = too_new("explain analyze select * from t", "5.7.44").unwrap();
        assert_eq!((said.feature, said.server.as_str()), ("EXPLAIN ANALYZE", "MySQL 5.7.44"));
        assert!(too_new("explain analyze select * from t", "8.0.18").is_none());
        assert_eq!(too_new("analyze select 1", "8.4.0").unwrap().feature, "ANALYZE <statement>");
        assert!(too_new("analyze table t", "5.7.44").is_none(), "ANALYZE TABLE is MySQL's");
        assert!(too_new("analyze select 1", "10.11.6-MariaDB").is_none());
        assert_eq!(too_new("with c as (select 1) select * from c", "5.7.44").unwrap().feature.split(' ').next(), Some("WITH"));
        assert!(too_new("select row_number() over (order by id) from t", "5.7.44").is_some());
        assert!(too_new("select 1 intersect select 1", "8.0.30").is_some());
        assert!(too_new("select 1 intersect select 1", "8.0.31").is_none());
        // In a string or a comment it isn't used.
        assert!(too_new("select 'with lateral' -- over (\nfrom t", "5.6.51").is_none());
        // Columns that only share a keyword's name.
        assert!(too_new("select lateral, nowait, except from jobs whre id = 1", "5.7.44").is_none());
        assert!(too_new("select * from t for update nowait", "5.7.44").is_some());
        // Versions that aren't plain MySQL's say nothing.
        assert!(too_new("explain analyze select 1", "8.0.mysql_aurora.3.04.0").is_none());
        assert!(too_new("explain analyze select 1", "5.7.25-TiDB-v7.5.0").is_none());
        assert!(too_new("explain analyze select 1", "5.7.44-log").is_some());
        // After another statement.
        assert!(too_new("set @a = 1; with c as (select 1) select * from c", "5.7.44").is_some());
    }
}
