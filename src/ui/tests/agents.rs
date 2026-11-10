use super::*;

/// The agent activity window: the sessions, then one session's calls; and
/// the status bar's note about what agents did since it was last open.
#[test]
fn agent_activity_window() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = query_app();
    app.set_data_dir(dir.path().to_path_buf());
    let project = app.current_project.clone().unwrap();
    let mut shared = app.store.project(&project).unwrap().connections[0].clone();
    shared.agents = crate::db::AgentAccess::Read;
    let name = shared.name.clone();
    app.store.upsert(&project, Some(&name), shared).unwrap();
    let log = log_path(dir.path(), &project);
    let entry = |session: &str, at: &str, conn: &str, sql: &str| Entry {
        session: session.into(),
        at: format!("2026-10-13T{at}+03:00"),
        agent: "claude-code".into(),
        agent_version: Some("2.0.31".into()),
        dir: Some("/src/shop".into()),
        connection: conn.into(),
        tool: "query".into(),
        sql: sql.into(),
        ms: 3,
        ..Entry::default()
    };
    append(&log, Entry { rows: Some(42), ..entry("4f1c9a2e", "09:15:02", "shop", "select count(*) from orders") })
        .unwrap();
    append(
        &log,
        Entry {
            name: Some("billing".into()),
            rows: Some(7),
            ..entry("77ab01cd", "09:40:10", "shop", "select * from invoices")
        },
    )
    .unwrap();
    append(
        &log,
        Entry {
            refused: Some("“delete from users” is destructive".into()),
            ..entry("4f1c9a2e", "09:16:30", "prod", "delete from users")
        },
    )
    .unwrap();
    append(
        &log,
        Entry {
            dry_run: true,
            changed: Some(2),
            ..entry("4f1c9a2e", "09:18:44", "prod", "update users set note = 'x' where id < 3")
        },
    )
    .unwrap();
    app.update_agent_alerts();
    let screen = render(&mut app, 120, 14);
    assert!(screen.contains("agents: 1 refused · 1 dry run"), "{screen}");

    app.open_agents();
    assert_snapshot!("agent_sessions", render(&mut app, 120, 14));
    app.handle_agents_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_agents_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_snapshot!("agent_session_calls", render(&mut app, 120, 14));
}

#[test]
fn asking_in_plain_language_and_sharing_a_schema() {
    let (mut app, _dir) = query_app();
    app.connection.form.name = "prod".into();
    app.popups.confirm = Some(Confirm {
        message: "Send shop's table and column names to the model?".into(),
        details: vec!["project 'shop' shares its schemas only with models on this machine".into()],
        danger: false,
        action: ConfirmAction::ShareSchema {
            question: crate::app::query_screen::ask::Question {
                at: None,
                text: "x".into(),
                tab: 0,
                connection: "prod".into(),
                explain: false,
            },
            project_id: None,
        },
        offer_transaction: false,
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains(" y  this once") && screen.contains(" a  always, for this project"), "{screen}");
    // A detail too long for the box wraps under its text, and the box
    // grows with it: its last row (the keys) still shows.
    if let Some(confirm) = &mut app.popups.confirm {
        confirm.details = vec![
            "project 'shop' shares its schemas only with models on this machine, and far (x) isn't one: its \
             table and column names would go to https://nowhere.invalid/v1"
                .into(),
            "Rows are never sent; only the tables that matter for the question.".into(),
        ];
    }
    let screen = render(&mut app, 80, 24);
    assert!(screen.contains(" a  always, for this project"), "{screen}");
    let wrapped = screen.lines().find(|l| l.contains("names would go") || l.contains("would go to")).unwrap();
    assert!(wrapped.contains("│    "), "indented under the text: {screen}");
}

#[test]
fn on_a_connection_the_memory_window_is_its_databases_ontology() {
    let (mut app, _dir) = query_app();
    use crate::app::windows::memory::MemoryLine;
    use podelis_ai::ontology::outline::SectionKind as K;
    let header = |kind, subject: &str| MemoryLine::Header { kind, subject: subject.into() };
    let fact = |id: i64, subject: &str, relation: &str, value: &str, database: &str, section, confirmed: bool| {
        let source = if confirmed { "user" } else { "model:claude" };
        let fact = podelis_ai::memory::Fact {
            id,
            subject: subject.into(),
            relation: relation.into(),
            value: value.into(),
            source: source.into(),
            confirmed,
            at: String::new(),
            database: database.into(),
        };
        MemoryLine::Fact { fact, section: Some(section) }
    };
    app.popups.memory_view = Some(crate::app::windows::memory::MemoryView {
        database: Some("shop".into()),
        lines: vec![
            header(K::Table, "orders"),
            fact(1, "orders", "excluded by default", "status = 3", "shop", K::Table, true),
            header(K::Column, "orders.status"),
            fact(2, "orders.status", "means", "3 = cancelled", "", K::Column, true),
            header(K::Term, "revenue"),
            fact(3, "revenue", "defined as", "sum(orders.total)", "shop", K::Term, true),
            header(K::Suggested, ""),
            fact(4, "orders.total", "is", "with VAT", "shop", K::Suggested, false),
        ],
        ..Default::default()
    });
    let screen = render(&mut app, 120, 26);
    assert!(screen.contains("ONTOLOGY · SHOP · shop"), "{screen}");
    assert!(screen.contains(" table orders"), "{screen}");
    assert!(screen.contains("     excluded by default status = 3  · user"), "{screen}");
    assert!(screen.contains("       means 3 = cancelled  · user · every database"), "{screen}");
    assert!(screen.contains(" term revenue"), "{screen}");
    assert!(screen.contains("suggested by a model"), "{screen}");
    assert!(screen.contains("orders.total is with VAT  · model:claude, not confirmed: c"), "{screen}");
    assert!(screen.contains("p this or every database · E your editor"), "{screen}");
    // `a` on a header: the fact starts with what it's about.
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    let typed = app.popups.memory_view.as_ref().unwrap().adding.as_ref().map(|i| i.text().to_string());
    assert_eq!(typed.as_deref(), Some("orders "));
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    // `e` on a header does nothing: it isn't a fact.
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    assert!(app.popups.memory_view.as_ref().unwrap().adding.is_none());
}

#[test]
fn the_memory_window_marks_what_waits_for_the_user() {
    let (mut app, _dir) = query_app();
    let fact = |id: i64, subject: &str, value: &str, confirmed: bool, source: &str| {
        crate::app::windows::memory::MemoryLine::Fact {
            fact: podelis_ai::memory::Fact {
                id,
                subject: subject.into(),
                relation: "means".into(),
                value: value.into(),
                source: source.into(),
                confirmed,
                at: String::new(),
                database: String::new(),
            },
            section: None,
        }
    };
    app.popups.memory_view = Some(crate::app::windows::memory::MemoryView {
        lines: vec![
            fact(1, "orders.status", "3 = cancelled", true, "user"),
            fact(2, "orders.total", "VAT included", false, "model:claude"),
            fact(
                3,
                "orders",
                "note An order: status 1 new, 2 paid",
                true,
                &format!("file:/home/{}/db/migrations", "x".repeat(80)),
            ),
        ],
        ..Default::default()
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains("MEMORY · SHOP"), "{screen}");
    assert!(screen.contains("orders.status means 3 = cancelled  · user"), "{screen}");
    assert!(screen.contains("orders.total means VAT included  · model:claude, not confirmed: c"), "{screen}");
    assert!(screen.contains("a add · e change · c confirm · d forget · Esc close"), "{screen}");
    // A long source is cut from its start; the fact stays whole.
    assert!(screen.contains("orders means note An order: status 1 new, 2 paid  · …x"), "{screen}");
    assert!(screen.contains("x/db/migrations"), "{screen}");
    // `e`: the selected fact whole, to change.
    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    let view = app.popups.memory_view.as_ref().unwrap();
    assert_eq!(view.editing, Some(1));
    assert_eq!(view.adding.as_ref().map(crate::app::LineInput::text), Some("orders.status means 3 = cancelled"));
    let screen = render(&mut app, 110, 24);
    assert!(
        screen.contains("✎ orders.status means 3 = cancelled") && screen.contains("↵ save · Esc keep it"),
        "{screen}"
    );
    // Esc keeps it as it was.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let view = app.popups.memory_view.as_ref().unwrap();
    assert!(view.adding.is_none() && view.editing.is_none());
}

/// The edits' plan scrolls no further than its end, and `y` waits until
/// every change has been in sight.
#[test]
fn the_edits_plan_scrolls_to_its_end_and_no_further() {
    let (mut app, dir) = query_app();
    let file = dir.path().join("edits.yaml");
    std::fs::write(&file, "database: shop\n").unwrap();
    let adds: Vec<String> = (0..40).map(|i| format!("orders.c{i} means {i}")).collect();
    app.popups.memory_view = Some(crate::app::windows::memory::MemoryView {
        database: Some("shop".into()),
        plan: Some((serde_json::json!({ "add": adds }), file)),
        ..Default::default()
    });
    let _ = render(&mut app, 100, 20);
    let shown = app.popups.memory_view.as_ref().unwrap().plan_shown.get();
    assert!(shown > 0 && shown < 41, "{shown}");
    // `y` before the end has been seen: said, nothing loaded.
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(app.popups.memory_view.as_ref().unwrap().plan.is_some());
    for _ in 0..10 {
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
    }
    let scroll = app.popups.memory_view.as_ref().unwrap().plan_scroll;
    assert_eq!(scroll, 41 - shown, "stopped at the end");
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.popups.memory_view.as_ref().unwrap().plan_scroll, scroll - 1, "↑ moves at once");
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("orders.c38 means 38"), "{screen}");
}
