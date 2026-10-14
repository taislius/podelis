//! Comparing this tab's connection with another open one: their schemas,
//! or the rows the statement at the cursor returns on each.

use super::*;
use crate::diff::{self, Side, TableShape};

/// A connection's name for messages: its saved name, else its address.
pub(super) fn conn_label(form: &ConnectForm) -> String {
    let name = form.name.trim();
    if name.is_empty() { form.summary() } else { name.to_string() }
}

/// The other side of a comparison, as it was when it started.
pub struct Other {
    pub id: u64,
    pub name: String,
    pub engine: Engine,
}

/// Both sides' tables for a schema comparison.
pub type Shapes = (Vec<TableShape>, Vec<TableShape>);

/// A schema with each table's indexes and foreign keys.
async fn shapes(db: &Database) -> Result<Vec<TableShape>, String> {
    let catalog = db.catalog(false).await.map_err(|e| crate::db::error_message(&e))?;
    let mut out = Vec::with_capacity(catalog.len());
    for (info, columns) in catalog {
        // Details may need privileges the user lacks: then only columns are compared.
        let details = db.table_details(&info.schema, &info.name).await.ok();
        out.push(TableShape { info, columns, details });
    }
    Ok(out)
}

impl App {
    /// The other tabs' open connections: id and name.
    pub(super) fn other_connections(&self) -> Vec<(u64, String)> {
        self.parked_connections()
            .into_iter()
            .filter(|(id, _, _)| Some(*id) != self.conns.active)
            .map(|(id, _, form)| (id, conn_label(form)))
            .collect()
    }

    /// The connection to compare with: `id`, or the only other one.
    fn compare_target(&mut self, id: Option<u64>) -> Option<(Database, Other)> {
        if self.database.is_none() {
            self.set_status(self.not_connected_message(), true);
            return None;
        }
        let others = self.other_connections();
        let id = match (id, others.as_slice()) {
            (Some(id), _) => id,
            (None, [(id, _)]) => *id,
            (None, []) => {
                self.set_status("Open another connection in a tab first (Ctrl+O), then compare with it".into(), true);
                return None;
            }
            (None, _) => {
                self.set_status("Pick which connection: Ctrl+K, then type “compare”".into(), true);
                return None;
            }
        };
        let (_, db, form) = self.parked_connections().into_iter().find(|(i, _, _)| *i == id)?;
        let other = Other { id, name: conn_label(form), engine: db.engine() };
        Some((db.clone(), other))
    }

    /// Compares this tab's schema with another connection's.
    pub(super) fn compare_schema(&mut self, id: Option<u64>) {
        if self.executing {
            return;
        }
        let Some(db) = self.database.clone() else { return self.set_status(self.not_connected_message(), true) };
        let Some((there, other)) = self.compare_target(id) else { return };
        // An open stream holds this side's session.
        self.row_stream = None;
        self.set_status(format!("Comparing the schema with {}…", other.name), false);
        let send = self.sender();
        tokio::spawn(async move {
            let (a, b) = tokio::join!(shapes(&db), shapes(&there));
            let result = match (a, b) {
                (Ok(a), Ok(b)) => Ok((a, b)),
                (Err(e), _) => Err(format!("Couldn't read this schema: {e}")),
                (_, Err(e)) => Err(format!("Couldn't read {}'s schema: {e}", other.name)),
            };
            send(DbEventKind::SchemaCompared { other, result });
        });
    }

    pub(super) fn schema_compared(&mut self, other: &Other, result: Result<Shapes, String>) {
        let (here, there) = match result {
            Ok(shapes) => shapes,
            Err(e) => return self.set_status(e, true),
        };
        let this = conn_label(&self.connect_form);
        let diff = diff::schema_diff(
            &here,
            &there,
            Side { name: &this, engine: self.engine() },
            Side { name: &other.name, engine: other.engine },
        );
        let status = if diff.differences == 0 {
            format!("The schemas match ({this} and {})", other.name)
        } else {
            format!("{} difference(s) between {this} and {}", diff.differences, other.name)
        };
        self.log(status.clone(), false);
        self.set_status(status, false);
        let mut text = diff.report;
        if !diff.sql.is_empty() {
            text = format!("{text}\n\n{}", diff.sql);
        }
        let sql = (!diff.sql.is_empty()).then_some((diff.sql, other.id));
        self.cell_view = Some(CellView { sql, ..CellView::new(format!("SCHEMA · {this} ↔ {}", other.name), text) });
    }

    /// The project's schema snapshots, newest first.
    pub(super) fn snapshots(&self) -> Vec<crate::snapshots::Saved> {
        match (&self.data_dir, &self.current_project) {
            (Some(dir), Some(id)) => crate::snapshots::list(&crate::snapshots::dir(dir, id)),
            _ => Vec::new(),
        }
    }

    /// Saves this connection's schema to a file, to compare with later.
    pub(super) fn snapshot_schema(&mut self) {
        let Some(db) = self.database.clone() else { return self.set_status(self.not_connected_message(), true) };
        let (Some(dir), Some(id)) = (&self.data_dir, &self.current_project) else { return };
        let dir = crate::snapshots::dir(dir, id);
        let connection = conn_label(&self.connect_form);
        self.row_stream = None;
        self.set_status(format!("Saving a snapshot of {connection}'s schema…"), false);
        let send = self.sender();
        tokio::spawn(async move {
            let saved = async {
                let tables = shapes(&db).await?;
                let now = chrono::Local::now();
                let taken = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
                let snapshot = crate::snapshots::Snapshot { connection, engine: db.engine(), taken, tables };
                let path = crate::snapshots::save(&dir, &snapshot, now).map_err(|e| format!("{e:#}"))?;
                Ok((path, snapshot.tables.len()))
            }
            .await;
            send(DbEventKind::SnapshotSaved(saved));
        });
    }

    pub(super) fn snapshot_saved(&mut self, result: Result<(PathBuf, usize), String>) {
        match result {
            Ok((path, tables)) => {
                let status = format!("Saved a schema snapshot ({tables} tables): {}", path.display());
                self.log(status.clone(), false);
                self.set_status(status, false);
            }
            Err(e) => self.set_status(format!("Couldn't save a snapshot: {e}"), true),
        }
    }

    /// Compares this connection's schema now with snapshot `index` (in
    /// [`Self::snapshots`]): what changed since.
    pub(super) fn compare_snapshot(&mut self, index: usize) {
        let Some(db) = self.database.clone() else { return self.set_status(self.not_connected_message(), true) };
        let Some(saved) = self.snapshots().into_iter().nth(index) else { return };
        self.row_stream = None;
        self.set_status(format!("Comparing with the snapshot of {} from {}…", saved.connection, saved.taken), false);
        let send = self.sender();
        tokio::spawn(async move {
            let result = async {
                let snapshot = crate::snapshots::load(&saved.path).map_err(|e| format!("{e:#}"))?;
                Ok((shapes(&db).await?, snapshot))
            }
            .await;
            send(DbEventKind::SnapshotCompared(result));
        });
    }

    pub(super) fn snapshot_compared(&mut self, result: Result<(Vec<TableShape>, crate::snapshots::Snapshot), String>) {
        let (now, snapshot) = match result {
            Ok(both) => both,
            Err(e) => return self.set_status(e, true),
        };
        let this = format!("{} now", conn_label(&self.connect_form));
        let then = format!("the snapshot of {} from {}", snapshot.connection, snapshot.taken.replace('T', " "));
        let diff = diff::schema_diff(
            &now,
            &snapshot.tables,
            Side { name: &this, engine: self.engine() },
            Side { name: &then, engine: snapshot.engine },
        );
        let status = if diff.differences == 0 {
            format!("No changes since {then}")
        } else {
            format!("{} change(s) since {then}", diff.differences)
        };
        self.log(status.clone(), false);
        self.set_status(status, false);
        // "Only here" is what was added since; the SQL would bring a copy of
        // the database as it was then up to now.
        let mut text = diff.report;
        if !diff.sql.is_empty() {
            text = format!("{text}\n\nThe changes since, as SQL:\n\n{}", diff.sql);
        }
        self.cell_view = Some(CellView::new(format!("SCHEMA · {this} ↔ {then}"), text));
    }

    /// Runs the selection, or the statement at the cursor, on this and
    /// another connection and shows the rows that differ.
    pub(super) fn compare_rows(&mut self, id: Option<u64>) {
        if self.executing {
            return;
        }
        let Some(db) = self.database.clone() else { return self.set_status(self.not_connected_message(), true) };
        let text = self.tab.editor.text();
        let sql = match self.tab.editor.selection_offsets() {
            Some((a, b)) => text[a..b].trim().to_string(),
            None => match sql::statement_at(&text, self.tab.editor.cursor_offset(), self.engine()) {
                Some(s) => s.to_string(),
                None => return self.set_status("No statement at cursor".to_string(), true),
            },
        };
        let infos = analyze::analyze(&parse(&sql, self.engine()));
        if infos.is_empty() || infos.iter().any(|i| i.kind != Kind::Read) {
            return self.set_status("Only reads can be compared: the query runs on both connections".into(), true);
        }
        let Some((there, other)) = self.compare_target(id) else { return };
        self.row_stream = None;
        self.executing = true;
        self.running_since = Some(Instant::now());
        self.run_owner = None;
        self.set_result_status(format!("Running on this connection and on {}…", other.name), false);
        let send = self.sender();
        let limit = self.settings.row_limit;
        self.task = Some(tokio::spawn(async move {
            let (a, b) = tokio::join!(db.run(&sql, limit), there.run(&sql, limit));
            let result = match (a, b) {
                (Ok(a), Ok(b)) => Ok((a, b)),
                (Err(e), _) => Err(format!("Here: {}", crate::db::error_message(&e))),
                (_, Err(e)) => Err(format!("On {}: {}", other.name, crate::db::error_message(&e))),
            };
            send(DbEventKind::RowsCompared { other, result: Box::new(result) });
        }));
    }

    pub(super) fn rows_compared(&mut self, other: &Other, result: Result<(QueryOutcome, QueryOutcome), String>) {
        self.executing = false;
        self.task = None;
        // A plan asked for meanwhile: the comparison is shown instead.
        self.queued_plan = None;
        let compared = result.and_then(|(here, there)| {
            let truncated = here.truncated() || there.truncated();
            diff::row_diff(&here, &there).map(|d| (d, truncated, here.rows.len()))
        });
        let (diff, truncated, rows) = match compared {
            Ok(d) => d,
            Err(e) => {
                self.log(e.clone(), true);
                self.set_result_status(e, true);
                return;
            }
        };
        self.show_results(diff.outcome);
        // The rows are a comparison, not a table's: nothing to edit or re-run.
        self.tab.last_sql = None;
        self.tab.last_query_infos.clear();
        let by = diff.key.map(|k| format!(" by {k}")).unwrap_or_default();
        let mut status = if diff.only_here + diff.only_there + diff.changed == 0 {
            format!("The rows match: {} the same here and on {}", diff.same, other.name)
        } else {
            format!(
                "Compared{by} with {}: {} only here, {} only there, {} changed, {} the same",
                other.name, diff.only_here, diff.only_there, diff.changed, diff.same
            )
        };
        if truncated {
            let _ = write!(status, " (first {rows} rows; row_limit)");
        }
        self.log(status.clone(), false);
        self.set_result_status(status, false);
        self.set_result_tab(ResultTab::Rows);
        self.focus = Focus::Results;
    }
}
