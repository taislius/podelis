//! Editing a result cell: `c` on a cell of a plain single-table SELECT opens
//! a prompt; Enter saves it with `UPDATE … WHERE <primary key> = …`.

use super::*;

impl App {
    /// Opens the edit prompt for the selected cell, or explains why the
    /// result can't be edited.
    pub(super) fn start_cell_edit(&mut self) {
        match self.cell_edit_target() {
            Ok(edit) => self.cell_edit = Some(edit),
            Err(why) => self.set_status(t!("edit.cant", why = why), true),
        }
    }

    fn cell_edit_target(&self) -> Result<CellEdit, String> {
        if self.connection.mode == ConnectionMode::ReadOnly {
            return Err(t!("rows.read_only").into());
        }
        let results = self.tab.grid.results.as_ref().ok_or(t!("rows.no_results"))?;
        let row = *self
            .tab
            .grid
            .view
            .get(self.tab.grid.table_state.selected().ok_or(t!("follow.no_row_selected"))?)
            .ok_or(t!("follow.no_row_selected"))?;
        let col = self.tab.grid.selected_col;
        let (sql, _) = self.tab.last_sql.as_ref().ok_or(t!("rows.no_query"))?;
        let source = analyze::edit_source(&parse(sql, self.engine())).ok_or(t!("rows.not_editable"))?;

        // The table in the catalog: by schema if the query named one.
        let catalog = self.connection.schema.as_ref().ok_or(t!("rows.schema_loading"))?;
        let mut matches = catalog
            .tables
            .iter()
            .filter(|t| t.info.name == source.table && source.schema.as_ref().is_none_or(|s| *s == t.info.schema));
        let table = matches.next().ok_or_else(|| t!("rows.table_not_found", table = source.table))?;
        if matches.next().is_some() {
            return Err(t!("rows.ambiguous_table", table = source.table));
        }
        if table.info.is_view {
            return Err(t!("rows.is_view").into());
        }
        let selectable = |name: &str| source.columns.as_ref().is_none_or(|cols| cols.iter().any(|c| c == name));
        let position = |name: &str| results.columns.iter().position(|c| c == name);

        let column = &results.columns[col];
        if !selectable(column) {
            return Err(t!("edit.computed", column = column));
        }
        let info = table
            .columns
            .iter()
            .find(|c| c.name == *column)
            .ok_or_else(|| t!("edit.not_a_column", column = column, table = source.table))?;

        let pk: Vec<&ColumnInfo> = table.columns.iter().filter(|c| c.primary_key).collect();
        if pk.is_empty() {
            return Err(t!("rows.no_primary_key", table = source.table));
        }
        let engine = self.engine();
        let ty = |i: usize| results.column_types.get(i).map_or("", |t| t);
        let mut key = Vec::new();
        for k in pk {
            let i = position(&k.name)
                .filter(|_| selectable(&k.name))
                .ok_or_else(|| t!("rows.key_not_in_result", column = k.name))?;
            let value = results.rows[row][i].as_deref().ok_or(t!("rows.key_is_null"))?;
            key.push((k.name.clone(), crate::db::value_literal(value, ty(i), engine)));
        }
        let table_name = match &source.schema {
            Some(s) => format!("{}.{}", engine.quote_ident(s), engine.quote_ident(&table.info.name)),
            None => engine.quote_ident(&table.info.name),
        };
        Ok(CellEdit {
            row,
            col,
            column: column.clone(),
            table: table_name,
            key,
            column_type: ty(col).to_string(),
            nullable: info.nullable,
            text: LineInput::new(results.rows[row][col].clone().unwrap_or_default()),
        })
    }

    pub(super) fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(edit) = &mut self.cell_edit else { return };
        if key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if edit.nullable {
                self.save_cell_edit(None);
            } else {
                let msg = t!("edit.not_null", column = edit.column);
                self.set_status(msg, true);
            }
            return;
        }
        match edit.text.handle(key) {
            InputResult::Cancel => self.cell_edit = None,
            InputResult::Submit => {
                let text = edit.text.text().to_string();
                self.save_cell_edit(Some(text));
            }
            InputResult::Changed | InputResult::Moved | InputResult::Ignored => {}
        }
    }

    /// Runs the UPDATE through the normal safety checks (production asks).
    fn save_cell_edit(&mut self, value: Option<String>) {
        if self.conn_busy() {
            return self.set_status(t!("edit.wait_for_query").to_string(), true);
        }
        let Some(edit) = self.cell_edit.take() else { return };
        let engine = self.engine();
        let literal = |v: Option<&str>| {
            v.map_or_else(|| "NULL".to_string(), |v| crate::db::value_literal(v, &edit.column_type, engine))
        };
        let condition: Vec<String> = edit.key.iter().map(|(k, v)| format!("{} = {v}", engine.quote_ident(k))).collect();
        let sql = format!(
            "UPDATE {} SET {} = {} WHERE {}",
            edit.table,
            engine.quote_ident(&edit.column),
            literal(value.as_deref()),
            condition.join(" AND ")
        );
        self.execute_with(sql, 0, RunPurpose::CellEdit { row: edit.row, col: edit.col, value });
    }

    /// After the UPDATE: show the new value if exactly one row changed.
    pub(super) fn apply_cell_edit(&mut self, outcome: &QueryOutcome, row: usize, col: usize, value: Option<String>) {
        match outcome.rows_affected {
            Some(1) => {
                if let Some(results) = &mut self.tab.grid.results
                    && let Some(cell) = results.rows.get_mut(row).and_then(|r| r.get_mut(col))
                {
                    *cell = value;
                    self.tab.grid.col_widths = column_widths(results);
                }
                self.log(t!("edit.updated").to_string(), false);
                self.set_status(t!("edit.saved").to_string(), false);
            }
            Some(0) => {
                let msg = t!("edit.not_updated").to_string();
                self.log(msg.clone(), true);
                self.set_status(msg, true);
            }
            n => {
                let msg = t!("edit.updated_many", n = n.unwrap_or(0));
                self.log(msg.clone(), true);
                self.set_status(msg, true);
            }
        }
    }
}
