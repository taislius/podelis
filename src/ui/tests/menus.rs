//! Every menu's items and order: a change to a menu shows here.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The open menu: its title, then a line per item with its key.
fn items(app: &App) -> String {
    let menu = app.popups.menu.as_ref().expect("a menu");
    let lines: Vec<String> = menu
        .rows(usize::MAX)
        .into_iter()
        .map(|row| match row {
            None => "---".to_string(),
            Some(i) => {
                let item = &menu.items[i];
                format!("{}  [{}]{}", item.label, item.keys, if item.dimmed { "  (dimmed)" } else { "" })
            }
        })
        .collect();
    format!("{}\n{}", menu.title, lines.join("\n"))
}

#[test]
fn the_results_menu_over_rows() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    assert_snapshot!(items(&app));
}

#[test]
fn the_export_menu_opens_with_a_key() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('E')));
    assert_snapshot!(render(&mut app, 90, 24));
}

/// What the export form is set to.
fn export_choice(app: &App) -> crate::app::export_form::ExportChoice {
    app.popups.export_form.as_ref().expect("the form").choice
}

#[test]
fn the_export_form_offers_only_what_can_be_done() {
    use crate::app::Format;
    use crate::app::export_form::ExportTo;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('E')));
    let start = export_choice(&app);
    assert_eq!((start.to, start.format, start.every), (ExportTo::File, Format::Csv, false));

    // To a file there's a name; every format a file takes goes round.
    let screen = render(&mut app, 100, 30);
    assert!(screen.contains("podelis-export-….csv") && screen.contains("every row"), "{screen}");
    app.handle_key(key(KeyCode::Down));
    let mut seen = vec![];
    for _ in 0..5 {
        seen.push(export_choice(&app).format);
        app.handle_key(key(KeyCode::Right));
    }
    assert_eq!(seen, [Format::Csv, Format::Json, Format::Ascii, Format::Sql, Format::Parquet]);
    assert_eq!(export_choice(&app).format, Format::Csv, "and round");
    app.handle_key(key(KeyCode::Left));
    assert_eq!(export_choice(&app).format, Format::Parquet, "backwards too");

    // Parquet can't be copied: to the clipboard, the nearest that can.
    app.handle_key(key(KeyCode::Up));
    app.handle_key(key(KeyCode::Right));
    assert_eq!(export_choice(&app).to, ExportTo::Clipboard);
    assert_eq!(export_choice(&app).format, Format::Sql);
    let screen = render(&mut app, 100, 30);
    assert!(!screen.contains("podelis-export") && !screen.contains("every row"), "{screen}");
    // Rows aren't a choice there: ↓↓ goes round past them.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.popups.export_form.as_ref().unwrap().line, crate::app::export_form::ExportLine::To);

    // An ASCII table can't be every row: it moves to one that can.
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Down));
    while export_choice(&app).format != Format::Ascii {
        app.handle_key(key(KeyCode::Right));
    }
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Right));
    assert!(export_choice(&app).every);
    assert_ne!(export_choice(&app).format, Format::Ascii);
    assert!(export_choice(&app).format.supports(crate::app::Dest::ExportAll));

    // Esc closes and keeps nothing.
    app.handle_key(key(KeyCode::Esc));
    assert!(app.popups.export_form.is_none());
    app.handle_key(key(KeyCode::Char('E')));
    assert_eq!(export_choice(&app), start);
}

#[test]
fn the_export_form_runs_what_it_says_and_starts_there_next_time() {
    let (mut app, dir) = query_app();
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.focus = Focus::Results;
    let files = |dir: &std::path::Path| -> Vec<String> {
        let names = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned());
        names.filter(|n| n.starts_with("podelis-export")).collect()
    };
    // JSON, the rows shown.
    app.handle_key(key(KeyCode::Char('E')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.popups.export_form.is_none());
    let written = files(dir.path());
    assert!(written.len() == 1 && written[0].contains(".json"), "{written:?}");
    std::fs::remove_file(dir.path().join(&written[0])).unwrap();
    // The same again: E, Enter.
    app.handle_key(key(KeyCode::Char('E')));
    assert_eq!(export_choice(&app).format, crate::app::Format::Json);
    app.handle_key(key(KeyCode::Enter));
    assert!(files(dir.path())[0].contains(".json"));
}

#[test]
fn the_export_form_by_mouse_and_a_plans_table_has_only_its_own_rows() {
    use crate::app::export_form::{ExportClick, ExportTo};
    let (mut app, dir) = query_app();
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('E')));
    render(&mut app, 100, 30);
    let click_on = |app: &mut App, what: ExportClick| {
        let (r, _) = *app.hits.export_form_spots.iter().find(|(_, c)| *c == what).expect("a place to click");
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.x,
            row: r.y,
            modifiers: KeyModifiers::NONE,
        });
        render(app, 100, 30);
    };
    click_on(&mut app, ExportClick::Every(true));
    assert!(export_choice(&app).every);
    click_on(&mut app, ExportClick::NextFormat);
    assert_eq!(export_choice(&app).format, crate::app::Format::Json);
    click_on(&mut app, ExportClick::To(ExportTo::Clipboard));
    assert_eq!(export_choice(&app).to, ExportTo::Clipboard);
    assert!(!export_choice(&app).every, "the clipboard has no every row");
    click_on(&mut app, ExportClick::Run);
    assert!(app.popups.export_form.is_none());
    // A click outside closes it, without doing it.
    app.handle_key(key(KeyCode::Char('E')));
    render(&mut app, 100, 30);
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.popups.export_form.is_none());
    assert!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("podelis-export"))
    );
}

#[test]
fn the_open_menu() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('o')));
    let menu = app.popups.menu.as_ref().expect("a menu");
    let labels: Vec<&str> = menu.items.iter().map(|i| i.label.as_str()).collect();
    assert_snapshot!(labels.join("\n"));
}

#[test]
fn the_results_menu_without_rows() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('3')));
    app.handle_key(key(KeyCode::Char('m')));
    assert_snapshot!(items(&app));
}

#[test]
fn the_editor_menu() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    assert_snapshot!(items(&app));
}

#[test]
fn the_navigator_menu() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('m')));
    assert_snapshot!(items(&app));
}

#[test]
fn a_long_menu_scrolls_on_a_small_terminal() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    let last = app.popups.menu.as_ref().unwrap().items.last().unwrap().label.clone();
    let screen = render(&mut app, 80, 16);
    assert!(!screen.contains(&last), "{screen}");
    // Going past the first, round to the last: it comes into sight.
    app.handle_key(key(KeyCode::Up));
    let screen = render(&mut app, 80, 16);
    assert!(screen.contains(&last), "{screen}");
    let menu = app.popups.menu.as_ref().unwrap();
    assert!(menu.scroll > 0);
    // A click takes the row drawn there, not the row counted from the first.
    let rect = app.hits.menu;
    let on_first_row = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 3,
        row: rect.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    let expected = menu.scroll;
    app.handle_mouse(on_first_row);
    assert!(app.popups.menu.is_none(), "chosen: item {expected}");
}

#[test]
fn a_menu_fits_a_small_terminal_without_its_separators() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    let last = app.popups.menu.as_ref().unwrap().items.last().unwrap().label.clone();
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains(&last) && screen.contains("Show the full value"), "{screen}");
}

#[test]
fn a_dimmed_item_says_why_and_a_menu_item_does_what_its_key_does() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    let menu = app.popups.menu.as_ref().unwrap();
    assert!(menu.items.iter().any(|i| i.label == "Other results…" && i.dimmed));
    let at = menu.items.iter().position(|i| i.label == "Other results…").unwrap();
    app.popups.menu.as_mut().unwrap().selected = at;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.popups.menu.is_none());
    assert!(!app.notes.results_note.as_ref().map_or(&app.notes.status, |(m, _)| m).is_empty());
}

#[test]
fn every_command_with_a_key_is_in_the_keys_and_the_palette() {
    use crate::app::commands::{Command, Here, pane_key_list};
    let (app, _dir) = query_app();
    let help: Vec<String> = super::super::chrome::help_sections(&app)
        .into_iter()
        .flat_map(|(_, rows)| rows.into_iter().map(|(keys, what)| format!("{keys} {what}")))
        .collect();
    for action in crate::config::keys::Action::all() {
        let keys = app.keymap.label(action);
        if !keys.is_empty() {
            assert!(help.iter().any(|row| row.starts_with(keys)), "{action:?} isn't in the keys");
        }
    }
    let all = Command::all_for_tests();
    for here in [Here::Rows, Here::Messages, Here::PlanText, Here::Navigator] {
        for (keys, command) in pane_key_list(here) {
            let label = command.label(&app);
            assert!(help.iter().any(|row| row.contains(&label) && row.starts_with(&keys)), "{keys} {label}");
            if !matches!(command, Command::Action(_)) {
                assert!(all.contains(&command), "{command:?} isn't in the palette");
            }
        }
    }
}

#[test]
fn every_format_is_worded_alike_and_in_the_one_order() {
    use crate::app::{Dest, Format};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('C')));
    let copy: Vec<String> = app.popups.menu.as_ref().unwrap().items.iter().map(|i| i.label.clone()).collect();
    assert_eq!(copy.len(), Format::for_dest(Dest::Copy).count());
    assert_eq!(
        copy,
        [
            "Copy the rows as CSV",
            "Copy the rows as TSV",
            "Copy the rows as JSON",
            "Copy the rows as a Markdown table",
            "Copy the rows as an ASCII table",
            "Copy the rows as INSERT statements",
        ]
    );
}

#[test]
fn copying_as_tsv() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('C')));
    app.popups.menu.as_mut().unwrap().selected = 1;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.take_clipboard().is_some_and(|text| text.starts_with("id\temail\tnote\n")));
}

#[test]
fn the_plans_table_has_its_own_menu() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.tab.plan_swapped = true;
    app.handle_key(key(KeyCode::Char('m')));
    assert_snapshot!(items(&app));
    app.handle_key(key(KeyCode::Esc));
    // What changes data doesn't run over it.
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.popups.confirm.is_none());
}

#[test]
fn the_keys_window_scrolls() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('?')));
    assert!(app.help_visible);
    let top = render(&mut app, 100, 24);
    app.handle_key(key(KeyCode::PageDown));
    let lower = render(&mut app, 100, 24);
    assert!(app.help_visible && top != lower);
    app.handle_key(key(KeyCode::Char('?')));
    assert!(!app.help_visible && app.help_scroll == 0);
}

fn ctrl_k() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
}

#[test]
fn a_group_opens_with_its_letter_and_runs_with_the_next() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    // Ctrl+K s i: the connection's info, without a menu in between (with no
    // real connection in this test, it says so).
    app.handle_key(ctrl_k());
    app.handle_key(key(KeyCode::Char('s')));
    let palette = app.popups.palette.as_ref().expect("waiting in the group");
    assert_eq!(palette.group(), Some('s'));
    assert!(!palette.menu_shown());
    assert_eq!(app.notes.status, "^K s…");
    app.handle_key(key(KeyCode::Char('i')));
    assert!(app.popups.palette.is_none());
    assert_eq!(app.notes.editor_note.as_deref(), Some(t!("info.not_connected")));
}

#[test]
fn a_groups_menu_shows_after_the_pause_and_with_a_question_mark() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.handle_key(ctrl_k());
    app.handle_key(key(KeyCode::Char('s')));
    assert!(app.palette_menu_due().is_some(), "after the same pause");
    app.handle_key(key(KeyCode::Char('?')));
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("CTRL+K S") && screen.contains("s sessions") && screen.contains("i info"), "{screen}");
    assert!(!screen.contains("TABS") && !screen.contains("x mode"), "{screen}");
    assert_snapshot!(screen);
    // `?` again is every key, as everywhere.
    app.handle_key(key(KeyCode::Char('?')));
    assert!(app.popups.palette.is_none() && app.help_visible);
}

#[test]
fn a_stray_letter_in_a_group_runs_nothing() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.connection.txn = TxnState::Open;
    let text = app.tab.editor.text();
    app.handle_key(ctrl_k());
    app.handle_key(key(KeyCode::Char('s')));
    // `c` commits at the top: in this group it's nothing, and shows the group.
    app.handle_key(key(KeyCode::Char('c')));
    let palette = app.popups.palette.as_ref().expect("still in the group");
    assert!(palette.group() == Some('s') && palette.menu_shown() && !palette.searching());
    assert!(app.connection.txn == TxnState::Open && app.tab.editor.text() == text);
    // Backspace: the letters again; Esc from a group closes it all.
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.popups.palette.as_ref().unwrap().group(), None);
    app.handle_key(key(KeyCode::Char('s')));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.popups.palette.is_none());
}

#[test]
fn a_groups_m_is_the_panes_menu() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(ctrl_k());
    app.handle_key(key(KeyCode::Char('y')));
    app.handle_key(key(KeyCode::Char('m')));
    assert!(app.popups.palette.is_none());
    assert_eq!(app.popups.menu.as_ref().unwrap().title, "RESULTS");
}

#[test]
fn the_letters_given_up_search_instead() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.handle_key(ctrl_k());
    app.handle_key(key(KeyCode::Char('h')));
    let palette = app.popups.palette.as_ref().unwrap();
    assert!(palette.searching() && palette.query.text() == "h");
}

#[test]
fn filtering_from_the_palette_takes_the_keys_there() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    let text = app.tab.editor.text();
    app.set_result_tab_for_tests(ResultTab::Messages);
    app.run_command_for_tests(Command::FilterRows);
    assert!(app.focus == Focus::Results && app.tab.grid.filter_editing && app.tab.result_tab == ResultTab::Rows);
    app.handle_key(key(KeyCode::Char('z')));
    assert_eq!(app.tab.editor.text(), text, "typed into the filter, not the SQL");
    assert_eq!(app.tab.grid.filter.text(), "z");
    // The navigator's, even hidden.
    app.handle_key(key(KeyCode::Esc));
    app.focus = Focus::Editor;
    app.panes.schema_visible = false;
    app.run_command_for_tests(Command::FilterNames);
    assert!(app.focus == Focus::Schema && app.panes.schema_visible);
}

#[test]
fn all_commands_opens_the_list_at_once() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    let menu = app.popups.menu.as_mut().unwrap();
    let at = menu.items.iter().position(|i| i.label == "All commands…").unwrap();
    assert_eq!(menu.items[at].keys, "Ctrl+K /");
    menu.selected = at;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.popups.palette.as_ref().is_some_and(crate::app::palette::Palette::searching));
}

#[test]
fn copy_applies_to_the_line_as_ctrl_c_does_cut_doesnt() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    let items = &app.popups.menu.as_ref().unwrap().items;
    assert!(!items[0].dimmed && items[1].dimmed);
    app.handle_key(key(KeyCode::Esc));
    // A blank line: nothing to copy.
    app.tab.editor.set_text("");
    app.handle_key(KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE));
    assert!(app.popups.menu.as_ref().unwrap().items[0].dimmed);
}

#[test]
fn compare_is_dimmed_with_no_other_connection() {
    use crate::app::commands::{Can, Command};
    let (app, _dir) = query_app();
    assert!(matches!(app.can(Command::CompareSchema(None)), Can::No { .. }));
}

#[test]
fn space_in_a_menu_is_the_item_marked_space() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.popups.menu.is_none());
    assert_eq!(app.tab.grid.marked.len(), 1);
}

#[test]
fn markdowns_key_is_c_in_the_copy_menu_and_c_c_from_the_palette() {
    use crate::app::commands::Command;
    use crate::app::{Dest, Format};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    let markdown = Command::Rows { to: Dest::Copy, format: Format::Markdown };
    assert_eq!(markdown.keys(&app), "C C (results)");
    // Not in the menus of other formats.
    app.handle_key(key(KeyCode::Char('C')));
    let menu = app.popups.menu.as_ref().unwrap();
    assert_eq!(menu.items.iter().filter(|i| i.keys == "C").count(), 1);
}

#[test]
fn what_cant_change_data_is_dimmed_on_a_read_only_connection() {
    use crate::db::ConnectionMode;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.connection.mode = ConnectionMode::ReadOnly;
    app.handle_key(key(KeyCode::Char('m')));
    let menu = app.popups.menu.as_ref().unwrap();
    for label in ["Edit this cell", "Add a row", "Delete the marked rows…"] {
        assert!(menu.items.iter().any(|i| i.label == label && i.dimmed), "{label}");
    }
    let delete = menu.items.iter().position(|i| i.label == "Delete the marked rows…").unwrap();
    app.popups.menu.as_mut().unwrap().selected = delete;
    app.handle_key(key(KeyCode::Enter));
    assert!(app.popups.confirm.is_none());
}

#[test]
fn ctrl_and_alt_arent_a_panes_plain_letter() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
    assert!(!app.panes.zoomed);
    // Shift is how a capital is typed.
    app.handle_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT));
    assert!(app.popups.export_form.is_some());
    app.handle_key(key(KeyCode::Esc));
    // In a menu too: Alt+C isn't its `C`.
    app.handle_key(key(KeyCode::Char('m')));
    app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::ALT));
    assert_eq!(app.popups.menu.as_ref().map(|m| m.title), Some(t!("menu.results_title")));
}

#[test]
fn a_rows_menu_from_the_palette_is_the_results_with_their_keys() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.run_command_for_tests(Command::ExportMenu);
    assert_eq!(app.focus, Focus::Results);
    assert!(app.popups.export_form.is_some());
}

#[test]
fn the_navigators_filter_is_refused_where_it_isnt_drawn() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    let _ = render(&mut app, 80, 24);
    app.run_command_for_tests(Command::FilterNames);
    assert_eq!(app.focus, Focus::Editor);
    assert!(!app.connection.schema.as_ref().unwrap().filter_editing);
    assert_eq!(app.notes.status, "The terminal is too narrow for the navigator");
}

#[test]
fn refresh_needs_a_connection_not_a_navigator() {
    use crate::app::commands::{Can, Command};
    let (mut app, _dir) = query_app();
    app.connection.schema = None;
    assert!(matches!(app.can(Command::Definition), Can::No { .. }));
    assert_eq!(
        app.can(Command::RefreshSchema),
        Can::No { why: "This tab isn't connected".to_string(), refused: false }
    );
}

#[test]
fn refusals_look_alike_however_theyre_asked_for() {
    use crate::app::commands::Command;
    use crate::config::keys::Action;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.run_command_for_tests(Command::Action(Action::Commit));
    assert!(app.notes.status_is_error);
    // Over a plan's table, `f` is refused as an error.
    app.focus = Focus::Results;
    app.tab.plan_swapped = true;
    app.handle_key(key(KeyCode::Char('f')));
    assert_eq!(app.notes.results_note.as_ref().map(|(_, error)| *error), Some(true));
}

#[test]
fn the_keys_say_how_to_quit() {
    let (app, _dir) = query_app();
    let keys = super::super::chrome::help_sections(&app);
    assert!(keys.iter().flat_map(|(_, rows)| rows).any(|(key, _)| key == "Ctrl+Q"));
}

/// What a pane's command chosen away from its pane leaves behind: it either
/// goes to the pane (so what's typed next goes there too), or it's done at
/// once. A new command has to say which, or this fails.
#[tokio::test]
async fn a_panes_command_from_the_editor_goes_to_its_pane_or_is_done_at_once() {
    use crate::app::Dest;
    use crate::app::commands::{Command as C, Here, pane_key_list};
    let done_at_once = |c: C| {
        matches!(
            c,
            C::CopyCell
                | C::CopyRow
                | C::CopyMessage
                | C::CopyMessages
                | C::CopyPlanLines
                | C::CopyWholePlan
                | C::LoadMore
                | C::NextSet
                | C::PrevSet
                | C::EarlierResult
                | C::LaterResult
                | C::Pin
                | C::Chart
                | C::PipeRows
                | C::AddRow
                | C::DeleteRows
                | C::EditCell
                | C::WholeMessage
                | C::Definition
                | C::Related
                | C::SelectFrom
                | C::CopyName
                | C::OrderNames
                | C::SystemTables
                | C::RefreshSchema
                | C::Action(_)
        )
    };
    for here in [Here::Rows, Here::Messages, Here::PlanText, Here::Navigator] {
        for (_, command) in pane_key_list(here) {
            // Rows to a file or an app aren't for a test to write or start.
            if matches!(command, C::Rows { to: Dest::ExportShown | Dest::Open, .. }) || done_at_once(command) {
                continue;
            }
            let (mut app, _dir) = query_app();
            app.focus = Focus::Editor;
            app.run_command_for_tests(command);
            let want = if here == Here::Navigator { Focus::Schema } else { Focus::Results };
            assert_eq!(app.focus, want, "{command:?}");
            let filtering = app.tab.grid.filter_editing;
            assert!(!filtering || app.focus == Focus::Results, "{command:?}");
        }
    }
}

#[test]
fn showing_a_tab_from_the_palette_gives_it_the_keys() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.tab.editor.set_text("select 1");
    for (command, tab) in [
        (Command::ShowMessages, ResultTab::Messages),
        (Command::ShowPlan, ResultTab::Plan),
        (Command::ShowRows, ResultTab::Rows),
    ] {
        app.focus = Focus::Editor;
        app.run_command_for_tests(command);
        assert_eq!((app.focus, app.tab.result_tab), (Focus::Results, tab));
    }
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(app.tab.editor.text(), "select 1", "y isn't typed into the SQL");
}

#[test]
fn the_narrow_check_holds_while_zoomed() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.panes.zoomed = true;
    let _ = render(&mut app, 80, 24);
    assert!(app.hits.navigator_too_narrow);
    app.run_command_for_tests(Command::FilterNames);
    assert!(!app.connection.schema.as_ref().unwrap().filter_editing && app.focus == Focus::Editor);
    // Wide enough, it isn't refused.
    let _ = render(&mut app, 120, 30);
    assert!(!app.hits.navigator_too_narrow);
}

#[test]
fn the_palette_finds_a_panes_command_by_the_words_people_use() {
    let (mut app, _dir) = query_app();
    for (typed, found) in [
        ("jq", "Pipe rows through a command (grep, sort, jq…)"),
        ("grep", "Pipe rows through a command (grep, sort, jq…)"),
        ("refresh navigator", "Refresh the navigator"),
        ("export all", "Export every row as CSV (all rows, not just those loaded)"),
        ("other results", "Other results…"),
    ] {
        app.handle_key(ctrl_k());
        app.handle_key(key(KeyCode::Char('/')));
        for c in typed.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let texts: Vec<String> = app.palette_items().iter().map(|c| c.text(&app)).collect();
        assert!(texts.iter().any(|t| t == found), "{typed}: {texts:?}");
        app.handle_key(key(KeyCode::Esc));
    }
}

#[test]
fn a_letter_taken_from_a_group_is_listed_once() {
    use crate::config::keys::{KeyMap, KeySpec};
    use std::collections::HashMap;
    let (mut app, _dir) = query_app();
    let overrides = HashMap::from([("zoom".to_string(), KeySpec::One("Ctrl+K s".into()))]);
    app.keymap = KeyMap::new(&overrides).0;
    app.focus = Focus::Editor;
    app.handle_key(ctrl_k());
    app.show_palette_menu();
    let screen = render(&mut app, 90, 30);
    assert!(screen.contains("s zoom") && !screen.contains("server…"), "{screen}");
}

#[test]
fn copy_menu_starts_on_markdown_so_enter_copies_it() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('C')));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.take_clipboard().is_some_and(|text| text.starts_with("| id | email | note |")));
}

#[test]
fn the_wheel_moves_a_menu_and_the_keys() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    let wheel = |kind| MouseEvent { kind, column: 5, row: 5, modifiers: KeyModifiers::NONE };
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char('m')));
    let last = app.popups.menu.as_ref().unwrap().items.len() - 1;
    app.handle_mouse(wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.popups.menu.as_ref().unwrap().selected, 0, "stops at the top");
    for _ in 0..=last {
        app.handle_mouse(wheel(MouseEventKind::ScrollDown));
    }
    assert_eq!(app.popups.menu.as_ref().unwrap().selected, last, "stops at the bottom");
    app.handle_key(key(KeyCode::Esc));
    // The keys, which say how far down they are.
    app.handle_key(key(KeyCode::Char('?')));
    let top = render(&mut app, 100, 24);
    assert!(top.contains("KEYS 1–22/"), "{top}");
    app.handle_mouse(wheel(MouseEventKind::ScrollDown));
    assert!(app.help_scroll > 0);
    let lower = render(&mut app, 100, 24);
    assert!(!lower.contains("KEYS 1–") && lower.contains("KEYS "), "{lower}");
}

#[test]
fn every_row_is_every_row_whatever_a_filter_shows() {
    use crate::app::commands::Command;
    use crate::app::{Dest, Format};
    let (mut app, dir) = query_app();
    app.focus = Focus::Results;
    app.settings.export_dir = Some(dir.path().to_string_lossy().into_owned());
    app.tab.grid.filter.insert_str("ana");
    app.tab.grid.refresh_view();
    assert_eq!(app.tab.grid.view.len(), 1);
    let exported = |dir: &std::path::Path| -> String {
        let file = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("podelis-export"))
            .expect("the file");
        let text = std::fs::read_to_string(file.path()).unwrap();
        std::fs::remove_file(file.path()).unwrap();
        text
    };
    app.run_command_for_tests(Command::Rows { to: Dest::ExportAll, format: Format::Csv });
    let all = exported(dir.path());
    assert!(all.contains("ana@") && all.contains("ben@") && all.contains("cleo@"), "{all}");
    assert!(app.notes.status.contains("3 rows"), "{}", app.notes.status);
    // Sorted, every row comes as it's sorted: by id, descending.
    app.tab.grid.sort = Some((0, true));
    app.tab.grid.refresh_view();
    app.run_command_for_tests(Command::Rows { to: Dest::ExportAll, format: Format::Csv });
    let sorted = exported(dir.path());
    let ids: Vec<&str> = sorted.lines().skip(1).filter_map(|line| line.split(',').next()).collect();
    assert_eq!(ids.first(), Some(&"3"), "{sorted}");
    assert_eq!(ids.last(), Some(&"1"), "{sorted}");
    app.tab.grid.sort = None;
    app.tab.grid.refresh_view();
    // Those shown are still those shown.
    app.run_command_for_tests(Command::Rows { to: Dest::ExportShown, format: Format::Csv });
    let shown = exported(dir.path());
    assert!(shown.contains("ana@") && !shown.contains("ben@"), "{shown}");
}

#[test]
fn refusals_are_errors_and_what_isnt_there_isnt() {
    use crate::app::commands::{Can, Command};
    use crate::config::keys::Action;
    let (mut app, _dir) = query_app();
    assert!(matches!(app.can(Command::Action(Action::Commit)), Can::No { refused: true, .. }));
    assert!(matches!(app.can(Command::CompareSchema(None)), Can::No { refused: true, .. }));
    assert!(matches!(app.can(Command::EarlierResult), Can::No { refused: false, .. }));
    app.tab.plan_swapped = true;
    assert!(matches!(app.can(Command::EditCell), Can::No { refused: true, .. }));
}

#[test]
fn ctrl_c_and_y_copy_a_name_alike() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    app.handle_key(key(KeyCode::Char('y')));
    let by_y = app.take_clipboard();
    assert!(by_y.is_some());
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(app.take_clipboard(), by_y);
}

#[test]
fn with_every_row_filtered_out_what_needs_rows_is_dimmed_and_marks_still_count() {
    use crate::app::commands::{Can, Command};
    use crate::app::{Dest, Format};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Results;
    app.handle_key(key(KeyCode::Char(' ')));
    assert_eq!(app.tab.grid.marked.len(), 1);
    app.tab.grid.filter.insert_str("nothing like this");
    app.tab.grid.refresh_view();
    let csv = Command::Rows { to: Dest::Copy, format: Format::Csv };
    // Pipe takes those shown: none. Copy and open take the marked one.
    assert!(matches!(app.can(Command::PipeRows), Can::No { .. }));
    assert_eq!(app.can(csv), Can::Yes);
    assert_eq!(app.can(Command::OpenRows), Can::Yes);
    app.tab.grid.marked.clear();
    assert!(matches!(app.can(csv), Can::No { .. }) && matches!(app.can(Command::OpenRows), Can::No { .. }));
    // The keys say so too, rather than opening a prompt with nothing to send.
    app.handle_key(key(KeyCode::Char('|')));
    assert!(app.popups.pipe_prompt.is_none());
    app.handle_key(key(KeyCode::Char('o')));
    assert!(app.popups.menu.is_none());
}

#[test]
fn select_and_related_need_a_table_selected() {
    use crate::app::SchemaNode;
    use crate::app::commands::{Can, Command};
    let (mut app, _dir) = query_app();
    assert_eq!(app.can(Command::SelectFrom), Can::Yes);
    assert_eq!(app.can(Command::Related), Can::Yes);
    let panel = app.connection.schema.as_mut().unwrap();
    panel.selected = panel.nodes().iter().position(|n| matches!(n, SchemaNode::Section(_))).unwrap();
    assert_eq!(app.can(Command::SelectFrom), Can::No { why: "Select a table first".to_string(), refused: false });
    assert!(matches!(app.can(Command::Related), Can::No { .. }));
    assert!(matches!(app.can(Command::Definition), Can::No { .. }));
}

#[test]
fn a_refused_edit_from_the_palette_stays_where_it_is() {
    use crate::app::commands::Command;
    let (mut app, _dir) = query_app();
    app.focus = Focus::Editor;
    app.set_result_tab_for_tests(ResultTab::Messages);
    for command in [Command::EditCell, Command::AddRow, Command::DeleteRows] {
        app.run_command_for_tests(command);
        assert_eq!((app.focus, app.tab.result_tab), (Focus::Editor, ResultTab::Messages), "{command:?}");
        assert!(app.notes.status_is_error, "{command:?}");
    }
}
