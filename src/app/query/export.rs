//! Exporting results: to a file, or all rows of a query.

use super::super::*;

impl App {
    /// `podelis-export-<timestamp>.<ext>` in `export_dir` (or the current folder).
    pub(in crate::app) fn export_path(&self, format: ExportFormat) -> PathBuf {
        let ext = match format {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
        };
        let dir = match self.settings.export_dir.as_deref() {
            Some(d) if d.starts_with("~/") => crate::config::settings::expand_home(d),
            Some(d) => PathBuf::from(d),
            None => PathBuf::new(),
        };
        dir.join(format!("podelis-export-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S")))
    }

    /// The table `INSERT` exports go into: the query's, when it reads just one.
    pub(in crate::app) fn insert_table(&self) -> String {
        let engine = self.engine();
        self.tab.last_sql.as_ref().and_then(|(sql, _)| analyze::edit_source(&parse(sql, engine))).map_or_else(
            || "result".to_string(),
            |s| match s.schema {
                Some(schema) => format!("{}.{}", engine.quote_ident(&schema), engine.quote_ident(&s.table)),
                None => engine.quote_ident(&s.table),
            },
        )
    }

    /// Writes the rows as shown to a CSV or JSON file in the current folder.
    pub(in crate::app) fn export(&mut self, format: ExportFormat) {
        let path = self.export_path(format);
        let table = self.insert_table();
        let Some(results) = &self.tab.grid.results else { return };
        let truncated = results.truncated() || self.run.stream.is_some();
        let rows = self.tab.grid.view.len();
        let engine = self.engine();
        let text = match format {
            ExportFormat::Csv => crate::transfer::export::csv(results, &self.tab.grid.view),
            ExportFormat::Json => crate::transfer::export::json(results, &self.tab.grid.view),
            ExportFormat::Sql => crate::transfer::export::sql_inserts(results, &self.tab.grid.view, &table, engine),
        };
        match std::fs::write(&path, text) {
            Ok(()) => {
                let mut note = format!(" ({rows} rows");
                if truncated && self.run.stream.is_some() {
                    note.push_str(" loaded; Ctrl+K “export all” writes every row");
                } else if truncated {
                    let _ = write!(note, " of {}; M loads more", results.total_rows);
                } else if !self.tab.grid.filter.is_empty() {
                    note.push_str(", filtered");
                }
                note.push(')');
                let shown = std::fs::canonicalize(&path).unwrap_or(path);
                self.set_status(format!("Exported to {}{note}", shown.display()), false);
            }
            Err(e) => self.set_status(format!("Export failed: {e}"), true),
        }
    }

    /// `b`: a bar chart of the selected column, for the rows shown.
    pub(in crate::app) fn chart_column(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let col = self.tab.grid.selected_col;
        match crate::chart::chart(results, &self.tab.grid.view, col) {
            Ok(text) => {
                let title = format!("CHART · {}", results.columns.get(col).map_or("", String::as_str));
                self.cell_view = Some(CellView::new(title, text));
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Writes every row of the result to a file: those loaded, then the
    /// rest of the stream page by page, straight to the file rather than
    /// into the grid. Esc stops it.
    pub(in crate::app) fn export_all(&mut self, format: ExportFormat) {
        const PAGE: usize = 10_000;
        if self.export_task.is_some() {
            return self.set_status("An export is already running (Esc stops it)".to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if self.run.stream.is_none() {
            if results.truncated() {
                return self.set_status(
                    "Only a statement run on its own (F6) can be exported in full; this result was cut at the row limit"
                        .to_string(),
                    true,
                );
            }
            return self.export(format);
        }
        if self.run.fetching_more {
            return self.set_status("Rows are still loading; try again in a moment".to_string(), true);
        }
        let Some(stream) = self.run.stream.take() else { return };
        let path = self.export_path(format);
        let paged = match format {
            ExportFormat::Csv => crate::transfer::export::Paged::Csv,
            ExportFormat::Json => crate::transfer::export::Paged::Json,
            ExportFormat::Sql => {
                crate::transfer::export::Paged::Inserts { table: self.insert_table(), engine: self.engine() }
            }
        };
        let (columns, types, loaded) = (results.columns.clone(), results.column_types.clone(), results.rows.clone());
        let send = self.sender();
        let target = path.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let written = async {
                let file = std::io::BufWriter::new(std::fs::File::create(&target)?);
                let mut out = crate::transfer::export::PagedWriter::new(file, paged, &columns, &types);
                out.page(loaded)?;
                loop {
                    let page = tokio::select! {
                        page = stream.next_page(PAGE) => page.map_err(|e| anyhow::anyhow!(e))?,
                        _ = &mut stopped => anyhow::bail!("stopped"),
                    };
                    let done = page.done;
                    out.page(page.rows)?;
                    send(DbEventKind::ExportProgress(out.rows));
                    if done {
                        break;
                    }
                }
                let rows = out.rows;
                out.finish()?;
                anyhow::Ok(rows)
            }
            .await;
            // The stream goes with this task, which cancels the query; an
            // unfinished file isn't left behind.
            drop(stream);
            if written.is_err() {
                let _ = std::fs::remove_file(&target);
            }
            send(DbEventKind::ExportDone(target, written.map_err(|e| format!("{e:#}"))));
        });
        self.export_task = Some((stop, path));
        self.set_status("Exporting every row… (Esc stops it)".to_string(), false);
    }

    /// Stops a running export; it removes its unfinished file.
    pub(in crate::app) fn stop_export(&mut self) {
        let Some((stop, _)) = self.export_task.take() else { return };
        let _ = stop.send(());
        self.set_status("Export stopped; the unfinished file is removed".to_string(), false);
    }
}
