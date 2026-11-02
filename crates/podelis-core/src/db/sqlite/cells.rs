//! Decoding SQLite values as text.

use super::super::*;
use sqlx::sqlite::SqliteRow;

/// Formats a SQLite value by its storage class.
pub(in crate::db) fn sqlite_cell(row: &SqliteRow, idx: usize) -> Option<String> {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Some("<error>".to_string());
    };
    if raw.is_null() {
        return None;
    }
    let ty = raw.type_info().name().to_string();
    Some(match ty.as_str() {
        "INTEGER" => row.try_get_unchecked::<i64, _>(idx).map(|v| v.to_string()).unwrap_or_default(),
        "REAL" => row.try_get_unchecked::<f64, _>(idx).map(|v| v.to_string()).unwrap_or_default(),
        "BLOB" => {
            let bytes: Vec<u8> = row.try_get_unchecked(idx).unwrap_or_default();
            Engine::Sqlite.dialect().hex(&bytes)
        }
        _ => row.try_get_unchecked::<String, _>(idx).unwrap_or_default(),
    })
}
