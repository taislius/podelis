use super::*;

#[test]
fn stepping_through_definitions_keeps_the_popup_size() {
    let (mut app, _dir) = query_app();
    let size = |screen: &str| {
        let lines: Vec<&str> = screen.lines().collect();
        let top = lines.iter().position(|l| l.contains("┌ TABLE")).unwrap_or_else(|| panic!("{screen}"));
        let bottom = top + lines[top..].iter().position(|l| l.contains("Esc close")).unwrap();
        (bottom - top, lines[top].matches('─').count())
    };
    app.cell_view = Some(crate::app::CellView::definition("TABLE a".into(), "id int".into()));
    let small = size(&render(&mut app, 120, 40));
    let long = (0..20).map(|i| format!("column_with_a_long_name_{i}  varchar(255) not null")).collect::<Vec<_>>();
    app.cell_view = Some(crate::app::CellView::definition("TABLE b".into(), long.join("\n")));
    assert_eq!(size(&render(&mut app, 120, 40)), small);
}

#[test]
fn a_definition_sits_beside_the_navigator() {
    let (mut app, _dir) = query_app();
    app.cell_view = Some(crate::app::CellView::definition("TABLE users".into(), "id int".into()));
    let screen = render(&mut app, 120, 40);
    let nav = app.hits.schema.unwrap();
    let top = screen.lines().find(|l| l.contains("┌ TABLE users")).unwrap_or_else(|| panic!("{screen}"));
    // Rendered lines are quoted: the `"` comes first.
    let x = top[..top.find("┌ TABLE").unwrap()].chars().count() as u16 - 1;
    assert_eq!(x, nav.right(), "just right of the navigator:\n{screen}");
}

#[test]
fn system_tables_in_their_own_section() {
    let (mut app, _dir) = query_app();
    let panel = app.connection.schema.as_mut().unwrap();
    for (schema, name, is_view) in [("pg_catalog", "pg_class", false), ("information_schema", "tables", true)] {
        let info = TableInfo {
            schema: schema.into(),
            name: name.into(),
            is_view,
            rows: None,
            bytes: None,
            system: true,
            partition_of: None,
        };
        panel.tables.push(SchemaTable {
            info,
            expanded: false,
            columns: vec![],
            details: None,
            details_loading: false,
        });
    }
    app.focus = Focus::Schema;
    assert_snapshot!(render(&mut app, 90, 16));
}

#[test]
fn hidden_schema_pane_cannot_keep_focus() {
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    let _ = render(&mut app, 60, 20); // too narrow for the navigator
    assert!(!app.panes.schema_shown);
    assert_eq!(app.focus, Focus::Editor);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.focus, Focus::Editor, "Tab skips the hidden pane");
}

#[test]
fn navigator_pages_reveals_expanded_columns_and_sorts() {
    let (mut app, _dir) = query_app();
    let col =
        |i: usize| ColumnInfo { name: format!("c{i}"), data_type: "int".into(), nullable: true, primary_key: false };
    let tables = (0..40)
        .map(|i| SchemaTable {
            info: TableInfo {
                schema: "public".into(),
                name: format!("t{i:02}"),
                is_view: false,
                rows: Some(i * 10),
                bytes: Some(1000 - i),
                system: false,
                partition_of: None,
            },
            expanded: false,
            columns: (0..8).map(col).collect(),
            details: None,
            details_loading: false,
        })
        .collect();
    app.connection.schema = Some(SchemaPanel { tables, selected: 1, ..SchemaPanel::default() });
    app.focus = Focus::Schema;
    let _ = render(&mut app, 120, 30);
    let page = app.connection.schema.as_ref().unwrap().page;
    assert!(page > 5 && page < 40);
    let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.connection.schema.as_ref().unwrap().selected, 1 + page);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.connection.schema.as_ref().unwrap().selected, 1);
    // The last table on screen, expanded: its columns scroll into view.
    let _ = render(&mut app, 120, 30);
    for _ in 2..page {
        press(&mut app, KeyCode::Down);
    }
    let _ = render(&mut app, 120, 30);
    let selected = app.connection.schema.as_ref().unwrap().selected;
    assert_eq!(app.hits.schema_offset, 0);
    press(&mut app, KeyCode::Enter);
    let screen = render(&mut app, 120, 30);
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(panel.scroll <= selected && selected + 8 < panel.scroll + panel.page, "columns shown:\n{screen}");
    assert!(screen.contains("c7"));
    // Ordered by rows (most first), then by size.
    press(&mut app, KeyCode::Char('o'));
    let nodes = app.connection.schema.as_ref().unwrap().nodes();
    assert_eq!(nodes[1], crate::app::SchemaNode::Table(39));
    assert!(render(&mut app, 120, 30).contains("by rows"));
    press(&mut app, KeyCode::Char('o'));
    let nodes = app.connection.schema.as_ref().unwrap().nodes();
    assert_eq!(nodes[1], crate::app::SchemaNode::Table(0));
    // The expanded table stays selected (and on screen), showing its size.
    let screen = render(&mut app, 120, 30);
    let t = app.connection.schema.as_ref().unwrap().nodes()[app.connection.schema.as_ref().unwrap().selected];
    let crate::app::SchemaNode::Table(t) = t else { panic!("{t:?}") };
    assert!(screen.contains("by size") && screen.contains(&format!("{} B", 1000 - t)), "{screen}");
}

#[test]
fn clicking_a_table_while_filtering_the_navigator_gives_it_the_keys() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _dir) = query_app();
    app.focus = Focus::Schema;
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    let _ = render(&mut app, 100, 30);
    let list = app.hits.schema_list;
    let click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: list.x + 2,
        row: list.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    app.handle_mouse(click);
    let panel = app.connection.schema.as_ref().unwrap();
    assert!(!panel.filter_editing, "the click ends typing the filter");
    assert_eq!(panel.filter.text(), "o", "the filter stays");
    // `d` is for the clicked table now, not the filter.
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(app.connection.schema.as_ref().unwrap().filter.text(), "o");
}
