//! What PostgreSQL has only since some version, for saying so when a
//! statement that uses it fails on an older server.

use crate::db::Engine;
use crate::db::dialect::{TooNew, code_words, has_seq, starts_with};

/// The major version: "16.2" → 16, "9.6.24" → 9.6 as 9 (all before 10).
fn major(version: &str) -> Option<u32> {
    version.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// The first of `sql`'s features a PostgreSQL `version` hasn't got.
pub(super) fn too_new(sql: &str, version: &str) -> Option<TooNew> {
    let v = major(version)?;
    let words = code_words(sql, Engine::Postgres);
    let call = |name: &str| has_seq(&words, &[name, "("]);
    let features: [(&'static str, u32, &'static str, bool); 15] = [
        ("MERGE", 15, "PostgreSQL 15 or newer", starts_with(&words, &["merge"])),
        ("JSON_TABLE", 17, "PostgreSQL 17 or newer", call("json_table")),
        (
            "JSON_VALUE, JSON_QUERY, JSON_EXISTS",
            17,
            "PostgreSQL 17 or newer",
            call("json_value") || call("json_query") || call("json_exists"),
        ),
        (
            "IS JSON",
            16,
            "PostgreSQL 16 or newer",
            has_seq(&words, &["is", "json"]) || has_seq(&words, &["is", "not", "json"]),
        ),
        ("ANY_VALUE", 16, "PostgreSQL 16 or newer", call("any_value")),
        // (json_object(text[]) is older: only JSON_ARRAY is new.)
        ("JSON_ARRAY", 16, "PostgreSQL 16 or newer", call("json_array")),
        ("NULLS NOT DISTINCT", 15, "PostgreSQL 15 or newer", has_seq(&words, &["nulls", "not", "distinct"])),
        (
            "SEARCH and CYCLE in WITH RECURSIVE",
            14,
            "PostgreSQL 14 or newer",
            has_seq(&words, &["search", "depth", "first"]) || has_seq(&words, &["search", "breadth", "first"]),
        ),
        (
            "CREATE OR REPLACE TRIGGER",
            14,
            "PostgreSQL 14 or newer",
            has_seq(&words, &["create", "or", "replace", "trigger"]),
        ),
        ("FETCH FIRST … WITH TIES", 13, "PostgreSQL 13 or newer", has_seq(&words, &["with", "ties"])),
        ("gen_random_uuid()", 13, "PostgreSQL 13 or newer (or the pgcrypto extension)", call("gen_random_uuid")),
        (
            "generated columns (GENERATED ALWAYS AS (…) STORED)",
            12,
            "PostgreSQL 12 or newer",
            has_seq(&words, &["generated", "always", "as", "("]),
        ),
        (
            "procedures (CALL, CREATE PROCEDURE)",
            11,
            "PostgreSQL 11 or newer",
            starts_with(&words, &["call"])
                || starts_with(&words, &["create", "procedure"])
                || starts_with(&words, &["create", "or", "replace", "procedure"]),
        ),
        (
            "identity columns (GENERATED … AS IDENTITY)",
            10,
            "PostgreSQL 10 or newer",
            has_seq(&words, &["as", "identity"]),
        ),
        ("ON CONFLICT", 10, "PostgreSQL 9.5 or newer", has_seq(&words, &["on", "conflict"]) && minor_before(version, 9, 5)),
    ];
    let (feature, _, needs, _) = features.into_iter().find(|(_, since, _, used)| *used && v < *since)?;
    Some(TooNew { feature, needs, server: format!("PostgreSQL {version}") })
}

/// Whether a 9.x `version` is before `major.minor` (ON CONFLICT came in 9.5).
fn minor_before(version: &str, major: u32, minor: u32) -> bool {
    let mut parts = version.split(|c: char| !c.is_ascii_digit()).map(|p| p.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(a), Some(b)) => (a, b) < (major, minor),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::too_new;

    #[test]
    fn what_an_old_postgres_hasnt_got() {
        let said = too_new("merge into t using s on t.id = s.id when matched then delete", "14.11").unwrap();
        assert_eq!((said.feature, said.server.as_str()), ("MERGE", "PostgreSQL 14.11"));
        assert!(too_new("merge into t using s on t.id = s.id when matched then delete", "15.6").is_none());
        assert_eq!(too_new("select any_value(x) from t", "15.6").unwrap().feature, "ANY_VALUE");
        assert!(too_new("insert into t values (1) on conflict do nothing", "9.4.26").is_some());
        assert!(too_new("insert into t values (1) on conflict do nothing", "9.6.24").is_none());
        assert!(too_new("select * from t fetch first 3 rows with ties", "12.18").is_some());
        assert!(too_new("select 'merge'", "9.6").is_none());
        // The old trigger syntax isn't a procedure; json_object(text[]) is old.
        assert!(too_new("create trigger t before insert on x for each row execute procedure f()", "10.23").is_none());
        assert!(too_new("create procedure p() language sql as $$ select 1 $$", "10.23").is_some());
        assert!(too_new("select json_object('{a,1}')", "14.11").is_none());
        assert!(too_new("select 1", "").is_none());
    }
}
