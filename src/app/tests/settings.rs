use super::*;
use crate::app::settings_view::Field;

#[test]
fn settings_change_in_place_and_are_saved_keeping_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _store_dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    std::fs::write(dir.path().join("config.toml"), "# my settings\nrow_limit = 500\n").unwrap();
    app.screen = Screen::Query;
    app.handle_key(ctrl('k'));
    app.handle_key(key(KeyCode::Char(',')));
    assert!(app.settings_view.is_some());
    // Lint: Enter turns it off, at once, and it's saved.
    let at = |field| Field::ALL.iter().position(|f| *f == field).unwrap();
    app.settings_view.as_mut().unwrap().selected = at(Field::Lint);
    assert!(app.settings.lint);
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.settings.lint);
    let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(text.starts_with("# my settings\nrow_limit = 500\n") && text.contains("lint = false"), "{text}");
    // A number is typed: Enter, the new one, Enter.
    app.settings_view.as_mut().unwrap().selected = at(Field::RowLimit);
    app.handle_key(key(KeyCode::Enter));
    app.handle_key(ctrl('u'));
    type_text(&mut app, "2000");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.settings.row_limit, 2000);
    let saved = crate::config::settings::Settings::load(dir.path()).0;
    assert_eq!((saved.row_limit, saved.lint), (2000, false));
    // Not a number: refused, said so, nothing changed.
    app.handle_key(key(KeyCode::Enter));
    type_text(&mut app, "x");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.settings.row_limit, 2000);
    assert!(app.settings_view.as_ref().unwrap().said.as_ref().is_some_and(|(_, error)| *error));
    // The theme changes at once; Esc closes.
    app.settings_view.as_mut().unwrap().selected = at(Field::Theme);
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.settings.theme, crate::config::settings::ThemeName::Light);
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.settings_view.is_none());
    assert_eq!(app.setting_shown(Field::Theme), "dark");
}
