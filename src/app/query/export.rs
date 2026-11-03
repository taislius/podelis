//! Exporting results: to a file, or all rows of a query.

use super::super::*;

impl App {
    /// `podelis-export-<timestamp>.<ext>` in `export_dir` (or the current folder).
    pub(in crate::app) fn export_path(&self, format: ExportFormat) -> PathBuf {
        self.export_path_ext(super::super::pipes::OpenFormat::from(format).ext())
    }

    /// The same, for a file of extension `ext` (`parquet`).
    fn export_path_ext(&self, ext: &str) -> PathBuf {
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
        let engine = self.engine();
        let text = match format {
            ExportFormat::Csv => crate::transfer::export::csv(results, &self.tab.grid.view),
            ExportFormat::Json => crate::transfer::export::json(results, &self.tab.grid.view),
            ExportFormat::Sql => crate::transfer::export::sql_inserts(results, &self.tab.grid.view, &table, engine),
            ExportFormat::Ascii => crate::transfer::export::ascii_table(results, &self.tab.grid.view),
        };
        let written = std::fs::write(&path, text);
        self.exported(&path, written);
    }

    /// Writes the rows as shown to a Parquet file, each column typed by its
    /// engine's type (`transfer::parquet`), from any engine: on a blocking
    /// thread (every value is read, then encoded), the screen drawn meanwhile.
    pub(in crate::app) fn export_parquet(&mut self) {
        if self.export_task.is_some() {
            return self.set_status(t!("export.already_running").to_string(), true);
        }
        let path = self.export_path_ext("parquet");
        let engine = self.engine();
        let note = self.shown_note();
        let Some(results) = &self.tab.grid.results else { return };
        let shown = crate::db::QueryOutcome {
            columns: results.columns.clone(),
            column_types: results.column_types.clone(),
            rows: self.tab.grid.view.iter().filter_map(|&i| results.rows.get(i).cloned()).collect(),
            ..crate::db::QueryOutcome::default()
        };
        let send = self.sender();
        let target = path.clone();
        tokio::task::spawn_blocking(move || {
            let all: Vec<usize> = (0..shown.rows.len()).collect();
            let written = std::fs::File::create(&target).and_then(|file| {
                let out = crate::transfer::parquet::write(&shown, &all, engine, std::io::BufWriter::new(file))?;
                out.into_inner().map(drop).map_err(std::io::IntoInnerError::into_error)
            });
            // A file that failed half-way isn't left looking like an export.
            if written.is_err() {
                let _ = std::fs::remove_file(&target);
            }
            send(Background::ExportShownDone(target, written.map_err(|e| e.to_string()), note).into());
        });
        // Its stop isn't heard (it's quick: the rows already loaded); kept so
        // one export runs at a time.
        let (stop, _) = tokio::sync::oneshot::channel();
        self.export_task = Some((stop, path));
        self.set_status(t!("export.exporting").to_string(), false);
    }

    /// How many of the rows an export of those shown has: " (10 rows
    /// loaded)", " (5 rows, filtered)"…
    fn shown_note(&self) -> String {
        let Some(results) = &self.tab.grid.results else { return String::new() };
        let truncated = results.truncated() || self.tab.run.stream.is_some();
        let rows = self.tab.grid.view.len();
        let said = if truncated && self.tab.run.stream.is_some() {
            t!("export.rows_loaded", rows = rows)
        } else if truncated {
            t!("export.rows_of", rows = rows, total = results.total_rows)
        } else if !self.tab.grid.filter.is_empty() {
            t!("export.rows_filtered", rows = rows)
        } else {
            tn!("export.rows", rows)
        };
        format!(" ({said})")
    }

    /// What an export of the rows shown says: where, and how many rows.
    fn exported(&mut self, path: &std::path::Path, written: std::io::Result<()>) {
        let note = self.shown_note();
        self.export_said(path, written.map_err(|e| e.to_string()), &note);
    }

    /// The status for an export of the rows shown that's done (`note`: how
    /// many of them).
    pub(in crate::app) fn export_said(&mut self, path: &std::path::Path, written: Result<(), String>, note: &str) {
        match written {
            Ok(()) => {
                let shown = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
                self.set_status(t!("export.done", path = shown.display(), note = note), false);
            }
            Err(e) => self.set_status(t!("run.export_failed", e = e), true),
        }
    }

    /// `b`: a bar chart of the selected column, for the rows shown.
    pub(in crate::app) fn chart_column(&mut self) {
        let Some(results) = &self.tab.grid.results else { return };
        let col = self.tab.grid.selected_col;
        match crate::chart::chart(results, &self.tab.grid.view, col) {
            Ok(text) => {
                let title = format!("{} · {}", t!("export.chart"), results.columns.get(col).map_or("", String::as_str));
                self.popups.cell_view = Some(CellView::new(title, text));
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// Writes every row of the result to a file: those loaded, then the
    /// rest of the stream page by page, straight to the file rather than
    /// into the grid. Esc stops it.
    pub(in crate::app) fn export_all(&mut self, paged: PagedFormat) {
        const PAGE: usize = 10_000;
        if self.export_task.is_some() {
            return self.set_status(t!("export.already_running").to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if self.tab.run.stream.is_none() {
            if results.truncated() {
                return self.set_status(t!("export.not_in_full").to_string(), true);
            }
            return match paged.as_text() {
                Some(format) => self.export(format),
                None => self.export_parquet(),
            };
        }
        if self.tab.run.fetching_more {
            return self.set_status(t!("export.still_loading").to_string(), true);
        }
        let Some(stream) = self.tab.run.stream.take() else { return };
        let engine = self.engine();
        // The file, and how it's written: text (`Some`), or Parquet.
        let (path, paged) = match paged {
            PagedFormat::Csv => (self.export_path(ExportFormat::Csv), Some(crate::transfer::export::Paged::Csv)),
            PagedFormat::Json => (self.export_path(ExportFormat::Json), Some(crate::transfer::export::Paged::Json)),
            PagedFormat::Sql => (
                self.export_path(ExportFormat::Sql),
                Some(crate::transfer::export::Paged::Inserts { table: self.insert_table(), engine }),
            ),
            PagedFormat::Parquet => (self.export_path_ext("parquet"), None),
        };
        let (columns, types, loaded) = (results.columns.clone(), results.column_types.clone(), results.rows.clone());
        let send = self.sender();
        let target = path.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let written = async {
                let file = std::io::BufWriter::new(std::fs::File::create(&target)?);
                let out = match paged {
                    Some(paged) => Sink::Text(crate::transfer::export::PagedWriter::new(file, paged, &columns, &types)),
                    None => Sink::Parquet(Box::new(crate::transfer::parquet::Pages::new(
                        file,
                        &columns,
                        &types,
                        engine,
                        target.with_extension("parquet.rows"),
                    )?)),
                };
                // Encoding (Parquet's above all) is work for a blocking
                // thread, not for the runtime's workers.
                let mut out = tokio::task::spawn_blocking(move || {
                    let mut out = out;
                    out.page(loaded).map(|()| out)
                })
                .await??;
                loop {
                    let page = tokio::select! {
                        page = stream.next_page(PAGE) => page.map_err(|e| anyhow::anyhow!(e))?,
                        _ = &mut stopped => anyhow::bail!("stopped"),
                    };
                    let done = page.done;
                    out = tokio::task::spawn_blocking(move || {
                        let mut out = out;
                        out.page(page.rows).map(|()| out)
                    })
                    .await??;
                    send(Background::ExportProgress(out.rows()).into());
                    if done {
                        break;
                    }
                }
                let rows = out.rows();
                // Parquet's file is written here, all of it: a stop ends it
                // between row groups.
                let halt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let flag = std::sync::Arc::clone(&halt);
                let mut finishing = tokio::task::spawn_blocking(move || out.finish(&flag));
                tokio::select! {
                    done = &mut finishing => done??,
                    _ = &mut stopped => {
                        halt.store(true, std::sync::atomic::Ordering::SeqCst);
                        let _ = finishing.await;
                        anyhow::bail!("stopped");
                    }
                }
                anyhow::Ok(rows)
            }
            .await;
            // The stream goes with this task, which cancels the query; an
            // unfinished file isn't left behind.
            drop(stream);
            if written.is_err() {
                let _ = std::fs::remove_file(&target);
            }
            send(Background::ExportDone(target, written.map_err(|e| format!("{e:#}"))).into());
        });
        self.export_task = Some((stop, path));
        self.set_status(t!("export.exporting").to_string(), false);
    }

    /// Stops a running export; it removes its unfinished file.
    pub(in crate::app) fn stop_export(&mut self) {
        let Some((stop, _)) = self.export_task.take() else { return };
        let _ = stop.send(());
        self.set_status(t!("export.stopped").to_string(), false);
    }
}

/// File formats for exporting results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    /// `INSERT` statements.
    Sql,
    /// A table with `+--+` borders.
    Ascii,
}

/// What every row of a result can be exported as, page by page as they
/// arrive (an ASCII table can't: its widths need every row first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedFormat {
    Csv,
    Json,
    Sql,
    /// Parquet's file (binary: a file only, never the clipboard).
    Parquet,
}

impl PagedFormat {
    /// The text format it is, if it's one (not Parquet).
    pub fn as_text(self) -> Option<ExportFormat> {
        match self {
            PagedFormat::Csv => Some(ExportFormat::Csv),
            PagedFormat::Json => Some(ExportFormat::Json),
            PagedFormat::Sql => Some(ExportFormat::Sql),
            PagedFormat::Parquet => None,
        }
    }
}

/// Where every row of a result goes, a page at a time: text, or Parquet.
enum Sink<W: std::io::Write + Send> {
    Text(crate::transfer::export::PagedWriter<W>),
    Parquet(Box<crate::transfer::parquet::Pages<W>>),
}

impl<W: std::io::Write + Send> Sink<W> {
    fn page(&mut self, rows: Vec<Vec<Option<String>>>) -> std::io::Result<()> {
        match self {
            Sink::Text(out) => out.page(rows),
            Sink::Parquet(out) => out.page(&rows),
        }
    }

    fn rows(&self) -> u64 {
        match self {
            Sink::Text(out) => out.rows,
            Sink::Parquet(out) => out.rows,
        }
    }

    /// Ends the file; `stop` ends Parquet's writing between row groups.
    fn finish(self, stop: &std::sync::atomic::AtomicBool) -> std::io::Result<()> {
        match self {
            Sink::Text(out) => out.finish().map(drop),
            Sink::Parquet(out) => out.finish(stop).map(drop),
        }
    }
}

impl From<ExportFormat> for pipes::OpenFormat {
    fn from(format: ExportFormat) -> Self {
        match format {
            ExportFormat::Csv => pipes::OpenFormat::Csv,
            ExportFormat::Json => pipes::OpenFormat::Json,
            ExportFormat::Sql => pipes::OpenFormat::Sql,
            ExportFormat::Ascii => pipes::OpenFormat::Ascii,
        }
    }
}
