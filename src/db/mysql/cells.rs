//! Decoding MySQL values as text.

use super::super::*;

/// Formats a MySQL value. `raw_sql` uses the text protocol, so every value
/// is its textual form; binary data is shown in hex (`0xDEADBEEF`), as is
/// text that isn't UTF-8 or has control characters.
pub(in crate::db) fn mysql_cell(row: &MySqlRow, idx: usize) -> Option<String> {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Some("<error>".to_string());
    };
    if raw.is_null() {
        return None;
    }
    let ty = raw.type_info().name().to_string();
    let is_bit = ty == "BIT";
    let bytes: Vec<u8> = row.try_get_unchecked(idx).unwrap_or_default();
    let dialect = Engine::MySql.dialect();
    if dialect.is_binary_type(&ty) {
        return Some(dialect.hex(&bytes));
    }
    if is_bit {
        let n = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        return Some(n.to_string());
    }
    Some(match String::from_utf8(bytes) {
        Ok(s) if !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r') => s,
        Ok(s) => dialect.hex(s.as_bytes()),
        Err(e) => dialect.hex(e.as_bytes()),
    })
}
