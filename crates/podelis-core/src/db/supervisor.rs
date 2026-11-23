//! The connection's supervisor: one task per [`Database`] that owns its
//! session connection, and runs what's asked of the session one piece at a
//! time (Alice Ryhl's actor: the task owns the state, the `Database` is its
//! handle).
//!
//! What's asked is work on the session (a run, a catalog query, a check),
//! sent with [`Database::with_session`] and answered on its own channel. A
//! streamed result is one long piece of work: it holds the session while
//! its rows are read, as the session's lock did. Two things don't queue:
//! a cancel (sent to the server on a pool connection, or by the engine's
//! interrupt), and giving up on a session that doesn't answer
//! ([`Database::abandon_session`]), which stops the work running on it.
//!
//! Between pieces of work it keeps the connection: with a heartbeat set
//! ([`Database::set_heartbeat`]) it checks an idle one; a lost one it
//! checks again at growing waits (5 s, twice as long each time, up to a
//! minute, a little jittered), reopening its session unless a transaction
//! is open on it, and pauses after [`LOST_CHECKS`] failed checks rather
//! than trying for hours (a check asked for, or a run, goes on). Each
//! change is a report ([`Health`]).
//!
//! The task ends when every handle is gone: its mailbox closes, and the
//! session goes with it.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Weak;

use futures_util::FutureExt as _;

use super::*;

/// A future borrowing the session.
pub(super) type SessionFuture<'s, T> = Pin<Box<dyn Future<Output = T> + Send + 's>>;

/// Work on the session, as the supervisor runs it.
type Work = Box<dyn for<'s> FnOnce(&'s mut Session) -> SessionFuture<'s, ()> + Send>;

/// How long a check waits for the server (shorter in this crate's tests,
/// whose links die on purpose).
pub const CHECK_WITHIN: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(5) };
/// A lost connection's first wait, and its longest.
const FIRST_LOST_WAIT: Duration = if cfg!(test) { Duration::from_millis(100) } else { Duration::from_secs(5) };
const LONGEST_LOST_WAIT: Duration = if cfg!(test) { Duration::from_millis(1200) } else { Duration::from_secs(60) };
/// Failed checks of a lost connection before the supervisor pauses.
pub const LOST_CHECKS: u32 = 5;
/// How soon after the last check something the user does checks a lost
/// connection again ([`Database::nudge`]).
const RECHECK_AFTER_INPUT: Duration = Duration::from_secs(2);

/// What the supervisor knows of its connection, said as it happens: the
/// one source of whether it's lost (the app no longer reads it from error
/// text). Each change is a report; `reconnects` counts the sessions
/// reopened, so two reconnects in a row are two reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub state: HealthState,
    pub reconnects: u64,
    /// While lost: when it's checked next (`None` while a check runs, or
    /// paused).
    pub next_check: Option<Instant>,
    /// How long its last check took to be answered.
    pub latency: Option<Duration>,
    /// What the old session had that the last reconnect's new one hasn't
    /// (`database shop`, `sql_mode`, temporary tables): nothing's run again,
    /// it's the user's to set again.
    pub lost: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthState {
    /// It answers (the last work on it did).
    Live,
    /// It doesn't answer, and couldn't be reopened yet: why.
    Lost(String),
    /// Lost, and checked [`LOST_CHECKS`] times without an answer: not
    /// checked again until asked ([`Database::check_now`]) or a run.
    Paused(String),
    /// Its session was reopened: what was on the old one (a transaction,
    /// `SET`, `USE`) is gone.
    Reconnected,
    /// Closed (`Database::close`).
    Closed,
}

impl Health {
    /// A report of `state`, with nothing else said.
    pub fn of(state: HealthState) -> Self {
        Health { state, reconnects: 0, next_check: None, latency: None, lost: Vec::new() }
    }
}

impl HealthState {
    /// Lost, paused or not.
    pub fn is_lost(&self) -> bool {
        matches!(self, HealthState::Lost(_) | HealthState::Paused(_))
    }
}

/// How many pieces of work may wait for the session. A full mailbox means
/// the app asks faster than one session can answer, which it shouldn't:
/// the sender then waits.
const MAILBOX: usize = 32;

struct Job {
    work: Work,
    /// A streamed result's: stopped when the session is given up on.
    stream: bool,
    /// Ends when its caller stops waiting for it (`None` for a stream's,
    /// which has no one caller): only then does a give-up stop it, so a
    /// give-up that comes late doesn't stop the work after.
    gone: Option<oneshot::Receiver<()>>,
    /// The connection's close: the one job run once it's closing.
    closes: bool,
}

/// The handle's side of the supervisor.
pub(super) struct Handle {
    jobs: mpsc::Sender<Job>,
    /// Bumped to stop the work running on a session given up on.
    abandon: watch::Sender<u64>,
    /// How often to check an idle connection (`None`: no heartbeat).
    heartbeat: watch::Sender<Option<Duration>>,
    /// Something was done while it's lost (or a check is wanted now).
    nudge: Arc<tokio::sync::Notify>,
    /// What the supervisor and its handles share.
    shared: Arc<Shared>,
    /// Its reports.
    health: watch::Sender<Health>,
}

/// Set by the handles, read by the supervisor, or the other way round.
#[derive(Default)]
struct Shared {
    /// The supervisor is running work now.
    busy: AtomicBool,
    /// The connection is closing: no work but the close runs, and what
    /// runs is stopped.
    closing: AtomicBool,
    /// The work just done was skipped (its caller gone before its turn):
    /// nothing was sent, so the session isn't known to be alive any later.
    skipped: AtomicBool,
    /// Pieces of work sent and not done yet (waiting or running).
    queued: std::sync::atomic::AtomicUsize,
    /// …and that work is a streamed result's.
    streaming: AtomicBool,
    /// …reading rows now, or stopping them: held this long, it's stuck on
    /// the link; waiting for its next page to be asked for, it's only an
    /// open result.
    reading: AtomicBool,
    /// A transaction is open on the session (the app knows, and says): a
    /// check then never reopens it, which would lose the transaction
    /// without a word.
    in_transaction: AtomicBool,
    /// A check was asked for: the waits start again from the first, and a
    /// pause ends.
    resume: AtomicBool,
    /// …or wanted at once, the next time the session is free.
    check_now: AtomicBool,
    /// When work last ran on the session (or a check, or it opened), for
    /// the idle time.
    last_activity: std::sync::Mutex<Option<Instant>>,
}

impl Shared {
    /// How long since work last ran on the session.
    fn idle_for(&self) -> Duration {
        self.last_activity.lock().ok().and_then(|at| *at).map_or(Duration::MAX, |at| at.elapsed())
    }
}

impl Handle {
    /// Starts the supervisor of `session`; `db` is its connection, which it
    /// checks through while one is (its handles are what keep it).
    pub(super) fn spawn(session: Session, db: Weak<Inner>) -> Self {
        let (jobs, mailbox) = mpsc::channel(MAILBOX);
        let (abandon, abandoned) = watch::channel(0);
        let (heartbeat, heartbeats) = watch::channel(None);
        let (health, reports) = watch::channel(Health::of(HealthState::Live));
        let nudge = Arc::new(tokio::sync::Notify::new());
        // Idle from when the session opened (a first run on a new
        // connection isn't checked first).
        let shared =
            Arc::new(Shared { last_activity: std::sync::Mutex::new(Some(Instant::now())), ..Shared::default() });
        let supervisor = Supervisor {
            session,
            db,
            abandoned,
            heartbeats,
            reports,
            nudge: Arc::clone(&nudge),
            shared: Arc::clone(&shared),
            last_check: None,
            lost_checks: 0,
            lost_due: None,
        };
        tokio::spawn(supervisor.run(mailbox));
        Handle { jobs, abandon, heartbeat, nudge, shared, health }
    }

    pub(super) fn streaming(&self) -> bool {
        self.shared.streaming.load(Ordering::SeqCst)
    }

    /// A stream's rows are being read now, or stopped (not waiting for the
    /// next page to be asked for).
    pub(super) fn set_reading(&self, on: bool) {
        self.shared.reading.store(on, Ordering::SeqCst);
    }

    /// The work running now was skipped (its caller gone): nothing sent.
    pub(super) fn skipped(&self) {
        self.shared.skipped.store(true, Ordering::SeqCst);
    }

    /// A transaction is open on the session, as the work on it saw (the
    /// app's word, `note_transaction`, comes after and stands).
    pub(super) fn set_in_transaction(&self, open: bool) {
        self.shared.in_transaction.store(open, Ordering::SeqCst);
    }

    pub(super) fn reading(&self) -> bool {
        self.shared.reading.load(Ordering::SeqCst)
    }

    /// Stops the work running on the session, if any (a run or a stream's
    /// rows stuck on a dead link).
    pub(super) fn stop_work(&self) {
        self.abandon.send_modify(|n| *n += 1);
    }
}

struct Supervisor {
    session: Session,
    db: Weak<Inner>,
    abandoned: watch::Receiver<u64>,
    heartbeats: watch::Receiver<Option<Duration>>,
    reports: watch::Receiver<Health>,
    nudge: Arc<tokio::sync::Notify>,
    shared: Arc<Shared>,
    /// When it was last checked.
    last_check: Option<Instant>,
    /// Checks in a row that found it lost.
    lost_checks: u32,
    /// A lost connection's next check, jittered once per wait (the
    /// countdown and the wait are the same), with the check and count it
    /// follows.
    lost_due: Option<(Option<Instant>, u32, Instant)>,
}

impl Supervisor {
    /// Each piece of work in turn, and the checks between them, until every
    /// handle is gone.
    async fn run(mut self, mut mailbox: mpsc::Receiver<Job>) {
        loop {
            let due = self.check_due();
            self.report_next_check(due);
            let wait = async move {
                match due {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                job = mailbox.recv() => match job {
                    Some(job) => {
                        self.work(job).await;
                        // A check asked for while it waited, once no more does.
                        if self.shared.queued.load(Ordering::SeqCst) == 0
                            && self.shared.check_now.swap(false, Ordering::SeqCst)
                        {
                            self.check().await;
                        }
                    }
                    None => break,
                },
                () = wait => self.check().await,
                () = self.nudge.notified() => {
                    let lost = self.reports.borrow().state.is_lost();
                    let paused = matches!(self.reports.borrow().state, HealthState::Paused(_));
                    let since = self.last_check.map_or(Duration::MAX, |at| at.elapsed());
                    let wanted = self.shared.check_now.swap(false, Ordering::SeqCst);
                    if wanted && self.shared.queued.load(Ordering::SeqCst) > 0 {
                        // Work waits: it goes first, and the check right after.
                        self.shared.check_now.store(true, Ordering::SeqCst);
                    } else if wanted || (lost && !paused && since >= RECHECK_AFTER_INPUT) {
                        self.check().await;
                    }
                }
                _ = self.heartbeats.changed() => {}
            }
        }
    }

    async fn work(&mut self, job: Job) {
        // Only a give-up from now on stops it.
        self.abandoned.borrow_and_update();
        // Closing: work but the close isn't run (its caller hears the
        // session's work stopped). Looked at after the give-ups seen so
        // far: a close's comes after it's closing.
        if self.shared.closing.load(Ordering::SeqCst) && !job.closes {
            drop(job);
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        self.shared.busy.store(true, Ordering::SeqCst);
        // Work that panics (a driver's, say) doesn't end the supervisor:
        // its caller hears the session's work stopped, and the session is
        // given up on (its state unknown). Work the session's given up on
        // meanwhile (a run, or a stream's rows, stuck on a dead link: Esc
        // again) is stopped, so the next run can open a new session.
        self.shared.streaming.store(job.stream, Ordering::SeqCst);
        let mut gone = job.gone;
        let mut given_up = false;
        let panicked = {
            let work = AssertUnwindSafe((job.work)(&mut self.session)).catch_unwind();
            tokio::pin!(work);
            loop {
                tokio::select! {
                    done = &mut work => break done.is_err(),
                    // A stream's rows stop at once; other work once its caller's
                    // gone too (Esc drops the run first): not a later job's, if
                    // the give-up came after its own was done.
                    _ = self.abandoned.changed(), if !given_up => {
                        if gone.is_none() || self.shared.closing.load(Ordering::SeqCst) {
                            break false;
                        }
                        given_up = true;
                    }
                    () = async { if let Some(g) = gone.as_mut() { let _ = g.await; } }, if given_up => break false,
                }
            }
        };
        self.shared.streaming.store(false, Ordering::SeqCst);
        self.shared.reading.store(false, Ordering::SeqCst);
        if panicked {
            self.panicked();
        }
        self.shared.busy.store(false, Ordering::SeqCst);
        self.shared.queued.fetch_sub(1, Ordering::SeqCst);
        // Activity only if something was sent.
        if !self.shared.skipped.swap(false, Ordering::SeqCst) {
            self.touch();
        }
        // A check asked for (`check_now`) runs as work: it starts the waits
        // again, and ends a pause.
        if self.shared.resume.swap(false, Ordering::SeqCst) {
            self.lost_checks = 0;
            self.last_check = Some(Instant::now());
        }
        if !self.reports.borrow().state.is_lost() {
            self.lost_checks = 0;
        }
    }

    /// Work on the session panicked: the session is given up on (the next
    /// run opens a new one), and said lost meanwhile.
    ///
    /// A database that lives in the session (SQLite's `:memory:`) keeps it:
    /// a new one would be empty.
    fn panicked(&self) {
        let Some(inner) = self.db.upgrade() else { return };
        let db = Database(inner);
        if db.0.owns_data {
            tracing::error!("work on the session panicked: the session is kept (its database lives in it)");
            return;
        }
        tracing::error!("work on the session panicked: giving the session up");
        db.0.stale.store(true, Ordering::SeqCst);
        db.report_lost("an internal error stopped the session's work");
    }

    fn touch(&self) {
        if let Ok(mut at) = self.shared.last_activity.lock() {
            *at = Some(Instant::now());
        }
    }

    fn idle_for(&self) -> Duration {
        self.shared.idle_for()
    }

    /// When the next check is due: a lost connection's after its wait, an
    /// idle one's after the heartbeat; none while paused, or with no
    /// heartbeat and nothing lost.
    fn check_due(&mut self) -> Option<Instant> {
        // Closing: no check (one due would be met at once, and again).
        if self.shared.closing.load(Ordering::SeqCst) {
            return None;
        }
        let state = self.reports.borrow().state.clone();
        match state {
            HealthState::Paused(_) | HealthState::Closed => None,
            HealthState::Lost(_) => {
                // The same wait until a check is made, not a new jitter each
                // time the loop wakes.
                if let Some((after, count, due)) = self.lost_due
                    && after == self.last_check
                    && count == self.lost_checks
                {
                    return Some(due);
                }
                let due = self.last_check.unwrap_or_else(Instant::now) + lost_wait(self.lost_checks);
                self.lost_due = Some((self.last_check, self.lost_checks, due));
                Some(due)
            }
            HealthState::Live | HealthState::Reconnected => {
                let every = (*self.heartbeats.borrow())?;
                let idle = self.idle_for().min(every);
                Some(Instant::now() + every.saturating_sub(idle))
            }
        }
    }

    /// While lost, when it's checked next (`due`), for the countdown.
    fn report_next_check(&self, due: Option<Instant>) {
        let lost = matches!(self.reports.borrow().state, HealthState::Lost(_));
        let next = if lost { due } else { None };
        let Some(db) = self.db.upgrade() else { return };
        db.supervisor.health.send_if_modified(|h| {
            let changed = h.next_check != next;
            h.next_check = next;
            changed
        });
    }

    /// Checks the connection, as work of its own: answers, or doesn't.
    async fn check(&mut self) {
        if self.shared.closing.load(Ordering::SeqCst) {
            return;
        }
        let Some(inner) = self.db.upgrade() else { return };
        let db = Database(inner);
        // Counted as work, so nothing takes the session as free meanwhile;
        // claimed as `with_free_session` claims it (one or the other): work
        // claimed or waiting goes first, and tries the link itself.
        if self.shared.queued.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            // Counted as made: the next is a wait from now (the loop isn't
            // woken again at once while that work comes).
            self.last_check = Some(Instant::now());
            return;
        }
        self.shared.busy.store(true, Ordering::SeqCst);
        db.0.supervisor.health.send_if_modified(|h| std::mem::take(&mut h.next_check).is_some());
        let check = AssertUnwindSafe(db.check_session(&mut self.session, false)).catch_unwind().await;
        let answered = if let Ok(answer) = check {
            answer.is_ok()
        } else {
            self.panicked();
            false
        };
        self.shared.busy.store(false, Ordering::SeqCst);
        self.shared.queued.fetch_sub(1, Ordering::SeqCst);
        self.last_check = Some(Instant::now());
        self.touch();
        if answered {
            self.lost_checks = 0;
            return;
        }
        self.lost_checks = self.lost_checks.saturating_add(1);
        if self.lost_checks >= LOST_CHECKS {
            db.report_paused();
        }
    }
}

/// How long a lost connection waits for its next check after `failed`
/// checks: 5 s, twice as long each time, up to a minute, and a tenth either
/// way so connections lost together aren't checked together.
pub fn lost_wait(failed: u32) -> Duration {
    let base = (FIRST_LOST_WAIT * (1u32 << failed.min(4))).min(LONGEST_LOST_WAIT);
    // A tenth either way, from the clock (no generator needed for this).
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let spread = f64::from(nanos % 2001) / 10_000.0 - 0.1;
    base.mul_f64(1.0 + spread)
}

impl Database {
    /// The connection's reports, as they change (the current one first).
    pub fn health(&self) -> watch::Receiver<Health> {
        self.0.supervisor.health.subscribe()
    }

    /// Checks an idle connection every `every` (`None`: only a lost one is
    /// checked).
    pub fn set_heartbeat(&self, every: Option<Duration>) {
        self.0.supervisor.heartbeat.send_if_modified(|h| {
            let changed = *h != every;
            *h = every;
            changed
        });
    }

    /// Whether a transaction is open on the session (the app tracks it): a
    /// check never reopens a session with one open.
    pub fn note_transaction(&self, open: bool) {
        let was_open = self.0.supervisor.shared.in_transaction.swap(open, Ordering::SeqCst);
        // Lost while its transaction was open, and the transaction's done
        // with now: nothing's left on the old session to keep, so the next
        // check opens a new one rather than waiting on it.
        if was_open && !open && self.0.supervisor.health.borrow().state.is_lost() {
            self.0.stale.store(true, Ordering::SeqCst);
        }
    }

    /// Whether a transaction is open on the session, as the app last said.
    pub fn in_transaction(&self) -> bool {
        self.0.supervisor.shared.in_transaction.load(Ordering::SeqCst)
    }

    /// Something was done while it's lost (a key, a click): checked again
    /// soon, unless it was just checked or it's paused.
    pub fn nudge(&self) {
        self.0.supervisor.nudge.notify_one();
    }

    /// Checked as soon as the session is free (the heartbeat switched on).
    pub fn check_soon(&self) {
        self.0.supervisor.shared.check_now.store(true, Ordering::SeqCst);
        self.0.supervisor.nudge.notify_one();
    }

    /// Checks the connection now, in the session's turn (`r`): its latency
    /// and whether its session was reopened, or why it doesn't answer. The
    /// waits start again from the first, and a pause ends.
    pub async fn check_now(&self) -> Result<(Duration, bool)> {
        self.check_in_turn(None).await?.ok_or_else(closed)
    }

    /// `check_now`, waiting at most `turn` for the session (a result's rows
    /// still open hold it): `None` if it didn't come (the check isn't run).
    /// Once it's started, the check is waited for, whatever it takes.
    pub async fn check_within_turn(&self, turn: Duration) -> Result<Option<(Duration, bool)>> {
        self.check_in_turn(Some(turn)).await
    }

    async fn check_in_turn(&self, turn: Option<Duration>) -> Result<Option<(Duration, bool)>> {
        let db = self.clone();
        // No `turn`: waited for as long as it takes.
        let turn = turn.unwrap_or(Duration::from_hours(365 * 24));
        let check = self.with_session_within(turn, move |session| {
            Box::pin(async move {
                // Its turn came (given up on before it, nothing changes).
                db.0.supervisor.shared.resume.store(true, Ordering::SeqCst);
                db.check_session(session, true).await
            })
        });
        check.await?.transpose()
    }

    /// Runs `work` on the session in its turn, waiting at most `turn` for
    /// that turn: `None` if it didn't come (it isn't run then). Once it's
    /// started, its answer is waited for, however long it takes; one
    /// that's there at once (a connection closed) is that, not "busy".
    pub(super) async fn with_session_within<T, F>(&self, turn: Duration, work: F) -> Result<Option<T>>
    where
        T: Send + 'static,
        F: for<'s> FnOnce(&'s mut Session) -> SessionFuture<'s, T> + Send + 'static,
    {
        // Started or given up on, one or the other: a time-out as it starts
        // doesn't say "not its turn" while it runs after all.
        const WAITING: u8 = 0;
        const STARTED: u8 = 1;
        const GIVEN_UP: u8 = 2;
        let state = Arc::new(std::sync::atomic::AtomicU8::new(WAITING));
        let starting = Arc::clone(&state);
        let (started, has_started) = oneshot::channel::<()>();
        let job = self.with_session(move |session| {
            Box::pin(async move {
                if starting.compare_exchange(WAITING, STARTED, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                    return None;
                }
                let _ = started.send(());
                Some(work(session).await)
            })
        });
        tokio::pin!(job);
        tokio::select! {
            biased;
            done = &mut job => done,
            came = tokio::time::timeout(turn, has_started) => match came {
                Ok(Ok(())) => job.await,
                _ if state.compare_exchange(WAITING, GIVEN_UP, Ordering::SeqCst, Ordering::SeqCst).is_ok() => Ok(None),
                // It started just then: its answer.
                _ => job.await,
            },
        }
    }

    /// How long since work last ran on the session.
    pub fn idle_for(&self) -> Duration {
        self.0.supervisor.shared.idle_for()
    }

    /// The check itself: the engine's ping statement, reopening a session
    /// that was given up on or dropped (unless a transaction is open: then
    /// only asked whether it answers). No answer within [`CHECK_WITHIN`]
    /// while lost gives the session up, so the next check opens a new one
    /// rather than waiting on a dead link; the first time, it's only said.
    /// `asked`: a check asked for (`r`), which ends a pause whatever it
    /// finds (still lost, the checks go on).
    async fn check_session(&self, session: &mut Session, asked: bool) -> Result<(Duration, bool)> {
        let started = Instant::now();
        let was_lost = self.0.supervisor.health.borrow().state.is_lost();
        let in_transaction = self.0.supervisor.shared.in_transaction.load(Ordering::SeqCst);
        // A protocol ping, not a statement: it leaves the session as the
        // last run left it (MySQL's warnings, `ROW_COUNT()`), and can't fail
        // in an aborted transaction. With no answer, the session (and its
        // transaction, if the link comes back) is left be; a link that broke
        // at once has lost any transaction already, on the server: then the
        // session's reopened (and pinged there), and the reconnect says so.
        // One given up on before is reopened at once.
        let answer = if self.0.stale.load(Ordering::SeqCst) {
            tokio::time::timeout(CHECK_WITHIN, self.ping_on(session)).await.ok()
        } else {
            match session.answer_within(CHECK_WITHIN).await {
                crate::db::run::Answer::Yes => Some(Ok(false)),
                crate::db::run::Answer::Broke => {
                    self.0.stale.store(true, Ordering::SeqCst);
                    tokio::time::timeout(CHECK_WITHIN, self.ping_on(session)).await.ok()
                }
                crate::db::run::Answer::Silent => None,
            }
        };
        let latency = started.elapsed();
        // `None`: no answer within the check's time.
        match answer {
            Some(Ok(reconnected)) => {
                if !reconnected {
                    self.report_live();
                }
                self.0.supervisor.health.send_if_modified(|h| {
                    let changed = h.latency != Some(latency);
                    h.latency = Some(latency);
                    changed
                });
                Ok((latency, reconnected))
            }
            Some(Err(e)) => {
                self.report_lost_after(&error_message(&e), asked);
                Err(e)
            }
            None => {
                if was_lost && !in_transaction {
                    self.0.stale.store(true, Ordering::SeqCst);
                }
                let why = format!("no answer in {} s", CHECK_WITHIN.as_secs());
                self.report_lost_after(&why, asked);
                Err(anyhow::anyhow!(why).context(errors::NoAnswer))
            }
        }
    }

    /// It answered: live again if it wasn't.
    pub(super) fn report_live(&self) {
        self.0.supervisor.health.send_if_modified(|h| {
            let changed = matches!(h.state, HealthState::Lost(_) | HealthState::Paused(_) | HealthState::Reconnected);
            if changed {
                h.state = HealthState::Live;
                h.next_check = None;
            }
            changed
        });
    }

    /// It doesn't answer, and couldn't be reopened: why. (Paused stays
    /// paused: it's still lost, and not checked until asked.)
    pub(super) fn report_lost(&self, why: &str) {
        self.0.supervisor.health.send_if_modified(|h| {
            let lost = HealthState::Lost(why.to_string());
            let changed = h.state != lost && !matches!(h.state, HealthState::Closed | HealthState::Paused(_));
            if changed {
                h.state = lost;
            }
            changed
        });
    }

    /// A check found it lost; one asked for also ends a pause (the checks
    /// go on from the first wait).
    fn report_lost_after(&self, why: &str, asked: bool) {
        if asked {
            self.0.supervisor.health.send_if_modified(|h| {
                let paused = matches!(h.state, HealthState::Paused(_));
                if paused {
                    h.state = HealthState::Lost(why.to_string());
                }
                paused
            });
        }
        self.report_lost(why);
    }

    /// Checked [`LOST_CHECKS`] times without an answer: no more until asked.
    fn report_paused(&self) {
        self.0.supervisor.health.send_if_modified(|h| {
            let HealthState::Lost(why) = &h.state else { return false };
            h.state = HealthState::Paused(why.clone());
            h.next_check = None;
            true
        });
    }

    /// Its session was reopened; `lost` is what the old one had that the
    /// new one hasn't.
    pub(super) fn report_reconnected(&self, lost: Vec<String>) {
        self.0.supervisor.health.send_modify(|h| {
            h.state = HealthState::Reconnected;
            h.reconnects += 1;
            h.next_check = None;
            h.lost = lost;
        });
    }

    pub(super) fn report_closed(&self) {
        self.0.supervisor.health.send_modify(|h| {
            h.state = HealthState::Closed;
            h.next_check = None;
        });
    }

    /// Whether the session is free now (nothing running on it, nothing
    /// waiting).
    #[cfg(test)]
    pub(super) fn session_free(&self) -> bool {
        let shared = &self.0.supervisor.shared;
        !shared.busy.load(Ordering::SeqCst) && shared.queued.load(Ordering::SeqCst) == 0
    }

    /// Runs `work` on the session if it's free now, taken at once so
    /// nothing gets in before it; `None` if it isn't (the caller uses a
    /// pool connection instead).
    pub(super) async fn with_free_session<T, F>(&self, work: F) -> Option<Result<T>>
    where
        T: Send + 'static,
        F: for<'s> FnOnce(&'s mut Session) -> SessionFuture<'s, T> + Send + 'static,
    {
        let shared = &self.0.supervisor.shared;
        // Given up on (a dead link): not taken (its work couldn't be
        // stopped, its caller still waiting); the next run renews it.
        if shared.busy.load(Ordering::SeqCst) || self.0.stale.load(Ordering::SeqCst) {
            return None;
        }
        // Claimed: counted as waiting before anything else can be.
        shared.queued.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        let (reply, answer) = oneshot::channel();
        let work: Work = Box::new(move |session| {
            Box::pin(async move {
                let _ = reply.send(work(session).await);
            })
        });
        let unsent = Unsent(Some(&shared.queued));
        let (_waiting, gone) = oneshot::channel();
        if self.0.supervisor.jobs.send(Job { work, stream: false, gone: Some(gone), closes: false }).await.is_err() {
            return Some(Err(closed()));
        }
        unsent.sent();
        Some(answer.await.map_err(|_| anyhow::anyhow!("the session's work stopped")))
    }

    /// Sends `job` to the supervisor, counted until it's done.
    async fn send_job(&self, job: Job) -> Result<()> {
        let shared = &self.0.supervisor.shared;
        shared.queued.fetch_add(1, Ordering::SeqCst);
        let unsent = Unsent(Some(&shared.queued));
        self.0.supervisor.jobs.send(job).await.map_err(|_| closed())?;
        unsent.sent();
        Ok(())
    }

    /// Runs `work` on the session, in its turn, and returns what it gave.
    /// Work given up on before its turn (its caller stopped waiting) isn't
    /// run.
    pub(super) async fn with_session<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'s> FnOnce(&'s mut Session) -> SessionFuture<'s, T> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let shared = Arc::clone(&self.0.supervisor.shared);
        let work: Work = Box::new(move |session| {
            Box::pin(async move {
                if reply.is_closed() {
                    shared.skipped.store(true, Ordering::SeqCst);
                    return;
                }
                let _ = reply.send(work(session).await);
            })
        });
        let (_waiting, gone) = oneshot::channel();
        self.send_job(Job { work, stream: false, gone: Some(gone), closes: false }).await?;
        answer
            .await
            .map_err(|_| anyhow::anyhow!("the session's work stopped (an internal error, or the connection closed)"))
    }

    /// Starts a streamed result's `work` on the session, in its turn,
    /// without waiting for it: it answers on channels of its own.
    pub(super) async fn stream_on_session<F>(&self, work: F) -> Result<()>
    where
        F: for<'s> FnOnce(&'s mut Session) -> SessionFuture<'s, ()> + Send + 'static,
    {
        self.send_job(Job { work: Box::new(work), stream: true, gone: None, closes: false }).await
    }

    /// The connection's close: what runs on the session is stopped, what
    /// waits isn't run, and the session's closed (`Session::Closed`)
    /// next, not in its turn behind them.
    pub(super) async fn close_session(&self) {
        let supervisor = &self.0.supervisor;
        supervisor.shared.closing.store(true, Ordering::SeqCst);
        supervisor.stop_work();
        let (reply, answer) = oneshot::channel();
        let work: Work = Box::new(move |session| {
            Box::pin(async move {
                *session = Session::Closed;
                let _ = reply.send(());
            })
        });
        if self.send_job(Job { work, stream: false, gone: None, closes: true }).await.is_ok() {
            let _ = answer.await;
        }
    }

    /// Whether work runs on the session now.
    pub(super) fn busy(&self) -> bool {
        self.0.supervisor.shared.busy.load(Ordering::SeqCst)
    }
}

fn closed() -> anyhow::Error {
    anyhow::anyhow!("the connection is closed")
}

/// A job counted as waiting but not yet in the mailbox: uncounted again
/// if it never gets there (the mailbox closed, or the sender gave up while
/// it was full); the supervisor uncounts it once it's in.
struct Unsent<'a>(Option<&'a std::sync::atomic::AtomicUsize>);

impl Unsent<'_> {
    fn sent(mut self) {
        self.0 = None;
    }
}

impl Drop for Unsent<'_> {
    fn drop(&mut self) {
        if let Some(queued) = self.0 {
            queued.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database file of the test's own (SQLite: no server needed).
    #[cfg(feature = "sqlite")]
    async fn file_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let params = crate::db::tests::file_params(&dir.path().join("s.db"), false);
        (Database::connect(params).await.unwrap(), dir)
    }

    /// A query that streams many rows (a page at a time).
    #[cfg(feature = "sqlite")]
    const MANY_ROWS: &str =
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000) SELECT i FROM n";

    /// Work on the session runs one piece at a time, in the order sent.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn work_runs_in_the_order_sent() {
        let (db, _dir) = file_db().await;
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let works = (0..20).map(|i| {
            let order = Arc::clone(&order);
            db.with_session(move |_| {
                Box::pin(async move {
                    // Each takes a while: none starts beside another.
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    order.lock().unwrap().push(i);
                })
            })
        });
        for done in futures_util::future::join_all(works).await {
            done.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    /// The supervisor ends with the last handle: its reports end too.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn the_supervisor_ends_with_its_last_handle() {
        let (db, _dir) = file_db().await;
        let mut health = db.health();
        let other = db.clone();
        drop(db);
        assert!(tokio::time::timeout(Duration::from_millis(200), health.changed()).await.is_err(), "a handle's left");
        drop(other);
        let ended = tokio::time::timeout(Duration::from_secs(1), health.changed()).await.expect("ended at once");
        assert!(ended.is_err(), "no reports after the last handle");
    }

    /// Closed: reported, and nothing runs on it after.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_closed_connection_says_so_and_runs_nothing() {
        let (db, _dir) = file_db().await;
        db.close().await;
        assert_eq!(db.health().borrow().state, HealthState::Closed);
        assert!(db.run("SELECT 1", 1).await.is_err());
    }

    /// A streamed result holds the session while its rows are read: what
    /// comes after waits, and runs once the stream is let go of.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_streamed_result_holds_the_session_until_its_let_go_of() {
        let (db, _dir) = file_db().await;
        let (first, rows) = db.run_streaming(MANY_ROWS, 10).await.unwrap();
        assert_eq!(first.rows.len(), 10);
        let rows = rows.expect("more to read");
        assert!(!db.session_free(), "the stream holds it");
        let after = db.clone();
        let mut next = tokio::spawn(async move { after.run("SELECT 7", 1).await });
        assert!(tokio::time::timeout(Duration::from_millis(300), &mut next).await.is_err(), "waits its turn");
        drop(rows);
        let o = tokio::time::timeout(Duration::from_secs(5), next).await.expect("runs once let go").unwrap().unwrap();
        assert_eq!(o.rows[0][0].as_deref(), Some("7"));
    }

    /// Giving the session up stops a stream still reading on it, and the
    /// next run opens a new session.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn giving_the_session_up_stops_its_stream() {
        let (db, _dir) = file_db().await;
        let (_, rows) = db.run_streaming(MANY_ROWS, 10).await.unwrap();
        let _rows = rows.expect("more to read");
        let after = db.clone();
        let next = tokio::spawn(async move { after.run("SELECT 7", 1).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        db.abandon_session();
        let err = tokio::time::timeout(Duration::from_secs(5), next).await.expect("not held").unwrap().unwrap_err();
        assert!(errors::is_reconnected(&err), "on a new session, not sent: {err:#}");
        assert_eq!(db.health().borrow().reconnects, 1);
    }

    /// With the heartbeat on, an idle connection is checked with nothing
    /// asked of it (its latency reported); switched on, at once.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn the_heartbeat_checks_an_idle_connection_by_itself() {
        let (db, _dir) = file_db().await;
        let mut health = db.health();
        db.set_heartbeat(Some(Duration::from_millis(100)));
        let checked = async {
            while health.borrow_and_update().latency.is_none() {
                health.changed().await.unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(2), checked).await.expect("checked by itself");
        // A long heartbeat, and a check wanted now: checked at once.
        let (db, _dir) = file_db().await;
        let mut health = db.health();
        db.set_heartbeat(Some(Duration::from_secs(600)));
        db.check_soon();
        let checked = async {
            while health.borrow_and_update().latency.is_none() {
                health.changed().await.unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(1), checked).await.expect("checked at once");
    }

    /// A streamed read that panics: the same as other work.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_stream_that_panics_doesnt_end_the_supervisor() {
        let (db, _dir) = file_db().await;
        db.stream_on_session(|_| Box::pin(async { panic!("a driver's bug, reading rows") })).await.unwrap();
        crate::db::tests::rerun(&db, "SELECT 7", 1).await;
    }

    /// Giving the session up stops a run hung on it (a dead link; Esc
    /// again, which drops the run first), not only a stream: the next run
    /// opens a new session rather than waiting behind it for good.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn giving_the_session_up_stops_a_hung_run() {
        let (db, _dir) = file_db().await;
        let hung = db.clone();
        let stuck = tokio::spawn(async move { hung.with_session(|_| Box::pin(std::future::pending::<()>())).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        stuck.abort();
        db.abandon_session();
        tokio::time::timeout(Duration::from_secs(5), crate::db::tests::rerun(&db, "SELECT 7", 1))
            .await
            .expect("not behind it");
    }

    /// The session taken only if it's free right now (a MySQL catalog
    /// query, else the pool): not while work runs or waits.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn the_session_is_taken_only_if_free() {
        let (db, _dir) = file_db().await;
        let free = db.with_free_session(|_| Box::pin(async { 7 })).await;
        assert_eq!(free.map(Result::unwrap), Some(7));
        let held = db.clone();
        let holding = tokio::spawn(async move {
            held.with_session(|_| Box::pin(async { tokio::time::sleep(Duration::from_millis(300)).await })).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(db.with_free_session(|_| Box::pin(async {})).await.is_none(), "held: the pool instead");
        holding.await.unwrap().unwrap();
        assert!(db.with_free_session(|_| Box::pin(async {})).await.is_some());
    }

    /// A check on a closed connection says so, not "session busy" (its
    /// own answer comes first, every time).
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_check_on_a_closed_connection_says_so() {
        let (db, _dir) = file_db().await;
        db.close().await;
        for _ in 0..20 {
            let checked = format!("{:?}", db.check_statement("SELECT 1").await);
            assert!(!checked.contains("busy"), "{checked}");
        }
    }

    /// Work that panics doesn't end the supervisor: its caller hears it
    /// stopped, the connection is said lost, and the next run opens a new
    /// session and works.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn work_that_panics_doesnt_end_the_supervisor() {
        let (db, _dir) = file_db().await;
        let stopped = db.with_session(|_| Box::pin(async { panic!("a driver's bug") })).await;
        let err = stopped.expect_err("its caller hears");
        assert!(format!("{err:#}").contains("stopped"), "{err:#}");
        assert!(db.health().borrow().state.is_lost());
        crate::db::tests::rerun(&db, "SELECT 7", 1).await;
        assert!(!db.health().borrow().state.is_lost());
    }

    /// A run's `BEGIN` is noted before the next job (a check) gets the
    /// session, not only once the app hears of it; its `COMMIT` too, and a
    /// failed run's `BEGIN` that ran.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_runs_transaction_is_noted_before_the_next_job() {
        let (db, _dir) = file_db().await;
        db.run("BEGIN", 1).await.unwrap();
        assert!(db.in_transaction());
        db.run("SELECT 1; COMMIT", 1).await.unwrap();
        assert!(!db.in_transaction());
        db.run("BEGIN; SELECT * FROM missing", 1).await.unwrap_err();
        assert!(db.in_transaction(), "the BEGIN before the failing statement ran");
        db.run("ROLLBACK", 1).await.unwrap();
        assert!(!db.in_transaction());
    }

    /// A close isn't held up by what runs on the session (a hung run whose
    /// caller still waits), nor by what waits: both hear the work stopped.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_close_doesnt_wait_behind_work() {
        let (db, _dir) = file_db().await;
        let hung = db.clone();
        let running = tokio::spawn(async move { hung.with_session(|_| Box::pin(std::future::pending::<()>())).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let next = db.clone();
        let waiting = tokio::spawn(async move { next.run("SELECT 1", 1).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::time::timeout(Duration::from_secs(5), db.close()).await.expect("not behind the hung run");
        assert!(running.await.unwrap().is_err(), "the hung run's caller hears");
        assert!(waiting.await.unwrap().is_err(), "the waiting run isn't run");
        assert!(matches!(db.health().borrow().state, HealthState::Closed));
    }

    /// A time limit counts from the statement, not from reopening a
    /// session given up on: that run says it wasn't sent (not a time-out),
    /// and the next runs within its limit on the new session.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_time_limit_doesnt_count_the_reopen() {
        let (db, _dir) = file_db().await;
        db.abandon_session();
        let limit = Some(Duration::from_secs(5));
        match db.run_within("SELECT 7", 1, limit).await {
            Err(crate::db::RunError::Failed(e)) => assert!(crate::db::is_reconnected(&e), "{e:#}"),
            other => panic!("not sent, said so: {other:?}"),
        }
        let o = db.run_within("SELECT 7", 1, limit).await.expect("on the new session");
        assert_eq!(crate::db::tests::cells(&o), vec![vec![Some("7")]]);
    }

    /// A check asked for while work holds the session isn't dropped: it's
    /// made once that work's done.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_check_asked_for_behind_work_is_made_after_it() {
        let (db, _dir) = file_db().await;
        let mut health = db.health();
        let held = db.clone();
        let holding = tokio::spawn(async move {
            held.with_session(|_| Box::pin(async { tokio::time::sleep(Duration::from_millis(300)).await })).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(health.borrow_and_update().latency.is_none());
        db.check_soon();
        holding.await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while health.borrow_and_update().latency.is_none() {
                health.changed().await.unwrap();
            }
        })
        .await
        .expect("checked after the work");
    }

    /// A session a check reopened (nothing running) isn't one a run lands
    /// on unsaid: the next isn't sent, saying so; once the app's said the
    /// reconnect, runs go on.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_checks_reconnect_isnt_landed_on_unsaid() {
        let (db, _dir) = file_db().await;
        db.abandon_session();
        db.check_now().await.unwrap();
        assert_eq!(db.health().borrow().reconnects, 1, "reopened by the check");
        let err = db.run("SELECT 1", 1).await.expect_err("not sent on it unsaid");
        assert!(crate::db::is_reconnected(&err), "{err:#}");
        db.run("SELECT 1", 1).await.expect("said by that run: the next goes on");
        // Said by the app instead: no run's refused.
        db.abandon_session();
        db.check_now().await.unwrap();
        db.reconnect_told(db.health().borrow().reconnects);
        db.run("SELECT 1", 1).await.expect("the app said it");
    }

    /// A stream whose caller's gone before its turn sends nothing.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_queued_stream_whose_caller_is_gone_sends_nothing() {
        let (db, _dir) = file_db().await;
        let slow = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000000) \
                    SELECT count(*) FROM n";
        crate::db::tests::a_queued_stream_whose_caller_is_gone_sends_nothing(&db, slow).await;
    }

    /// A database that lives in its session (SQLite's `:memory:`) isn't
    /// replaced by an empty one: not after work on it panics, nor when
    /// it's given up on (Esc again).
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn an_in_memory_database_keeps_its_session() {
        let params = ConnectParams {
            engine: Engine::Sqlite,
            ..crate::db::tests::file_params(std::path::Path::new(":memory:"), false)
        };
        let db = Database::connect(params).await.unwrap();
        db.run("CREATE TABLE kept (x int); INSERT INTO kept VALUES (7)", 1).await.unwrap();
        let stopped = db.with_session(|_| Box::pin(async { panic!("a driver's bug") })).await;
        assert!(stopped.is_err(), "its caller hears");
        let o = db.run("SELECT x FROM kept", 1).await.expect("the same session, not an empty one");
        assert_eq!(crate::db::tests::cells(&o), vec![vec![Some("7")]]);
        db.abandon_session();
        let o = db.run("SELECT x FROM kept", 1).await.expect("kept when given up on too");
        assert_eq!(crate::db::tests::cells(&o), vec![vec![Some("7")]]);
        assert!(!db.health().borrow().state.is_lost());
    }

    /// A check asked for waits only so long for its turn (`None`: the
    /// session's held, and the check isn't run); once it has it, its answer
    /// is waited for, however long it takes.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_check_waits_for_its_turn_briefly_and_then_for_its_answer() {
        let (db, _dir) = file_db().await;
        let busy = db.clone();
        let held = tokio::spawn(async move {
            busy.with_session(|_| Box::pin(async { tokio::time::sleep(Duration::from_millis(400)).await })).await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(db.check_within_turn(Duration::from_millis(100)).await.unwrap().is_none(), "held");
        held.await.unwrap().unwrap();
        assert!(db.check_within_turn(Duration::from_millis(100)).await.unwrap().is_some(), "free");
    }

    /// A give-up that comes after the stuck work was done (Esc again, just
    /// as it finished) doesn't stop the work after it, whose caller still
    /// waits; work whose caller's gone is stopped.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_late_give_up_doesnt_stop_the_next_work() {
        let (db, _dir) = file_db().await;
        let next = db.clone();
        let waited = tokio::spawn(async move {
            next.with_session(|_| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    7
                })
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        db.abandon_session();
        assert_eq!(waited.await.unwrap().unwrap(), 7, "its caller still waited: not stopped");
        // Its caller gone (the run's task dropped), then given up on: stopped.
        let stuck = db.clone();
        let run = tokio::spawn(async move {
            stuck.with_session(|_| Box::pin(async { std::future::pending::<()>().await })).await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        run.abort();
        db.abandon_session();
        let after = tokio::time::timeout(Duration::from_secs(2), db.with_session(|_| Box::pin(async { 1 }))).await;
        assert_eq!(after.expect("the session's free again").unwrap(), 1);
    }

    /// A session given up on isn't taken as free (its work couldn't be
    /// stopped): the caller uses a pool connection.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_session_given_up_on_isnt_taken_as_free() {
        let (db, _dir) = file_db().await;
        db.abandon_session();
        assert!(db.with_free_session(|_| Box::pin(async { 1 })).await.is_none());
    }

    /// An open result waiting for its next page to be asked for isn't
    /// reading: a run waiting behind it isn't taken for one stuck on the
    /// link.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn an_open_result_waiting_for_its_next_page_isnt_reading() {
        let (db, _dir) = file_db().await;
        let (_, rows) = db.run_streaming(MANY_ROWS, 10).await.unwrap();
        let rows = rows.expect("more to read");
        assert!(db.0.supervisor.streaming() && !db.0.supervisor.reading());
        rows.next_page(10).await.unwrap();
        assert!(!db.0.supervisor.reading(), "done reading that page");
    }

    /// A new connection is idle from when it opened, not for ever: its
    /// first run isn't checked first.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_new_connection_is_idle_since_it_opened() {
        let (db, _dir) = file_db().await;
        assert!(db.idle_for() < Duration::from_secs(5), "{:?}", db.idle_for());
    }

    /// Work waiting in the mailbox means the session isn't free (a MySQL
    /// catalog query then goes to the pool, not behind it); work given up
    /// on before its turn isn't run, and frees it again.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn waiting_work_means_the_session_isnt_free() {
        let (db, _dir) = file_db().await;
        assert!(db.session_free());
        let ran = Arc::new(AtomicBool::new(false));
        let marked = Arc::clone(&ran);
        // Sent, then given up on at once: queued, not yet started.
        let sent = db
            .with_session(move |_| {
                Box::pin(async move {
                    marked.store(true, Ordering::SeqCst);
                })
            })
            .now_or_never();
        assert!(sent.is_none());
        assert!(!db.session_free(), "waiting work counts");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(db.session_free() && !ran.load(Ordering::SeqCst), "given up on: not run, and done");
    }

    /// Work given up on while the mailbox is full (it never got in) isn't
    /// counted as waiting for good: the session's free again after.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn work_given_up_on_a_full_mailbox_is_uncounted() {
        let (db, _dir) = file_db().await;
        let mut waiting = Vec::new();
        // One running, the mailbox full behind it.
        for _ in 0..=MAILBOX {
            let busy = db.clone();
            waiting.push(tokio::spawn(async move {
                busy.with_session(|_| Box::pin(async { tokio::time::sleep(Duration::from_millis(5)).await })).await
            }));
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        for _ in 0..5 {
            let _ = tokio::time::timeout(Duration::from_micros(10), db.with_session(|_| Box::pin(async {}))).await;
            let _ = tokio::time::timeout(Duration::from_micros(10), db.with_free_session(|_| Box::pin(async {}))).await;
        }
        for w in waiting {
            let _ = w.await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(db.session_free(), "nothing left counted");
        assert!(db.with_free_session(|_| Box::pin(async { 1 })).await.is_some(), "and taken as free");
    }

    /// A statement check waits only briefly for its turn ("session busy"),
    /// and once it has it, for its answer.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_statement_check_waits_for_its_turn_briefly() {
        let (db, _dir) = file_db().await;
        let busy = db.clone();
        let held = tokio::spawn(async move {
            busy.with_session(|_| Box::pin(async { tokio::time::sleep(Duration::from_millis(900)).await })).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let err = db.check_statement("SELECT 1").await.expect_err("the session's held");
        assert!(format!("{err:#}").contains("busy"), "{err:#}");
        held.await.unwrap().unwrap();
        assert!(matches!(db.check_statement("SELECT 1").await.unwrap(), CheckOutcome::Ok));
    }

    #[test]
    fn a_lost_connection_waits_longer_each_time_up_to_a_minute_jittered() {
        // (In tests a wait is 1/50 of what it is: 5 s is 100 ms.)
        let within = |failed: u32, secs: f64| {
            let secs = secs * FIRST_LOST_WAIT.as_secs_f64() / 5.0;
            let wait = lost_wait(failed).as_secs_f64();
            assert!((secs * 0.89..=secs * 1.11).contains(&wait), "{failed} failed: {wait} s, not about {secs} s");
        };
        // Doubling from the first: a minute is reached before the pause.
        within(0, 5.0);
        within(1, 10.0);
        within(2, 20.0);
        within(3, 40.0);
        within(4, 60.0);
        within(9, 60.0);
    }
}
