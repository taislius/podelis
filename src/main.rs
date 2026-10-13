mod agents;
mod analyze;
mod app;
mod cli;
mod clipboard;
mod complete;
mod containers;
mod crash;
mod db;
mod diff;
mod doctor;
mod editor;
mod export;
#[cfg(test)]
mod fuzz_tests;
mod history;
mod import;
mod keys;
mod library;
mod lint;
mod logging;
mod mcp;
mod params;
mod parse;
mod scope;
mod settings;
mod sql;
mod store;
mod theme;
mod ui;

use std::io::{Write as _, stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossterm::clipboard::{ClipboardSelection, ClipboardType, CopyToClipboard};
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use app::App;
use store::ConnectionStore;

#[tokio::main]
async fn main() -> Result<()> {
    let args = match cli::parse(std::env::args().skip(1)) {
        cli::ParseOutcome::Run(args) => args,
        cli::ParseOutcome::Exit(code, text) => {
            let _ = if code == 0 { writeln!(stdout(), "{text}") } else { writeln!(std::io::stderr(), "{text}") };
            std::process::exit(code);
        }
    };
    let log_path = logging::init();
    let (store, store_warning) = ConnectionStore::load();
    if args.mcp {
        std::process::exit(mcp::run(&store, args.project.as_deref()).await);
    }
    if args.doctor {
        let code = doctor::run(&store, store_warning.as_deref(), args.project.as_deref(), args.test_connections).await;
        std::process::exit(code);
    }
    if args.test {
        std::process::exit(cli::test(&args, &store).await);
    }
    if args.import.is_some() {
        std::process::exit(cli::import(&args, &store).await);
    }
    if args.execute.is_some() {
        std::process::exit(cli::execute(&args, &store).await);
    }
    let config_dir = store::config_dir();
    if let Some(id) = &store.migrated_project {
        history::migrate_legacy(&config_dir.join("history.jsonl"), &config_dir.join("history"), id);
    }
    let (settings, settings_warning) = settings::Settings::load(&store::config_dir());
    theme::set_theme(settings.theme);
    let (keymap, key_warnings) = keys::KeyMap::new(&settings.keys);
    let warnings: Vec<String> = store_warning.into_iter().chain(settings_warning).chain(key_warnings).collect();
    let warning = (!warnings.is_empty()).then(|| warnings.join("\n"));
    if let Some(path) = &log_path {
        tracing::info!(path = %path.display(), "debug log enabled");
    }
    let mut app = App::new(store, warning, Some(config_dir));
    app.settings = settings;
    app.keymap = keymap;
    app.start(args.project.as_deref(), args.connection.as_deref());

    // Installs a panic hook that restores the terminal.
    let mut terminal = ratatui::init();
    // Lets terminals that support it (kitty, WezTerm, Ghostty, foot, iTerm2
    // with CSI u) report Ctrl+Enter distinctly from Enter.
    let enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    let mouse = app.settings.mouse;
    enable_input_modes(enhanced, mouse)?;

    let ratatui_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let log = crash::record(info);
        // A background task's panic (a query, a catalog load) doesn't end
        // the app: leave the terminal alone and let the UI report it.
        if std::thread::current().name() != Some("main") {
            crash::note_background(info);
            return;
        }
        restore_input_modes(enhanced, mouse);
        ratatui_hook(info);
        if let Some(path) = log {
            let _ = writeln!(
                std::io::stderr(),
                "\nPodėlis crashed. Details were saved to {} (please attach it to a bug report).",
                path.display()
            );
        }
    }));

    let result = run(&mut terminal, &mut app, enhanced, mouse).await;
    app.save_draft();

    restore_input_modes(enhanced, mouse);
    ratatui::restore();
    result
}

fn enable_input_modes(enhanced: bool, mouse: bool) -> std::io::Result<()> {
    if enhanced {
        execute!(stdout(), PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
    }
    // Focus reports tell whether a long query's end needs announcing.
    execute!(stdout(), EnableBracketedPaste, EnableFocusChange)?;
    if mouse {
        execute!(stdout(), EnableMouseCapture)?;
    }
    Ok(())
}

fn restore_input_modes(enhanced: bool, mouse: bool) {
    let _ = execute!(stdout(), DisableBracketedPaste, DisableFocusChange, SetCursorStyle::DefaultUserShape);
    if mouse {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Copies with the system's clipboard tool where there is one (any size, any
/// terminal), else via the OSC 52 escape sequence, which works over SSH in
/// most modern terminals (tmux needs `set -g set-clipboard on`).
fn copy_to_clipboard(text: String) {
    if clipboard::write(&text).is_ok() {
        return;
    }
    let _ = execute!(
        stdout(),
        CopyToClipboard { content: text, destination: ClipboardSelection(vec![ClipboardType::Clipboard]) }
    );
}

/// Rings the bell and asks the terminal for a desktop notification (OSC 9:
/// `iTerm2`, `WezTerm`, Ghostty, kitty…; others ignore it).
fn notify(text: &str) {
    let clean: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    let mut out = stdout();
    let _ = write!(out, "\x07\x1b]9;Podėlis: {clean}\x07");
    let _ = out.flush();
}

fn handle_input(app: &mut App, event: Event) {
    match event {
        // Some terminals (enhanced keyboards) also report releases.
        Event::Key(key) if key.kind != KeyEventKind::Release => app.handle_key(key),
        Event::Paste(text) => app.handle_paste(&text),
        Event::Mouse(mouse) => app.handle_mouse(mouse),
        Event::FocusGained => app.set_terminal_focus(true),
        Event::FocusLost => app.set_terminal_focus(false),
        _ => {}
    }
}

async fn run(terminal: &mut DefaultTerminal, app: &mut App, enhanced: bool, mouse: bool) -> Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    // Paused while another program (the viewer) has the terminal, so its
    // keystrokes aren't read here.
    let paused = Arc::new(AtomicBool::new(false));
    let reader_paused = Arc::clone(&paused);
    std::thread::spawn(move || {
        loop {
            if reader_paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            match event::poll(Duration::from_millis(200)) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if input_tx.send(ev).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });

    let mut cursor_shape: Option<SetCursorStyle> = None;
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let lint_due = app.lint_due().map(tokio::time::Instant::from_std);
        let menu_due = app.palette_menu_due().map(tokio::time::Instant::from_std);
        let splash_due = app.splash_due().map(tokio::time::Instant::from_std);
        tokio::select! {
            () = async { tokio::time::sleep_until(menu_due.unwrap()).await }, if menu_due.is_some() => {
                app.show_palette_menu();
            }
            () = async { tokio::time::sleep_until(splash_due.unwrap()).await }, if splash_due.is_some() => {
                app.show_splash();
            }
            () = async { tokio::time::sleep_until(lint_due.unwrap()).await }, if lint_due.is_some() => {
                app.tick();
            }
            event = input_rx.recv() => {
                let Some(event) = event else { break };
                handle_input(app, event);
                // Input that queued up meanwhile (a trackpad's scroll burst,
                // key repeat) is handled before drawing again, unless the
                // loop has something to do first.
                for _ in 0..64 {
                    if app.has_pending_output() {
                        break;
                    }
                    match input_rx.try_recv() {
                        Ok(event) => handle_input(app, event),
                        Err(_) => break,
                    }
                }
            }
            // Query timeouts, and panicked tasks that never send a result.
            () = tokio::time::sleep(Duration::from_secs(1)), if app.wants_ticks() => {
                app.tick_running();
            }
            db_event = app.poll_db_event() => {
                if let Some(db_event) = db_event {
                    app.handle_db_event(db_event);
                }
            }
        }

        // Block cursor in vim Normal/Visual mode, bar in Insert mode.
        let shape = match app.vim_mode() {
            None => SetCursorStyle::DefaultUserShape,
            Some(app::VimMode::Insert) => SetCursorStyle::SteadyBar,
            Some(_) => SetCursorStyle::SteadyBlock,
        };
        if Some(shape) != cursor_shape {
            let _ = execute!(stdout(), shape);
            cursor_shape = Some(shape);
        }
        if let Some(text) = app.take_clipboard() {
            copy_to_clipboard(text);
        }
        if let Some(text) = app.take_notification() {
            notify(&text);
        }
        if let Some(external) = app.take_external() {
            // Hand the terminal over, run it, and take the terminal back.
            paused.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(250));
            restore_input_modes(enhanced, mouse);
            ratatui::restore();
            app.run_external(&external);
            crossterm::terminal::enable_raw_mode()?;
            execute!(stdout(), crossterm::terminal::EnterAlternateScreen)?;
            enable_input_modes(enhanced, mouse)?;
            terminal.clear()?;
            cursor_shape = None;
            paused.store(false, Ordering::SeqCst);
        }
        if app.should_quit {
            break;
        }
    }

    Ok(())
}
