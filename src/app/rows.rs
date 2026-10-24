//! Adding and deleting result rows (`a`, `Space` to mark, `d`), and copying
//! rows as a Markdown table (`C`). Like cell edits, these work on results of
//! a plain single-table SELECT and go through the usual safety checks.

use std::collections::BTreeSet;

use super::pipes::OpenFormat;
use super::*;

/// The table behind the current result, as far as editing goes.
pub(super) struct EditTable {
    /// Quoted, possibly schema-qualified name.
    pub sql_name: String,
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    /// Primary key columns and where they are in the result (if they are).
    pub key: Vec<(String, Option<usize>)>,
}

/// The add-row form: one field per column of the table.
pub struct RowForm {
    pub table: String,
    table_sql: String,
    pub columns: Vec<ColumnInfo>,
    pub values: Vec<String>,
    pub field: usize,
}

const MARKDOWN_MAX_ROWS: usize = 1000;

/// Rows as a Markdown table. Pipes are escaped, line breaks become spaces.
pub(super) fn to_markdown(outcome: &QueryOutcome, rows: &[usize]) -> String {
    use std::fmt::Write as _;
    let cell = |v: &str| v.replace('|', "\\|").replace(['\r', '\n'], " ");
    let mut out = String::new();
    let _ = writeln!(out, "| {} |", outcome.columns.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(out, "|{}", " --- |".repeat(outcome.columns.len()));
    for &i in rows.iter().take(MARKDOWN_MAX_ROWS) {
        let cells: Vec<String> =
            outcome.rows[i].iter().map(|c| c.as_deref().map_or_else(|| "NULL".into(), cell)).collect();
        let _ = writeln!(out, "| {} |", cells.join(" | "));
    }
    out
}

impl App {
    /// The editable table behind the current result, or why there isn't one.
    pub(super) fn edit_table(&self) -> Result<EditTable, String> {
        if self.connection.mode == ConnectionMode::ReadOnly {
            return Err(t!("rows.read_only").into());
        }
        let results = self.tab.grid.results.as_ref().ok_or(t!("rows.no_results"))?;
        let (sql, _) = self.tab.last_sql.as_ref().ok_or(t!("rows.no_query"))?;
        let source = analyze::edit_source(&parse(sql, self.engine())).ok_or(t!("rows.not_editable"))?;
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
        let engine = self.engine();
        let sql_name = match &source.schema {
            Some(s) => format!("{}.{}", engine.quote_ident(s), engine.quote_ident(&table.info.name)),
            None => engine.quote_ident(&table.info.name),
        };
        let selectable = |name: &str| source.columns.as_ref().is_none_or(|cols| cols.iter().any(|c| c == name));
        let key = table
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| {
                (c.name.clone(), results.columns.iter().position(|r| *r == c.name).filter(|_| selectable(&c.name)))
            })
            .collect();
        Ok(EditTable { sql_name, name: table.info.name.clone(), columns: table.columns.clone(), key })
    }

    /// The `pk = value` condition identifying result row `row`.
    fn row_condition(&self, table: &EditTable, row: usize) -> Result<String, String> {
        if table.key.is_empty() {
            return Err(t!("rows.no_primary_key", table = table.name));
        }
        let results = self.tab.grid.results.as_ref().ok_or(t!("rows.no_results"))?;
        let engine = self.engine();
        let mut parts = Vec::new();
        for (name, pos) in &table.key {
            let pos = pos.ok_or_else(|| t!("rows.key_not_in_result", column = name))?;
            let value = results.rows[row][pos].as_deref().ok_or(t!("rows.key_is_null"))?;
            let ty = results.column_types.get(pos).map_or("", |t| t);
            parts.push(format!("{} = {}", engine.quote_ident(name), crate::db::value_literal(value, ty, engine)));
        }
        Ok(parts.join(" AND "))
    }

    /// The rows an action applies to: the marked ones, else the current one
    /// (indices into `results.rows`).
    fn target_rows(&self) -> Vec<usize> {
        if !self.tab.grid.marked.is_empty() {
            return self.tab.grid.marked.iter().copied().collect();
        }
        self.tab.grid.table_state.selected().and_then(|i| self.tab.grid.view.get(i)).copied().into_iter().collect()
    }

    /// `Space`: marks or unmarks the current row, then moves down.
    pub(super) fn toggle_mark(&mut self) {
        let Some(at) = self.tab.grid.table_state.selected() else { return };
        let Some(&row) = self.tab.grid.view.get(at) else { return };
        if !self.tab.grid.marked.remove(&row) {
            self.tab.grid.marked.insert(row);
        }
        self.tab.grid.table_state.select(Some((at + 1).min(self.tab.grid.view.len().saturating_sub(1))));
        let n = self.tab.grid.marked.len();
        self.set_status(if n == 0 { String::new() } else { tn!("rows.marked", n) }, false);
    }

    /// `a`: opens the add-row form.
    pub(super) fn start_row_insert(&mut self) {
        match self.edit_table() {
            Ok(table) => {
                let values = vec![String::new(); table.columns.len()];
                self.row_form = Some(RowForm {
                    table: table.name,
                    table_sql: table.sql_name,
                    columns: table.columns,
                    values,
                    field: 0,
                });
            }
            Err(why) => self.set_status(t!("rows.cant_add", why = why), true),
        }
    }

    pub(super) fn handle_row_form_key(&mut self, key: KeyEvent) {
        let Some(form) = &mut self.row_form else { return };
        if let Some(i) = list::step(form.field, form.columns.len(), key, Nav::FIELDS) {
            form.field = i;
            return;
        }
        match key.code {
            KeyCode::Esc => self.row_form = None,
            KeyCode::Enter => self.submit_row_insert(),
            _ => {
                input::edit_at_end(&mut form.values[form.field], key);
            }
        }
    }

    pub(super) fn paste_row_form(&mut self, text: &str) {
        if let Some(form) = &mut self.row_form {
            input::paste_at_end(&mut form.values[form.field], text);
        }
    }

    /// INSERT with the filled-in fields; empty ones get their default.
    fn submit_row_insert(&mut self) {
        if self.conn_busy() {
            return self.set_status(t!("rows.wait_for_query").to_string(), true);
        }
        let Some(form) = self.row_form.take() else { return };
        let engine = self.engine();
        let filled: Vec<(String, String)> = form
            .columns
            .iter()
            .zip(&form.values)
            .filter(|(_, v)| !v.is_empty())
            .map(|(c, v)| (engine.quote_ident(&c.name), crate::sql::params::literal(v, engine)))
            .collect();
        let sql = if filled.is_empty() {
            engine.dialect().insert_defaults(&form.table_sql)
        } else {
            let (cols, vals): (Vec<String>, Vec<String>) = filled.into_iter().unzip();
            format!("INSERT INTO {} ({}) VALUES ({})", form.table_sql, cols.join(", "), vals.join(", "))
        };
        self.execute_with(sql, 0, RunPurpose::RowChange(t!("rows.added")));
    }

    /// `d`: deletes the marked rows (or the current one), always asking first.
    pub(super) fn delete_rows(&mut self) {
        let rows = self.target_rows();
        let table = match self.edit_table() {
            Ok(t) => t,
            Err(why) => return self.set_status(t!("rows.cant_delete", why = why), true),
        };
        if rows.is_empty() {
            return self.set_status(t!("rows.none_selected").to_string(), true);
        }
        let mut conditions = Vec::new();
        for &row in &rows {
            match self.row_condition(&table, row) {
                Ok(c) => conditions.push(format!("({c})")),
                Err(why) => return self.set_status(t!("rows.cant_delete", why = why), true),
            }
        }
        let sql = format!("DELETE FROM {} WHERE {}", table.sql_name, conditions.join(" OR "));
        let mut details: Vec<String> =
            conditions.iter().take(5).map(|c| c.trim_matches(['(', ')']).to_string()).collect();
        if conditions.len() > 5 {
            details.push(t!("rows.and_more", n = conditions.len() - 5));
        }
        let infos = analyze::analyze(&parse(&sql, self.engine()));
        let offer_transaction = self.connection.txn == TxnState::Idle;
        self.confirm = Some(Confirm {
            message: tn!("rows.delete_from", rows.len(), table = table.name),
            details,
            danger: true,
            action: ConfirmAction::Run {
                sql,
                offset: 0,
                infos,
                purpose: RunPurpose::RowChange(t!("rows.deleted")),
                filled: None,
            },
            offer_transaction,
        });
    }

    /// After an INSERT or DELETE from the grid: report, and run the query
    /// again to show the change.
    pub(super) fn rows_changed(&mut self, what: &str, outcome: &QueryOutcome) {
        let n = outcome.rows_affected.unwrap_or(0);
        let msg = format!("{what} {}", tn!("rows.rows", n));
        self.log(msg.clone(), false);
        self.set_status(msg.clone(), false);
        self.tab.grid.marked.clear();
        let refresh = self.tab.last_query_infos.iter().all(|i| i.kind == Kind::Read);
        if let (true, Some((sql, offset))) = (refresh, self.tab.last_sql.clone()) {
            let infos = self.tab.last_query_infos.clone();
            self.rerun_shown(sql, offset, infos);
            self.tab.run.extra.status_prefix = Some(msg);
        }
    }

    /// `C`: copies the marked rows (or all rows shown) as a Markdown table.
    pub(super) fn copy_markdown(&mut self) {
        let Some(results) = self.tab.grid.results.as_ref().filter(|r| !r.columns.is_empty()) else {
            return self.set_status(t!("rows.none_to_copy").to_string(), true);
        };
        let rows: Vec<usize> = if self.tab.grid.marked.is_empty() {
            self.tab.grid.view.clone()
        } else {
            self.tab.grid.marked.iter().copied().collect()
        };
        let text = to_markdown(results, &rows);
        let n = rows.len().min(MARKDOWN_MAX_ROWS);
        let more = if rows.len() > MARKDOWN_MAX_ROWS { format!(" (first {MARKDOWN_MAX_ROWS})") } else { String::new() };
        self.copy_text(text, &tn!("rows.copied_markdown", n, more = more));
    }
}

impl App {
    /// The marked rows, or else all those shown, as CSV, JSON or INSERTs.
    pub(super) fn copy_rows_as(&mut self, format: ExportFormat) {
        let format = OpenFormat::from(format);
        let Some((rows, text)) = self.rows_as(format) else {
            return self.set_status(t!("rows.none_to_copy").to_string(), true);
        };
        self.copy_text(text, &tn!("rows.copied_as", rows, format = format.name()));
    }

    /// The rows to copy or open (the marked ones, else those shown), in
    /// `format`, and how many; `None` without a result.
    pub(super) fn rows_as(&self, format: OpenFormat) -> Option<(usize, String)> {
        let results = self.tab.grid.results.as_ref().filter(|r| !r.columns.is_empty())?;
        let rows: Vec<usize> = if self.tab.grid.marked.is_empty() {
            self.tab.grid.view.clone()
        } else {
            self.tab.grid.marked.iter().copied().collect()
        };
        let text = match format {
            OpenFormat::Csv => crate::transfer::export::csv(results, &rows),
            OpenFormat::Tsv => crate::transfer::export::tsv(results, &rows),
            OpenFormat::Json => crate::transfer::export::json(results, &rows),
            OpenFormat::Text => crate::transfer::export::render(results, &rows, crate::transfer::export::Format::Table),
            OpenFormat::Ascii => crate::transfer::export::ascii_table(results, &rows),
            OpenFormat::Sql => {
                crate::transfer::export::sql_inserts(results, &rows, &self.insert_table(), self.engine())
            }
        };
        Some((rows.len(), text))
    }
}

/// Marked result rows (indices into `results.rows`).
pub type Marks = BTreeSet<usize>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_tables_escape_pipes() {
        let outcome = QueryOutcome {
            columns: vec!["id".into(), "note".into()],
            rows: vec![vec![Some("1".into()), Some("a|b\nc".into())], vec![Some("2".into()), None]],
            ..QueryOutcome::default()
        };
        assert_eq!(to_markdown(&outcome, &[0, 1]), "| id | note |\n| --- | --- |\n| 1 | a\\|b c |\n| 2 | NULL |\n");
    }
}
