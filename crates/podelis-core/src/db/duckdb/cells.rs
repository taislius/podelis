//! DuckDB's values as text, from the Arrow batches its results come in:
//! Arrow's own formatting, which knows every type (lists, structs, maps,
//! intervals…), and each column's type by DuckDB's name for it.

use ::duckdb::arrow::array::{Array, RecordBatch};
use ::duckdb::arrow::datatypes::{DataType, Schema, TimeUnit};
use ::duckdb::arrow::util::display::array_value_to_string;

/// Row `row` of `array` as text; `None` for NULL.
pub(super) fn cell(array: &dyn Array, row: usize) -> Option<String> {
    if array.is_null(row) {
        return None;
    }
    array_value_to_string(array, row).ok()
}

/// Row `row` of `batch`, each column's value as text.
pub(super) fn row(batch: &RecordBatch, row: usize) -> Vec<Option<String>> {
    batch.columns().iter().map(|column| cell(column.as_ref(), row)).collect()
}

/// (name, type) of each column, the type as DuckDB names it.
pub(super) fn columns(schema: &Schema) -> Vec<(String, String)> {
    schema.fields().iter().map(|f| (f.name().clone(), type_name(f.data_type()))).collect()
}

/// What DuckDB calls the type it hands over as `data_type`.
fn type_name(data_type: &DataType) -> String {
    match data_type {
        DataType::Null => "NULL".into(),
        DataType::Boolean => "BOOLEAN".into(),
        DataType::Int8 => "TINYINT".into(),
        DataType::Int16 => "SMALLINT".into(),
        DataType::Int32 => "INTEGER".into(),
        DataType::Int64 => "BIGINT".into(),
        DataType::UInt8 => "UTINYINT".into(),
        DataType::UInt16 => "USMALLINT".into(),
        DataType::UInt32 => "UINTEGER".into(),
        DataType::UInt64 => "UBIGINT".into(),
        DataType::Float16 | DataType::Float32 => "FLOAT".into(),
        DataType::Float64 => "DOUBLE".into(),
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => format!("DECIMAL({p},{s})"),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "VARCHAR".into(),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => "BLOB".into(),
        DataType::FixedSizeBinary(16) => "UUID".into(),
        DataType::Date32 | DataType::Date64 => "DATE".into(),
        DataType::Time32(_) | DataType::Time64(_) => "TIME".into(),
        DataType::Timestamp(_, Some(_)) => "TIMESTAMP WITH TIME ZONE".into(),
        DataType::Timestamp(TimeUnit::Second, None) => "TIMESTAMP_S".into(),
        DataType::Timestamp(TimeUnit::Millisecond, None) => "TIMESTAMP_MS".into(),
        DataType::Timestamp(TimeUnit::Nanosecond, None) => "TIMESTAMP_NS".into(),
        DataType::Timestamp(_, None) => "TIMESTAMP".into(),
        DataType::Interval(_) | DataType::Duration(_) => "INTERVAL".into(),
        DataType::List(f) | DataType::LargeList(f) | DataType::ListView(f) => format!("{}[]", type_name(f.data_type())),
        DataType::FixedSizeList(f, n) => format!("{}[{n}]", type_name(f.data_type())),
        DataType::Struct(_) => "STRUCT".into(),
        DataType::Map(..) => "MAP".into(),
        DataType::Union(..) => "UNION".into(),
        DataType::Dictionary(_, values) => type_name(values),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_by_duckdb_names() {
        use std::sync::Arc;
        use ::duckdb::arrow::datatypes::Field;
        assert_eq!(type_name(&DataType::Int32), "INTEGER");
        assert_eq!(type_name(&DataType::Decimal128(10, 2)), "DECIMAL(10,2)");
        assert_eq!(type_name(&DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))), "TIMESTAMP WITH TIME ZONE");
        let list = DataType::List(Arc::new(Field::new("item", DataType::Int64, true)));
        assert_eq!(type_name(&list), "BIGINT[]");
    }
}
