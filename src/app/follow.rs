//! Following a foreign key from a result cell (`f`): opens the referenced
//! row in a new tab and runs it.

use super::*;

impl App {
    pub(super) fn follow_foreign_key(&mut self) {
        match self.foreign_key_query() {
            Ok(Some((name, sql))) => {
                self.new_tab(Some(name), &sql, None);
                self.execute_all();
            }
            // Waiting for the table's foreign keys; tried again when they arrive.
            Ok(None) => self.set_status("Loading foreign keys…".to_string(), false),
            Err(why) => self.set_status(format!("Can't follow: {why}"), true),
        }
    }

    /// The tab name and query for the referenced row, or `None` while the
    /// source table's details are loading.
    fn foreign_key_query(&mut self) -> Result<Option<(String, String)>, String> {
        let results = self.tab.grid.results.as_ref().ok_or("no results")?;
        let row = *self
            .tab
            .grid
            .view
            .get(self.tab.grid.table_state.selected().ok_or("no row selected")?)
            .ok_or("no row selected")?;
        let column = results.columns.get(self.tab.grid.selected_col).ok_or("no column selected")?.clone();
        let value = results.rows[row][self.tab.grid.selected_col].clone().ok_or("the value is NULL")?;
        let (sql, _) = self.tab.last_sql.as_ref().ok_or("no query")?;
        let engine = self.engine();
        let source = analyze::edit_source(&parse(sql, engine))
            .ok_or("only results of a plain SELECT from one table can be followed")?;
        let panel = self.schema.as_ref().ok_or("the schema hasn't loaded yet")?;
        let t = panel
            .tables
            .iter()
            .position(|t| t.info.name == source.table && source.schema.as_ref().is_none_or(|s| *s == t.info.schema))
            .ok_or_else(|| format!("table {} not found in the schema", source.table))?;
        let Some(details) = &panel.tables[t].details else {
            self.pending_follow = true;
            self.request_details(t);
            return Ok(None);
        };
        let fk = details
            .foreign_keys
            .iter()
            .find(|f| f.column == column)
            .ok_or_else(|| format!("{column} isn't a foreign key"))?;
        // SQLite may leave the target column unnamed: the primary key.
        let target_column = match &fk.target_column {
            Some(c) => c.clone(),
            None => panel
                .tables
                .iter()
                .find(|t| t.info.name == fk.target_table)
                .and_then(|t| t.columns.iter().find(|c| c.primary_key))
                .map_or_else(|| "rowid".to_string(), |c| c.name.clone()),
        };
        let table = if source.schema.is_some() || fk.target_schema != panel.tables[t].info.schema {
            format!("{}.{}", engine.quote_ident(&fk.target_schema), engine.quote_ident(&fk.target_table))
        } else {
            engine.quote_ident(&fk.target_table)
        };
        let query = format!(
            "SELECT * FROM {table} WHERE {} = {};",
            engine.quote_ident(&target_column),
            crate::params::literal(&value, engine)
        );
        Ok(Some((format!("→ {}", fk.target_table), query)))
    }
}
