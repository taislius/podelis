use super::*;

/// (schema, table, column, type, nullable, "PRI" for primary key columns)
pub(super) type ColumnRow = (String, String, String, String, String, String);

/// (schema, table, type, estimated rows, estimated bytes)
pub(super) type TableRow = (String, String, String, Option<i64>, Option<i64>);

impl Database {
    /// Every table and view with its columns, in two queries (for the
    /// schema browser and autocomplete). `system` adds the database's own
    /// catalog tables.
    pub async fn catalog(&self, system: bool) -> Result<Vec<(TableInfo, Vec<ColumnInfo>)>> {
        let tables = self.tables(system).await?;
        let rows: Vec<ColumnRow> = on_driver!(self.0.engine, engine::queries::column_rows(self, system).await?);
        let mut by_table: std::collections::HashMap<(String, String), Vec<ColumnInfo>> =
            std::collections::HashMap::default();
        for (schema, table, name, data_type, nullable, key) in rows {
            by_table.entry((schema, table)).or_default().push(ColumnInfo {
                primary_key: key == "PRI",
                name,
                data_type,
                nullable: nullable == "YES",
            });
        }
        Ok(tables
            .into_iter()
            .map(|t| {
                let cols = by_table.remove(&(t.schema.clone(), t.name.clone())).unwrap_or_default();
                (t, cols)
            })
            .collect())
    }

    pub async fn tables(&self, system: bool) -> Result<Vec<TableInfo>> {
        let rows: Vec<TableRow> = on_driver!(self.0.engine, engine::queries::table_rows(self, system).await?);
        let dialect = self.0.engine.dialect();
        Ok(rows
            .into_iter()
            .map(|(schema, name, kind, rows, bytes)| TableInfo {
                system: dialect.is_system_table(&schema, &name),
                schema,
                name,
                is_view: kind.to_ascii_uppercase().contains("VIEW"),
                // Postgres reports -1 for tables never analyzed.
                rows: rows.filter(|n| *n >= 0),
                bytes: bytes.filter(|n| *n >= 0),
            })
            .collect())
    }

    #[cfg(test)]
    pub async fn columns(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
        let rows: Vec<(String, String, String)> =
            on_driver!(self.0.engine, engine::queries::column_info_rows(self, schema, table).await?);
        Ok(rows
            .into_iter()
            .map(|(name, data_type, nullable)| ColumnInfo {
                name,
                data_type,
                nullable: nullable == "YES",
                primary_key: false,
            })
            .collect())
    }
}
