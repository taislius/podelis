//! The rules every frame keeps, at every size and in every language, for
//! the scenarios that draw the app: the UI's own (`ui::tests::fit`) and the
//! database's behaviours (`app::tests::behaviours`). Rules rather than
//! pictures to compare: what must be there to act on (the keys that close,
//! commit, undo; what was said and why) is checked whole, on one line, at
//! each size. A frame that panics breaks them all.
//!
//! Each scenario is drawn on its own engine at every size, and on every
//! other engine at a few (their keys, footers and offers differ).
//!
//! What's known to break a rule is listed by its scenario and the frames
//! it breaks in (`[en] 40×12`, `[lt] 60×12 on SQLite`), with why: the
//! check fails when anything else breaks one (another frame, or another
//! scenario), and when a listed frame doesn't any more (so it's taken off
//! the list).

use std::panic::{AssertUnwindSafe, catch_unwind};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::App;

/// The terminal sizes each scenario is drawn at: narrow to wide, the
/// navigator's limit on both sides (it hides below 90), short to tall.
pub(crate) const WIDTHS: [u16; 6] = [40, 60, 80, 89, 90, 120];
pub(crate) const HEIGHTS: [u16; 3] = [12, 24, 40];
pub(crate) const LANGUAGES: [&str; 2] = ["en", "lt"];
/// The sizes a scenario is drawn at on the engines other than its own.
pub(crate) const OTHER_ENGINE_SIZES: [(u16, u16); 4] = [(40, 24), (60, 12), (90, 24), (120, 40)];

/// What makes a scenario's state, for a terminal (width, height).
pub(crate) type Setup = Box<dyn Fn(&mut App, u16, u16)>;
/// What must be whole on one line at (width, height), as drawn (runs of
/// spaces read as one).
pub(crate) type MustShow = Box<dyn Fn(&App, u16, u16) -> Vec<String>>;

/// A state of the app to draw, and what must be on screen in it.
pub(crate) struct Scenario {
    pub name: String,
    /// The app it starts from.
    pub base: fn() -> (App, tempfile::TempDir),
    pub setup: Setup,
    pub must_show: MustShow,
    /// The engine it's drawn on first (its base's when `None`); the
    /// other engines' runs put the app on theirs instead.
    pub engine: Option<crate::db::Engine>,
}

impl Scenario {
    pub(crate) fn new(
        name: impl Into<String>,
        base: fn() -> (App, tempfile::TempDir),
        setup: impl Fn(&mut App, u16, u16) + 'static,
        must_show: impl Fn(&App, u16, u16) -> Vec<String> + 'static,
    ) -> Self {
        Scenario { name: name.into(), base, setup: Box::new(setup), must_show: Box::new(must_show), engine: None }
    }

    /// Drawn first on `engine` (a server's words, its engine's).
    pub(crate) fn on(self, engine: crate::db::Engine) -> Self {
        Scenario { engine: Some(engine), ..self }
    }
}

thread_local! {
    /// The runtime of the check running on this thread (see [`with_runtime`]).
    static RUNTIME: std::cell::RefCell<Option<std::sync::Arc<tokio::runtime::Runtime>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with a runtime of its own, dropped after it (with whatever it
/// spawned): what an event starts in the background (a catalog's load, a
/// connection's supervisor) has one to be spawned on. One thread, driven
/// only by [`wait`]: nothing runs behind a frame being drawn, so each is
/// the same every time; a test's own, so what one test spawns is never
/// run by another's `wait`, in its language, on its state.
pub(crate) fn with_runtime<R>(f: impl FnOnce() -> R) -> R {
    /// Puts back the runtime there was, panicking or not.
    struct Restore(Option<std::sync::Arc<tokio::runtime::Runtime>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            RUNTIME.with(|r| *r.borrow_mut() = before);
        }
    }
    let runtime = std::sync::Arc::new(tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap());
    let _restore = Restore(RUNTIME.with(|r| r.replace(Some(std::sync::Arc::clone(&runtime)))));
    let _entered = runtime.enter();
    f()
}

/// `future`'s outcome, awaited on the scenarios' runtime from a thread of
/// its own (a scenario's setup is already inside the runtime), what's
/// spawned meanwhile run with it.
#[cfg_attr(not(feature = "sqlite"), expect(dead_code, reason = "the behaviours' scenarios are SQLite's"))]
pub(crate) fn wait<T: Send>(future: impl Future<Output = T> + Send) -> T {
    // On the check's own runtime, in its language.
    let runtime = RUNTIME.with(|r| r.borrow().clone()).expect("inside `with_runtime`");
    let language = crate::i18n::code();
    std::thread::scope(|s| {
        s.spawn(|| {
            let _language = crate::i18n::on_this_thread(language);
            runtime.block_on(future)
        })
        .join()
        .unwrap()
    })
}

pub(crate) fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal.backend().to_string()
}

/// The first `n` characters of `text`: what must show of a message that
/// may not fit whole (its start says what it is).
pub(crate) fn start(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Runs of whitespace as one space.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What `scenario` breaks in `language`: on its own engine at every
/// size, then on each other engine at a few (the app said to be on it);
/// one line per frame that breaks a rule.
fn broken(scenario: &Scenario, language: &str) -> Vec<String> {
    let _language = crate::i18n::on_this_thread(language);
    with_runtime(|| broken_frames(scenario))
}

/// [`broken`]'s frames, in the language and runtime it set.
fn broken_frames(scenario: &Scenario) -> Vec<String> {
    let own = WIDTHS.iter().flat_map(|&w| HEIGHTS.iter().map(move |&h| (None, w, h)));
    let others = crate::db::Engine::ALL
        .into_iter()
        .flat_map(|engine| OTHER_ENGINE_SIZES.iter().map(move |&(w, h)| (Some(engine), w, h)));
    let mut broken = Vec::new();
    for (engine, width, height) in own.chain(others) {
        let (mut app, _dir) = (scenario.base)();
        if let Some(own) = scenario.engine {
            app.connection.form.engine = own;
        }
        if let Some(engine) = engine {
            // Another engine than the base's: skipped where it's its own.
            if app.engine() == engine {
                continue;
            }
            app.connection.form.engine = engine;
        }
        (scenario.setup)(&mut app, width, height);
        let on = engine.map(|e| format!(" on {}", e.label())).unwrap_or_default();
        let drawn = catch_unwind(AssertUnwindSafe(|| render(&mut app, width, height)));
        let Ok(screen) = drawn else {
            broken.push(format!("{width}×{height}{on}: drawing panicked"));
            continue;
        };
        let lines: Vec<String> = screen.lines().map(squeezed).collect();
        let missing: Vec<String> = (scenario.must_show)(&app, width, height)
            .into_iter()
            .map(|s| squeezed(&s))
            .filter(|s| !lines.iter().any(|line| line.contains(s.as_str())))
            .collect();
        if !missing.is_empty() {
            broken.push(format!("{width}×{height}{on}: not whole on screen: {missing:?}"));
        }
    }
    broken
}

/// A scenario's frames known to break a rule, and why.
pub(crate) struct Known {
    pub scenario: &'static str,
    pub why: &'static str,
    /// Each as a break says it: `[en] 40×12`, `[lt] 60×12 on SQLite`.
    pub frames: &'static [&'static str],
}

/// Draws every scenario at every size in every language and fails with
/// the rules broken, those `known` aside (by their frames), and any known
/// frame that isn't broken any more.
pub(crate) fn check(scenarios: &[Scenario], known: &[Known]) {
    let mut report = Vec::new();
    let mut fixed = Vec::new();
    for scenario in scenarios {
        let breaks: Vec<String> = LANGUAGES
            .iter()
            .flat_map(|language| broken(scenario, language).into_iter().map(move |b| format!("[{language}] {b}")))
            .collect();
        let frames: Vec<&str> =
            known.iter().filter(|k| k.scenario == scenario.name).flat_map(|k| k.frames.iter().copied()).collect();
        // A break's frame: what it says before its first `:`.
        let frame_of = |b: &String| b.split(':').next().unwrap_or_default().to_string();
        let unexplained: Vec<&String> = breaks.iter().filter(|b| !frames.contains(&frame_of(b).as_str())).collect();
        if !unexplained.is_empty() {
            let lines: Vec<&str> = unexplained.iter().map(|b| b.as_str()).collect();
            report.push(format!("{}:\n  {}", scenario.name, lines.join("\n  ")));
        }
        for entry in known.iter().filter(|k| k.scenario == scenario.name) {
            for frame in entry.frames.iter().filter(|f| !breaks.iter().any(|b| frame_of(b) == **f)) {
                fixed.push(format!("{} {frame} (was: {})", scenario.name, entry.why));
            }
        }
    }
    assert!(fixed.is_empty(), "fixed now: take them off the known list: {fixed:?}");
    assert!(report.is_empty(), "rules broken:\n{}", report.join("\n"));
}
