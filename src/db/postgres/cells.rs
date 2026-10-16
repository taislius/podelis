//! Decoding Postgres values as text.

use super::super::*;
use sqlx::postgres::{PgRow, PgValueFormat};

macro_rules! try_decode {
    ($row:expr, $idx:expr, $($ty:ty),+ $(,)?) => {
        $(
            if let Ok(v) = $row.try_get::<Option<$ty>, _>($idx) {
                return v.map(|v| v.to_string());
            }
        )+
    };
}

/// Formats a Postgres value. `raw_sql` uses the simple query protocol, so
/// values arrive in the server's text format — exactly what `psql` shows,
/// for every type including enums, arrays, intervals and ranges.
pub(in crate::db) fn pg_cell(row: &PgRow, idx: usize) -> Option<String> {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Some("<error>".to_string());
    };
    if raw.is_null() {
        return None;
    }
    if raw.format() == PgValueFormat::Text {
        let text = raw.as_str().unwrap_or("<invalid utf-8>");
        return Some(match raw.type_info().name() {
            "BOOL" => match text {
                "t" => "true".to_string(),
                "f" => "false".to_string(),
                other => other.to_string(),
            },
            _ => text.to_string(),
        });
    }
    pg_binary_cell(row, idx)
}

pub(in crate::db) fn pg_binary_cell(row: &PgRow, idx: usize) -> Option<String> {
    try_decode!(
        row,
        idx,
        String,
        i64,
        i32,
        i16,
        f64,
        f32,
        bool,
        Decimal,
        Uuid,
        chrono::NaiveDateTime,
        chrono::NaiveDate,
        chrono::NaiveTime,
        chrono::DateTime<chrono::Utc>,
        serde_json::Value,
    );
    if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return v.map(|b| Engine::Postgres.dialect().hex(&b));
    }
    Some("<unsupported>".to_string())
}
