//! Following a foreign key from a result cell (`f`): the row it points
//! to, in a pop-over (one `column  value` line each), without leaving the
//! result; `e` there opens it in a tab of its own and runs it.

use super::*;

/// What `f` found to look up.
pub(super) struct Followed {
    /// The referenced table, and the key and value: `customers · id = 1`.
    pub title: String,
    /// A tab's name, if it's opened in one.
    pub tab_name: String,
    pub sql: String,
    /// The tab that asked, on its connection then: the row is for it only.
    pub asked_from: (usize, Option<u64>),
}

/// The most rows a lookup shows (a key that isn't unique may match more).
const MAX_ROWS: usize = 20;

impl App {
    pub(super) fn follow_foreign_key(&mut self) {
        match self.foreign_key_query() {
            Ok(Some(found)) => self.look_up(found),
            // Waiting for the tables' foreign keys; tried again when they arrive.
            Ok(None) => self.set_status("Loading foreign keys…".to_string(), false),
            Err(why) => self.set_status(format!("Can't follow: {why}"), true),
        }
    }

    /// Runs the lookup on the tab's connection (it sees what the tab sees,
    /// its open transaction too) and shows the row when it comes.
    fn look_up(&mut self, found: Followed) {
        if self.run.executing {
            return self.set_status("A query is running: follow the key once it's done".into(), true);
        }
        let Some(db) = self.database.clone() else { return };
        // Rows still streaming hold the session: they stop here (as on
        // switching tabs), or the lookup would wait for the last of them.
        self.close_stream();
        let send = self.sender();
        self.set_status(format!("Looking up {}…", found.title), false);
        tokio::spawn(async move {
            let result = db.run(&found.sql, MAX_ROWS + 1).await.map_err(|e| crate::db::error_message(&e));
            send(DbEventKind::Followed(Box::new((found, result))));
        });
    }

    pub(super) fn followed(&mut self, found: Followed, result: Result<QueryOutcome, String>) {
        // Another tab (or connection) since: its pop-over, and `e` there,
        // would be for the wrong database.
        if found.asked_from != (self.active_tab, self.tabs[self.active_tab].conn) {
            return;
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.set_status(format!("Couldn't look up {}: {e}", found.title), true),
        };
        // "Loading foreign keys…", "Looking up…": done.
        if self
            .results_note
            .as_ref()
            .is_some_and(|(m, _)| m.starts_with("Loading foreign keys") || m.starts_with("Looking up"))
        {
            self.results_note = None;
        }
        self.set_status(String::new(), false);
        let name_w = outcome.columns.iter().map(|c| c.chars().count()).max().unwrap_or(0).min(30);
        let mut text = String::new();
        if outcome.rows.is_empty() {
            text.push_str("No row has that key (it was deleted, or the key isn't enforced).");
        }
        for (i, row) in outcome.rows.iter().take(MAX_ROWS).enumerate() {
            if outcome.rows.len() > 1 {
                let _ = writeln!(text, "{}— row {} —", if i > 0 { "\n" } else { "" }, i + 1);
            }
            for (name, cell) in outcome.columns.iter().zip(row) {
                let value = cell.as_deref().map_or_else(|| NULL_TEXT.to_string(), pretty_value);
                let value = value.replace('\n', &format!("\n{:name_w$}  ", ""));
                let _ = writeln!(text, "{name:<name_w$}  {value}");
            }
        }
        if outcome.rows.len() > MAX_ROWS {
            let _ = write!(text, "\n… and more: `e` opens them all in a tab");
        }
        self.cell_view = Some(CellView {
            follow: Some((found.tab_name, found.sql)),
            ..CellView::new(format!("→ {}", found.title), text)
        });
    }

    /// `e` in the pop-over: the lookup in a tab of its own, run.
    pub(super) fn open_followed(&mut self, tab: String, sql: &str) {
        self.cell_view = None;
        self.new_tab(Some(tab), sql, None);
        self.execute_all();
    }

    /// What to look up for the selected cell, or `None` while the tables'
    /// details are loading. The column is looked for among the foreign
    /// keys of the tables the query reads (a join's too): it must be one
    /// in exactly one of them.
    fn foreign_key_query(&mut self) -> Result<Option<Followed>, String> {
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
        let sources = query_tables(sql, engine);
        if sources.is_empty() {
            return Err("only results of a SELECT from tables can be followed".into());
        }
        let panel = self.schema.as_ref().ok_or("the schema hasn't loaded yet")?;
        let lower = |s: &str| s.to_lowercase();
        let mut found: Vec<usize> = sources
            .iter()
            .filter_map(|(schema, name)| {
                panel.tables.iter().position(|t| {
                    lower(&t.info.name) == lower(name)
                        && schema.as_ref().is_none_or(|s| lower(s) == lower(&t.info.schema))
                })
            })
            .collect();
        // One table written two ways (`public.t` and `t`) is one table.
        found.sort_unstable();
        found.dedup();
        if found.is_empty() {
            return Err(format!("{} isn't in the schema", sources[0].1));
        }
        // Each table's foreign keys, loaded first where they aren't yet.
        if let Some(&t) = found.iter().find(|&&t| panel.tables[t].details.is_none()) {
            // Loading them needs the session: rows still streaming stop.
            self.close_stream();
            self.pending_follow = true;
            self.request_details(t);
            return Ok(None);
        }
        let mut keys = Vec::new();
        for &t in &found {
            let table = &panel.tables[t];
            let details = table.details.as_ref().expect("loaded above");
            keys.extend(details.foreign_keys.iter().map(|fk| (t, fk)));
        }
        let matching: Vec<_> = keys.iter().filter(|(_, fk)| lower(&fk.column) == lower(&column)).collect();
        let (t, fk) = match matching.as_slice() {
            [one] => **one,
            [] if keys.is_empty() => return Err(format!("{column} isn't a foreign key, and these tables have none")),
            [] => {
                let some: Vec<String> =
                    keys.iter().take(4).map(|(_, fk)| format!("{} → {}", fk.column, fk.target_table)).collect();
                return Err(format!("{column} isn't a foreign key; these are: {}", some.join(", ")));
            }
            _ => {
                let tables: Vec<&str> = matching.iter().map(|(t, _)| panel.tables[*t].info.name.as_str()).collect();
                return Err(format!("{column} is a foreign key in {}: select from one of them", tables.join(" and ")));
            }
        };
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
        let source_schema = &panel.tables[t].info.schema;
        let qualified = sources.iter().any(|(s, _)| s.is_some()) || fk.target_schema != *source_schema;
        let table = if qualified {
            format!("{}.{}", engine.quote_ident(&fk.target_schema), engine.quote_ident(&fk.target_table))
        } else {
            engine.quote_ident(&fk.target_table)
        };
        let literal = crate::sql::params::literal(&value, engine);
        let sql = format!("SELECT * FROM {table} WHERE {} = {literal};", engine.quote_ident(&target_column));
        Ok(Some(Followed {
            title: format!("{} · {target_column} = {}", fk.target_table, truncate_value(&value)),
            tab_name: format!("→ {}", fk.target_table),
            sql,
            asked_from: (self.active_tab, self.tabs[self.active_tab].conn),
        }))
    }
}

/// The tables `sql`'s last statement reads, as written (schema, name),
/// each once: its `FROM` and joins, CTEs left out.
fn query_tables(sql: &str, engine: crate::db::Engine) -> Vec<(Option<String>, String)> {
    let parsed = parse(sql, engine);
    let Some(Ok(statements)) = parsed.statements.last().map(|s| s.ast.as_ref()) else { return Vec::new() };
    let mut tables: Vec<(Option<String>, String)> = Vec::new();
    for scope in crate::sql::scope::lower(statements) {
        for table in &scope.tables {
            if let crate::sql::scope::Source::Table { schema, name } = &table.source
                && !scope.is_cte(&name.to_lowercase())
                && !tables.iter().any(|(s, n)| s == schema && n == name)
            {
                tables.push((schema.clone(), name.clone()));
            }
        }
    }
    tables
}

/// A value short enough for a title.
fn truncate_value(value: &str) -> String {
    let value = value.lines().next().unwrap_or_default();
    if value.chars().count() > 30 { value.chars().take(29).collect::<String>() + "…" } else { value.to_string() }
}
