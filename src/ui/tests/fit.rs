//! The UI's own scenarios (popups, help, the results footer), drawn at
//! every size in every language against the rules (`ui::fit_rules`).

use super::*;
use crate::config::keys::Action;
use crate::ui::fit_rules::{Known, Scenario, check};

/// Scenarios' frames known to break a rule, and why; taken off as they're
/// fixed.
const KNOWN: &[Known] = &[];

#[expect(clippy::too_many_lines, reason = "one scenario a state, each short")]
fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario::new(
            "transaction review",
            query_app,
            |app, _, _| app.popups.review = Some(crate::app::Review { rows_affected: 3 }),
            |_, _, _| {
                vec![
                    format!("c {}", t!("review.commit")),
                    format!("r {}", t!("review.rollback")),
                    format!("Esc {}", t!("review.keep_open")),
                ]
            },
        ),
        Scenario::new(
            "sandbox with many steps",
            query_app,
            |app, _, _| {
                let step = |i: usize| crate::app::SandboxStep {
                    sql: format!("update orders set status = 'shipped' where id = {i}"),
                    tables: vec!["orders".into()],
                    rows: 1,
                    ddl: false,
                };
                app.connection.sandbox = Some(crate::app::Sandbox::for_tests((1..=30).map(step).collect()));
                app.popups.sandbox_view = Some(crate::app::windows::sandbox::SandboxView { scroll: 0, locks: None });
            },
            |_, _, _| {
                use crate::app::key_table::first_key;
                use crate::app::windows::sandbox::{SANDBOX_KEYS, SandboxKey as S};
                [
                    (S::Undo, t!("sandbox.undo")),
                    (S::Discard, t!("sandbox.discard")),
                    (S::RunForReal, t!("sandbox.run_for_real")),
                ]
                .into_iter()
                .map(|(key, what)| format!("{} {what}", first_key(SANDBOX_KEYS, key)))
                // And how to close it, whole (that it stays open may go
                // below).
                .chain(
                    t!("sandbox.esc", key = first_key(SANDBOX_KEYS, S::Close)).split(" · ").next().map(str::to_string),
                )
                .collect()
            },
        ),
        Scenario::new(
            "help with a long key",
            query_app,
            |app, width, _| {
                use crate::config::keys::{KeyMap, KeySpec};
                let long = KeySpec::Many(vec!["F5".into(), "Ctrl+Enter".into(), "Ctrl+Alt+Shift+F12".into()]);
                app.keymap = KeyMap::new(&std::collections::HashMap::from([("run".to_string(), long)])).0;
                app.help.visible = true;
                app.help.scroll = run_help_line(app, width);
            },
            // The run key's description begins beside it, however long the
            // key: a column of keys never leaves the words no room.
            |app, _, _| {
                let (_, what) = run_help_entry(app);
                vec![what.chars().take(10).collect()]
            },
        ),
        Scenario::new(
            "results footer",
            query_app,
            |app, _, _| {
                app.focus = Focus::Results;
                app.tab.grid.table_state.select(Some(0));
                app.tab.grid.selected_col = 0;
            },
            // The cell picked is said where there's room for it beside the
            // counts (they give way first).
            |_, width, _| {
                if width >= 60 { vec![format!("{} 01:id", t!("cell.cell"))] } else { Vec::new() }
            },
        ),
        Scenario::new(
            "a connection error over several lines",
            query_app,
            |app, _, _| {
                app.screen = Screen::Connect;
                let error = "couldn't connect: password authentication failed for user \"postgres\" · hint: \
                             check the user and password, or the server's pg_hba.conf for this address";
                app.connection.form.error = Some(error.to_string());
            },
            // Its first words and its last: none of it below the fields or
            // behind the keys.
            |app, _, _| {
                let error = app.connection.form.error.clone().unwrap_or_default();
                let last = error.split_whitespace().last().unwrap_or_default().to_string();
                vec![crate::ui::fit_rules::start(&error, 14), last]
            },
        ),
        Scenario::new(
            "the form's last field, nothing said",
            query_app,
            |app, _, _| {
                app.screen = Screen::Connect;
                app.connection.form.focus = ConnectField::Agents;
            },
            // In sight however short the screen: the fields scroll to it.
            |_, _, _| vec![t!("form.agents").to_string()],
        ),
        Scenario::new(
            "a small editor share",
            query_app,
            |app, _, _| {
                app.panes.editor_percent = 20;
                app.focus = Focus::Editor;
            },
            // The cursor's line of the SQL, at least, however small its
            // share (the editor scrolls to it).
            |_, _, _| vec!["where id < 4;".into()],
        ),
        Scenario::new(
            "editor run button",
            query_app,
            |app, _, _| app.focus = Focus::Editor,
            |app, _, _| vec![format!("▶ {} {}", app.keymap.short_label(Action::Run), t!("editor.run"))],
        ),
    ]
}

/// The help's entry for running (its keys, its words).
fn run_help_entry(app: &App) -> (String, String) {
    let keys = app.keymap.label(Action::Run);
    crate::ui::help::help_sections(app)
        .into_iter()
        .flat_map(|(_, entries)| entries)
        .find(|(k, _)| *k == keys)
        .expect("running is in the help")
}

/// The help's line that entry's key is on, as it's laid out on a screen
/// `width` wide: scrolled to the top, it's in sight at any height.
fn run_help_line(app: &App, width: u16) -> u16 {
    let (keys, _) = run_help_entry(app);
    let sections = crate::ui::help::help_sections(app);
    let lines = crate::ui::help::help_lines(app, &sections, crate::ui::help::help_width(app, &sections, width));
    let at = lines.iter().position(|line| line.spans.iter().any(|s| s.content.trim() == keys));
    u16::try_from(at.expect("the run key is in the help")).unwrap_or(0)
}

#[test]
fn every_scenario_keeps_the_rules_at_every_size() {
    check(&scenarios(), KNOWN);
}
