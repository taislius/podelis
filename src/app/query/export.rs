//! Exporting results: to a file, or all rows of a query.

use super::super::*;

impl App {
    /// `podelis-export-<timestamp>.<ext>` in `export_dir` (or the current folder).
    pub(in crate::app) fn export_path(&self, format: Format) -> PathBuf {
        let ext = format.ext();
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

    /// Writes the rows as shown to a file in the current folder.
    pub(in crate::app) fn export(&mut self, format: Format) {
        let rows = self.tab.grid.view.clone();
        let note = self.shown_note();
        self.write_rows(format, &rows, note);
    }

    /// Writes `rows` of the result to a file; `note` says how many they are.
    fn write_rows(&mut self, format: Format, rows: &[usize], note: String) {
        if format == Format::Parquet {
            return self.export_parquet(rows, note);
        }
        let path = self.export_path(format);
        let Some(text) = self.rows_text(format, rows) else { return };
        let written = std::fs::write(&path, text);
        self.export_said(&path, written.map_err(|e| e.to_string()), &note);
    }

    /// Writes `rows` to a Parquet file, each column typed by its
    /// engine's type (`transfer::parquet`), from any engine: on a blocking
    /// thread (every value is read, then encoded), the screen drawn meanwhile.
    fn export_parquet(&mut self, rows: &[usize], note: String) {
        if self.export_task.is_some() {
            return self.set_status(t!("export.already_running").to_string(), true);
        }
        let path = self.export_path(Format::Parquet);
        let engine = self.engine();
        let Some(results) = &self.tab.grid.results else { return };
        let shown = crate::db::QueryOutcome {
            columns: results.columns.clone(),
            column_types: results.column_types.clone(),
            rows: rows.iter().filter_map(|&i| results.rows.get(i).cloned()).collect(),
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
    pub(in crate::app) fn export_all(&mut self, format: Format) {
        const PAGE: usize = 10_000;
        if self.export_task.is_some() {
            return self.set_status(t!("export.already_running").to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
        if self.tab.run.stream.is_none() {
            if results.truncated() {
                return self.set_status(t!("export.not_in_full").to_string(), true);
            }
            // All of it is here: every row, whatever a filter is showing, in
            // the order they're sorted.
            let rows = compute_view(results, "", self.tab.grid.sort);
            let note = format!(" ({})", tn!("export.rows", rows.len()));
            return self.write_rows(format, &rows, note);
        }
        if self.tab.run.fetching_more {
            return self.set_status(t!("export.still_loading").to_string(), true);
        }
        let engine = self.engine();
        // How it's written: text (`Some`), or Parquet.
        let paged = match format {
            Format::Csv => Some(crate::transfer::export::Paged::Csv),
            Format::Json => Some(crate::transfer::export::Paged::Json),
            Format::Sql => Some(crate::transfer::export::Paged::Inserts { table: self.insert_table(), engine }),
            Format::Parquet => None,
            Format::Tsv | Format::Markdown | Format::Ascii | Format::Text => {
                return self.set_status(t!("export.not_page_by_page", format = format.name()), true);
            }
        };
        let Some(stream) = self.tab.run.stream.take() else { return };
        let path = self.export_path(format);
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

/// Where rows go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// The marked rows (or those shown), to the clipboard.
    Copy,
    /// The rows shown, to a file.
    ExportShown,
    /// Every row of the result, not just those loaded, to a file.
    ExportAll,
    /// The marked rows (or those shown), in an app.
    Open,
}

/// What rows can be written as: declared in the one order every menu lists
/// them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    Markdown,
    /// A table with `+--+` borders.
    Ascii,
    /// Aligned columns, as `-e` prints them.
    Text,
    /// `INSERT` statements.
    Sql,
    /// Binary: a file only, never the clipboard.
    Parquet,
}

impl Format {
    pub const ALL: [Format; 8] = [
        Format::Csv,
        Format::Tsv,
        Format::Json,
        Format::Markdown,
        Format::Ascii,
        Format::Text,
        Format::Sql,
        Format::Parquet,
    ];

    /// Whether rows can go to `to` in this format. Every row goes page by
    /// page as it arrives, which an ASCII table can't: its widths need every
    /// row first.
    pub fn supports(self, to: Dest) -> bool {
        use Format::{Ascii, Csv, Json, Markdown, Parquet, Sql, Text, Tsv};
        match to {
            Dest::Copy => matches!(self, Csv | Tsv | Json | Markdown | Ascii | Sql),
            Dest::ExportShown => matches!(self, Csv | Json | Ascii | Sql | Parquet),
            Dest::ExportAll => matches!(self, Csv | Json | Sql | Parquet),
            Dest::Open => matches!(self, Csv | Tsv | Json | Ascii | Text | Sql),
        }
    }

    /// The formats rows can go to `to` in, in the one order.
    pub fn for_dest(to: Dest) -> impl Iterator<Item = Format> {
        Format::ALL.into_iter().filter(move |f| f.supports(to))
    }

    /// Its key in `[viewers]`.
    pub fn key(self) -> &'static str {
        match self {
            Format::Ascii => "ascii",
            other => other.ext(),
        }
    }

    /// Its file's extension.
    pub fn ext(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::Markdown => "md",
            Format::Sql => "sql",
            Format::Text | Format::Ascii => "txt",
            Format::Parquet => "parquet",
        }
    }

    /// Its name, as a status says it.
    pub fn name(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "TSV",
            Format::Json => "JSON",
            Format::Markdown => "Markdown",
            Format::Parquet => "Parquet",
            Format::Sql => t!("pipes.format_sql"),
            Format::Text => t!("pipes.format_text"),
            Format::Ascii => t!("pipes.format_ascii"),
        }
    }

    /// How a menu's "… as {format}" says it.
    pub fn phrase(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "TSV",
            Format::Json => "JSON",
            Format::Parquet => "Parquet",
            Format::Markdown => t!("menu.format_markdown"),
            Format::Sql => t!("menu.format_sql"),
            Format::Text => t!("menu.format_text"),
            Format::Ascii => t!("menu.format_ascii"),
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
