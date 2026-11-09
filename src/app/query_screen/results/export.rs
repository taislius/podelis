//! Exporting results: to a file, or all rows of a query.

use crate::app::*;
use crate::db::human_bytes;
use crate::transfer::limits::{self, Check, ExportLimits};

impl App {
    /// `podelis-export-<timestamp>.<ext>` in `export_dir` (or the current folder).
    pub(in crate::app) fn export_path(&self, format: Format) -> PathBuf {
        let ext = format.ext();
        self.export_dir().join(format!("podelis-export-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S")))
    }

    /// `export_dir`, or the current folder.
    pub(in crate::app) fn export_dir(&self) -> PathBuf {
        match self.settings.export_dir.as_deref() {
            Some(d) if d.starts_with("~/") => crate::config::settings::expand_home(d),
            Some(d) => PathBuf::from(d),
            None => PathBuf::new(),
        }
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

    fn export_limits(&self) -> ExportLimits {
        #[cfg(test)]
        if let Some(limits) = self.exports.limits_for_tests {
            return limits;
        }
        let s = &self.settings;
        ExportLimits::from_gb(s.export_min_free_gb, s.export_ask_gb, s.export_max_gb)
    }

    /// Free space where exports go, without holding the screen for a
    /// mount that doesn't answer.
    fn export_free(&self) -> Option<u64> {
        limits::free_space_within(&self.export_dir(), Duration::from_millis(300))
    }

    /// Whether `size` bytes can be written where exports go; if not, says
    /// why. `every`: of every row, which keeps the disk's floor; the rows
    /// shown only have to fit.
    fn export_fits(&mut self, size: u64, every: bool) -> bool {
        let limits = self.export_limits();
        let free = self.export_free();
        let check = if every { limits.fits(size, free) } else { limits.fits_file(size, free) };
        let why = match check {
            Check::TooBig => t!("export.too_big_now", size = human_bytes(size)),
            Check::DiskLow if every => {
                t!("export.wont_fit", free = human_bytes(free.unwrap_or(0)), keep = human_bytes(limits.min_free))
            }
            Check::DiskLow => t!("export.no_room", size = human_bytes(size), free = human_bytes(free.unwrap_or(0))),
            Check::Go | Check::Ask => return true,
        };
        self.set_status(why, true);
        false
    }

    /// Writes the rows as shown to a file in the current folder.
    pub(in crate::app) fn export(&mut self, format: Format) {
        let rows = self.tab.grid.view.clone();
        let note = self.shown_note();
        self.write_rows(format, &rows, note);
    }

    /// Writes `rows` of the result, all of them loaded, to a file; `note`
    /// says how many they are. Its size is known, so it only has to fit.
    fn write_rows(&mut self, format: Format, rows: &[usize], note: String) {
        if format == Format::Parquet {
            // Its own size isn't known until it's written: about that of its values.
            let row_bytes = self.tab.grid.results.as_ref().map_or(0.0, |r| limits::average_row_bytes(&r.rows));
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an estimate"
            )]
            let about = (rows.len() as f64 * row_bytes) as u64;
            if self.export_fits(about, false) {
                self.export_parquet(rows, note);
            }
            return;
        }
        let path = self.export_path(format);
        let Some(text) = self.rows_text(format, rows) else { return };
        if !self.export_fits(text.len() as u64, false) {
            return;
        }
        let written = std::fs::write(&path, text);
        self.export_said(&path, written.map_err(|e| e.to_string()), &note);
    }

    /// Writes `rows` to a Parquet file, each column typed by its
    /// engine's type (`transfer::parquet`), from any engine: on a blocking
    /// thread (every value is read, then encoded), the screen drawn meanwhile.
    fn export_parquet(&mut self, rows: &[usize], note: String) {
        if self.exports.task.is_some() {
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
        self.exports.task = Some(ExportRun::new(stop, path, None, None));
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
        if self.exports.task.is_some() || self.exports.estimating.is_some() {
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
        if !format.supports(Dest::ExportAll) {
            return self.set_status(t!("export.not_page_by_page", format = format.name()), true);
        }
        if self.export_fits(0, true) {
            self.estimate_export(format);
        }
    }

    /// The statement behind the rows, when a plan can be made of it beside
    /// them: asks that plan how many rows it returns, then goes on. Where
    /// it doesn't say, only the size it reaches asks.
    fn estimate_export(&mut self, format: Format) {
        const WAIT: Duration = Duration::from_secs(3);
        let limits = self.export_limits();
        let engine = self.engine();
        let sql = self.tab.last_sql.as_ref().map(|(sql, _)| sql.clone()).unwrap_or_default();
        let one = crate::language::split_statements(&sql, engine).len() == 1;
        let explain = engine.dialect().map(|d| (d, d.explain(sql.trim().trim_end_matches(';'), self.version_of_db())));
        let plan = explain.filter(|(_, explain)| {
            one && (limits.ask_every > 0 || limits.max > 0)
                && engine.dialect().is_some_and(|d| d.plans_rows(self.version_of_db()))
                && !engine.is_file_based()
                && self.connection.txn == TxnState::Idle
                && crate::sql::params::find(explain, engine).is_empty()
        });
        let (Some((dialect, explain)), Some(db)) = (plan, self.connection.database.clone()) else {
            return self.start_export_all(format, 0);
        };
        let row_bytes = self.export_row_bytes(format);
        let send = self.sender();
        // Answers only this request, about these rows: what else may have
        // happened by then is checked when it comes.
        let request = self.next_run_id();
        self.exports.estimating = Some(EstimateRequest { id: request, run: self.tab.run.id });
        self.set_status(t!("export.estimating").to_string(), false);
        tokio::spawn(async move {
            let plan = tokio::time::timeout(WAIT, db.run_aside(&explain, 10_000)).await;
            let rows =
                plan.ok().and_then(Result::ok).flatten().and_then(|plan| dialect.plan_rows(&plan.columns, &plan.rows));
            send(Background::ExportEstimated { request, format, rows, row_bytes }.into());
        });
    }

    /// About how many bytes a row takes in `format`: those loaded, written
    /// that way. (Parquet's can't be told before it's written: their values'.)
    pub(in crate::app) fn export_row_bytes(&self, format: Format) -> f64 {
        let Some(results) = &self.tab.grid.results else { return 0.0 };
        let sample: Vec<usize> = (0..results.rows.len().min(1000)).collect();
        match self.rows_text(format, &sample) {
            #[expect(clippy::cast_precision_loss, reason = "an estimate")]
            Some(text) if !sample.is_empty() => text.len() as f64 / sample.len() as f64,
            _ => limits::average_row_bytes(&results.rows),
        }
    }

    fn version_of_db(&self) -> &str {
        self.connection.database.as_ref().map_or("", |db| db.version())
    }

    /// Esc while the size is worked out: no export.
    pub(in crate::app) fn stop_estimating(&mut self) {
        self.exports.estimating = None;
        self.set_status(t!("export.not_started").to_string(), false);
    }

    /// What the plan said: an export big enough asks first.
    pub(in crate::app) fn export_estimated(&mut self, request: u64, format: Format, rows: Option<f64>, row_bytes: f64) {
        // One stopped, and asked again since: the answer of an old one.
        let Some(asked) = self.exports.estimating.filter(|asked| asked.id == request) else { return };
        self.exports.estimating = None;
        self.clear_export_note();
        // The run ids are unique across tabs: the same rows, here, still open.
        if asked.run != self.tab.run.id || self.tab.run.stream.is_none() {
            return self.set_status(t!("export.other_tab").to_string(), true);
        }
        // Another question is waiting: y there mustn't answer this.
        if self.popups.confirm.is_some() {
            return self.set_status(t!("export.other_question").to_string(), true);
        }
        let limits = self.export_limits();
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "an estimate, saturating")]
        let size = rows.map_or(0, |rows| (rows * row_bytes) as u64);
        let big = limits.ask_every > 0 && size >= limits.ask_every;
        let over = limits.max > 0 && size >= limits.max;
        let free = self.export_free();
        if !big && !over {
            return self.start_export_all(format, 0);
        }
        let shown_rows = crate::db::human_rows(rows.unwrap_or(0.0));
        let message = t!("export.ask_about", size = human_bytes(size), rows = shown_rows);
        let mut details = export_ask_details(&limits, size, free);
        details.push(t!("export.ask_waiting").to_string());
        // Agreed to about this much: it asks next after that.
        let action = ConfirmAction::ExportAll(format, size, self.tab.run.id);
        self.exports.asked = Some(Instant::now());
        self.popups.confirm = Some(Confirm { message, details, danger: false, action, offer_transaction: false });
    }

    /// The export reached a size to ask about. It waits for the answer
    /// whatever else comes up.
    pub(in crate::app) fn export_ask(&mut self, rows: u64, size: u64, free: Option<u64>) {
        if let Some(run) = &mut self.exports.task {
            run.paused = Some(ExportPause { rows, size, free });
            run.paused_said = false;
            self.offer_export_question();
        }
    }

    /// Asks "keep going?", now or as soon as no other question is open: the
    /// export waits until it's answered, so it mustn't be lost to another.
    pub(in crate::app) fn offer_export_question(&mut self) {
        let Some(run) = self.exports.task.as_mut() else { return };
        let Some(pause) = run.paused else { return };
        if let Some(open) = &self.popups.confirm {
            // Said once, not over what came since (this runs every second).
            if !matches!(open.action, ConfirmAction::KeepExporting) && !run.paused_said {
                run.paused_said = true;
                self.set_status(t!("export.paused", size = human_bytes(pause.size)), false);
            }
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "display only")]
        let shown_rows = crate::db::human_rows(pause.rows as f64);
        let message = t!("export.ask_reached", size = human_bytes(pause.size), rows = shown_rows);
        // What's written is off what's free already: nothing more to subtract.
        let mut details = export_ask_details(&self.export_limits(), 0, pause.free);
        details.push(t!("export.ask_waiting").to_string());
        let action = ConfirmAction::KeepExporting;
        self.exports.asked = Some(Instant::now());
        self.popups.confirm = Some(Confirm { message, details, danger: false, action, offer_transaction: false });
    }

    /// A key started the export, so the footer says it's on its way until
    /// the next key; what comes next is said without one, and goes on.
    pub(in crate::app) fn clear_export_note(&mut self) {
        let ours = [t!("export.exporting"), t!("export.estimating")];
        if self.notes.results_note.as_ref().is_some_and(|(note, _)| ours.contains(&note.as_str())) {
            self.notes.results_note = None;
        }
        if self.notes.editor_note.as_deref().is_some_and(|note| ours.contains(&note)) {
            self.notes.editor_note = None;
        }
    }

    pub(in crate::app) fn keep_exporting(&mut self) {
        if let Some(run) = &mut self.exports.task {
            run.paused = None;
            if let Some(resume) = &run.resume {
                resume.notify_one();
            }
        }
    }

    /// Writes every row, page by page. `agreed`: the size already agreed to.
    pub(in crate::app) fn start_export_all(&mut self, format: Format, agreed: u64) {
        if self.exports.task.is_some() {
            return self.set_status(t!("export.already_running").to_string(), true);
        }
        // Asked again, as time has passed: a page on its way would go to the
        // grid and not the file, and the stream may have gone.
        if self.tab.run.fetching_more {
            return self.set_status(t!("export.still_loading").to_string(), true);
        }
        if self.tab.run.stream.is_none() {
            return self.set_status(t!("export.other_tab").to_string(), true);
        }
        let Some(results) = &self.tab.grid.results else { return };
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
        // The stream holds its session until the export ends.
        let conn = self.tabs.get(self.active_tab).and_then(|t| t.conn);
        let path = self.export_path(format);
        let (columns, types, loaded) = (results.columns.clone(), results.column_types.clone(), results.rows.clone());
        let send = self.sender();
        let target = path.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let resume = std::sync::Arc::new(tokio::sync::Notify::new());
        let watch =
            ExportWatch::new(self.export_limits(), self.export_dir(), &target, agreed, std::sync::Arc::clone(&resume));
        let page = limits::page_rows(limits::average_row_bytes(&loaded));
        let job = ExportJob { target: target.clone(), paged, columns, types, engine, loaded, stream, page };
        tokio::spawn(async move {
            // A file with rows agreed to stays, whatever goes wrong after.
            let mut keep = false;
            let written = write_every_row(job, watch, &send, stopped, &mut keep).await;
            if written.is_err() && !keep {
                let _ = std::fs::remove_file(&target);
            }
            send(Background::ExportDone(target, written.map_err(|e| format!("{e:#}"))).into());
        });
        self.exports.task = Some(ExportRun::new(stop, path, Some(resume), conn));
        self.set_status(t!("export.exporting").to_string(), false);
    }

    /// Stops a running export; it removes its unfinished file.
    pub(in crate::app) fn stop_export(&mut self) {
        // Its question, if it's on the screen, is about nothing now.
        if self.popups.confirm.as_ref().is_some_and(|c| matches!(c.action, ConfirmAction::KeepExporting)) {
            self.popups.confirm = None;
        }
        let Some(run) = self.exports.task.take() else { return };
        let _ = run.stop.send(());
        self.set_status(t!("export.stopped").to_string(), false);
    }
}

/// A size being worked out: the request's number, and the run whose rows
/// are to be exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EstimateRequest {
    pub id: u64,
    pub run: u64,
}

/// An export being written, with all that's about it: what stops it, its
/// file, what wakes it from a question, and that question.
pub struct ExportRun {
    stop: tokio::sync::oneshot::Sender<()>,
    pub path: PathBuf,
    resume: Option<std::sync::Arc<tokio::sync::Notify>>,
    pub paused: Option<ExportPause>,
    /// The pause was said while another question was open.
    paused_said: bool,
    /// The connection whose session it reads every row on (none: the rows
    /// already loaded): no query runs there meanwhile.
    pub conn: Option<u64>,
}

impl ExportRun {
    fn new(
        stop: tokio::sync::oneshot::Sender<()>,
        path: PathBuf,
        resume: Option<std::sync::Arc<tokio::sync::Notify>>,
        conn: Option<u64>,
    ) -> Self {
        ExportRun { stop, path, resume, paused: None, paused_said: false, conn }
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        ExportRun::new(tokio::sync::oneshot::channel().0, PathBuf::from("x"), None, None)
    }
}

/// How far an export got when it stopped to ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportPause {
    pub rows: u64,
    pub size: u64,
    pub free: Option<u64>,
}

/// Parquet's rows on their way to its file, beside it.
fn scratch_path(target: &std::path::Path) -> PathBuf {
    target.with_extension("parquet.rows")
}

/// What an export writing page by page holds itself to.
struct ExportWatch {
    limits: ExportLimits,
    dir: PathBuf,
    target: PathBuf,
    scratch: PathBuf,
    next_ask: u64,
    resume: std::sync::Arc<tokio::sync::Notify>,
    /// It has stopped reading the rows to wait for an answer: what's
    /// written by then was agreed to.
    waited: bool,
    /// And no page has come since, so an error now may be the wait's.
    just_waited: bool,
}

impl ExportWatch {
    fn new(
        limits: ExportLimits,
        dir: PathBuf,
        target: &std::path::Path,
        agreed: u64,
        resume: std::sync::Arc<tokio::sync::Notify>,
    ) -> Self {
        let scratch = scratch_path(target);
        ExportWatch {
            limits,
            dir,
            target: target.to_path_buf(),
            scratch,
            next_ask: limits.next_ask(agreed),
            resume,
            // Agreeing to its size before it started isn't a pause: nothing
            // was written then. The stream did wait for that answer, though.
            waited: false,
            just_waited: agreed > 0,
        }
    }

    /// Says how far it's got, and waits for an answer if it has to. Not
    /// after the `last` page: all of it is written by then. `Some`: it
    /// stops here, and why (over the largest file, or the disk's floor).
    async fn after_page(
        &mut self,
        rows: u64,
        last: bool,
        send: &impl Fn(DbEventKind),
        stopped: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> anyhow::Result<Option<String>> {
        let (size, free) = self.measure().await;
        send(Background::ExportProgress { rows, size, free }.into());
        if last {
            return Ok(None);
        }
        match self.limits.check(size, free, self.next_ask) {
            Check::Go => {}
            Check::TooBig => return Ok(Some(t!("export.too_big", size = human_bytes(size)))),
            Check::DiskLow => return Ok(Some(disk_low(free, self.limits.min_free))),
            Check::Ask => {
                send(Background::ExportAsk { rows, size, free }.into());
                tokio::select! {
                    () = self.resume.notified() => {}
                    _ = stopped => anyhow::bail!("stopped"),
                }
                self.next_ask = self.limits.next_ask(size);
                self.waited = true;
                self.just_waited = true;
            }
        }
        Ok(None)
    }

    /// Parquet's file alone, as it's written from the rows beside it, and
    /// the free space: read off the runtime's threads (a slow mount).
    async fn measure_final(&self) -> (u64, Option<u64>) {
        let (dir, target) = (self.dir.clone(), self.target.clone());
        let read = move || (std::fs::metadata(&target).map_or(0, |m| m.len()), limits::free_space(&dir));
        tokio::task::spawn_blocking(read).await.unwrap_or((0, None))
    }

    /// The file's size so far and the free space, read off the runtime's threads.
    async fn measure(&self) -> (u64, Option<u64>) {
        let (dir, target, scratch) = (self.dir.clone(), self.target.clone(), self.scratch.clone());
        let read = move || (limits::written(&target, &scratch), limits::free_space(&dir));
        tokio::task::spawn_blocking(read).await.unwrap_or((0, None))
    }
}

/// What an export of every row writes, and where.
struct ExportJob {
    target: PathBuf,
    paged: Option<crate::transfer::export::Paged>,
    columns: Vec<String>,
    types: Vec<String>,
    engine: Engine,
    loaded: Vec<Vec<Option<String>>>,
    stream: RowStream,
    /// Rows a page.
    page: usize,
}

/// Writes the rows loaded and those still to come. On an error the file is
/// removed, unless `keep` says it holds what was agreed to. The stream goes
/// with this, which cancels the query.
async fn write_every_row(
    job: ExportJob,
    mut watch: ExportWatch,
    send: &impl Fn(DbEventKind),
    mut stopped: tokio::sync::oneshot::Receiver<()>,
    keep: &mut bool,
) -> anyhow::Result<u64> {
    let ExportJob { target, paged, columns, types, engine, loaded, stream, page: page_rows } = job;
    let parquet = paged.is_none();
    let out = open_sink(&target, paged, &columns, &types, engine)?;
    // Encoding (Parquet's above all) is work for a blocking
    // thread, not for the runtime's workers.
    let mut out = tokio::task::spawn_blocking(move || {
        let mut out = out;
        out.page(loaded).map(|()| out)
    })
    .await??;
    // Why it ended before the last row, with its file to keep.
    let mut cut_short = None;
    loop {
        let page = tokio::select! {
            page = stream.next_page(page_rows) => page,
            _ = &mut stopped => anyhow::bail!("stopped"),
        };
        let page = match page {
            Ok(page) => page,
            Err(e) if watch.waited => {
                tracing::warn!("export: cut short: {e}");
                // The connection broken straight after a wait: most likely the
                // wait's doing. Anything else is what the server said.
                let lost = watch.just_waited && stream.broke();
                cut_short = Some(if lost { t!("export.lost_waiting").to_string() } else { one_line(&e) });
                break;
            }
            Err(_) if watch.just_waited && stream.broke() => anyhow::bail!(t!("export.lost_waiting")),
            Err(e) => anyhow::bail!(e),
        };
        watch.just_waited = false;
        let done = page.done;
        let (back, wrote) = tokio::task::spawn_blocking(move || {
            let mut out = out;
            let wrote = out.page(page.rows);
            (out, wrote)
        })
        .await?;
        out = back;
        // What was agreed to stays, however it ends after (Parquet's is
        // only a file once finished, so it can't).
        let stop = match wrote {
            Ok(()) => watch.after_page(out.rows(), done, send, &mut stopped).await?,
            Err(e) if watch.waited && !parquet => Some(e.to_string()),
            Err(e) => return Err(e.into()),
        };
        if let Some(why) = stop {
            if !watch.waited {
                anyhow::bail!(removed(&why));
            }
            cut_short = Some(why);
            break;
        }
        if done {
            break;
        }
    }
    drop(stream);
    let rows = out.rows();
    // Text is all on disk by now; Parquet's file is written here, all of it,
    // watched as the pages were, and a stop ends it between row groups.
    let halt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&halt);
    let mut finishing = tokio::task::spawn_blocking(move || out.finish(&flag));
    let mut every_second = tokio::time::interval(Duration::from_secs(1));
    let finished: anyhow::Result<()> = loop {
        tokio::select! {
            done = &mut finishing => break done.map_err(anyhow::Error::from).and_then(|r| r.map_err(Into::into)),
            _ = &mut stopped => {
                halt.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = finishing.await;
                anyhow::bail!("stopped");
            }
            _ = every_second.tick(), if parquet => {
                let (size, free) = watch.measure_final().await;
                let why = if watch.limits.max > 0 && size >= watch.limits.max {
                    Some(t!("export.too_big", size = human_bytes(size)))
                } else if !watch.limits.has_room(0, free) {
                    Some(disk_low(free, watch.limits.min_free))
                } else {
                    None
                };
                if let Some(why) = why {
                    halt.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = finishing.await;
                    anyhow::bail!(removed(&why));
                }
            }
        }
    };
    match (finished, cut_short) {
        (Ok(()), None) => Ok(rows),
        // Kept only once it's whole as far as it goes, and named as a part.
        (Ok(()), Some(why)) => {
            *keep = true;
            Err(keep_partial(&target, rows, &why))
        }
        // Text cut short, its last write failing too: what's there stays.
        (Err(e), Some(why)) if !parquet => {
            *keep = true;
            Err(keep_partial(&target, rows, &format!("{why}; {e:#}")))
        }
        (Err(e), _) => Err(e),
    }
}

/// The file every row goes to: text page by page (`Some`), or Parquet.
fn open_sink(
    target: &std::path::Path,
    paged: Option<crate::transfer::export::Paged>,
    columns: &[String],
    types: &[String],
    engine: Engine,
) -> anyhow::Result<Sink<std::io::BufWriter<std::fs::File>>> {
    let file = std::io::BufWriter::new(std::fs::File::create(target)?);
    Ok(match paged {
        Some(paged) => Sink::Text(crate::transfer::export::PagedWriter::new(file, paged, columns, types)),
        None => Sink::Parquet(Box::new(crate::transfer::parquet::Pages::new(
            file,
            columns,
            types,
            engine,
            scratch_path(target),
        )?)),
    })
}

/// Names a finished file as the part it is, and says so. (If it can't be
/// renamed it's still there, under its own name.)
fn keep_partial(target: &std::path::Path, rows: u64, why: &str) -> anyhow::Error {
    let partial = partial_path(target);
    let kept = if std::fs::rename(target, &partial).is_ok() { partial } else { target.to_path_buf() };
    let shown = std::fs::canonicalize(&kept).unwrap_or(kept);
    anyhow::anyhow!(t!("export.kept_partial", why = why, rows = rows, path = shown.display()))
}

/// Where an export cut short is kept: `a.csv` as `a.partial.csv`.
fn partial_path(path: &std::path::Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let ext = path.extension().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}.partial.{ext}"))
}

/// Why an export stopped, its file gone with it.
fn removed(why: &str) -> String {
    t!("export.removed", why = why)
}

fn disk_low(free: Option<u64>, keep: u64) -> String {
    t!("export.disk_low", free = human_bytes(free.unwrap_or(0)), keep = human_bytes(keep))
}

/// What a question about an export's size says beside it.
fn export_ask_details(limits: &ExportLimits, size: u64, free: Option<u64>) -> Vec<String> {
    let mut details = Vec::new();
    match free {
        Some(free) => details.push(t!("export.ask_free", free = human_bytes(free))),
        None if limits.min_free > 0 => details.push(t!("export.ask_free_unknown").to_string()),
        None => {}
    }
    if !limits.has_room(size, free) {
        details.push(t!("export.ask_wont_fit", keep = human_bytes(limits.min_free)));
    } else if limits.min_free > 0 && free.is_some() {
        details.push(t!("export.ask_kept", keep = human_bytes(limits.min_free)));
    }
    if limits.max > 0 {
        details.push(t!("export.ask_stops_at", size = human_bytes(limits.max)));
    }
    details
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_file_says_so_and_keeps_its_type() {
        let partial = partial_path(std::path::Path::new("/x/podelis-export-1.csv"));
        assert_eq!(partial, PathBuf::from("/x/podelis-export-1.partial.csv"));
    }
}

/// Exports of every row.
#[derive(Default)]
pub(in crate::app) struct Exports {
    /// "Export all rows" in progress: what stops it, and the file it writes.
    pub(in crate::app) task: Option<ExportRun>,
    /// Limits small enough to reach in a test (the settings are in GB).
    #[cfg(test)]
    pub(in crate::app) limits_for_tests: Option<crate::transfer::limits::ExportLimits>,
    /// How big a coming export would be is being worked out.
    pub(in crate::app) estimating: Option<EstimateRequest>,
    /// When a question about an export's size came up by itself.
    pub(in crate::app) asked: Option<std::time::Instant>,
    /// What the export form was last run with, where it starts next time.
    pub(in crate::app) choice: super::export_form::ExportChoice,
}
