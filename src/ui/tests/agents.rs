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
    app.ask_prompt = Some(crate::app::LineInput::new("revenue per country last month"));
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains("ASK ABOUT PROD"), "{screen}");
    assert!(screen.contains("? revenue per country last month"), "{screen}");
    assert!(screen.contains("it lands in the editor and doesn't run"), "{screen}");
    app.ask_prompt = None;
    app.confirm = Some(Confirm {
        message: "Send shop's table and column names to the model?".into(),
        details: vec!["project 'shop' shares its schemas only with models on this machine".into()],
        danger: false,
        action: ConfirmAction::ShareSchema {
            question: crate::app::ask::Question { text: "x".into(), tab: 0, connection: "prod".into() },
            project_id: None,
        },
        offer_transaction: false,
    });
    let screen = render(&mut app, 110, 24);
    assert!(screen.contains(" y  this once") && screen.contains(" a  always, for this project"), "{screen}");
    // A detail too long for the box wraps under its text, and the box
    // grows with it: its last row (the keys) still shows.
    if let Some(confirm) = &mut app.confirm {
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
fn the_memory_window_marks_what_waits_for_the_user() {
    let (mut app, _dir) = query_app();
    let fact = |id: i64, subject: &str, value: &str, confirmed: bool, source: &str| serde_json::json!({ "id": id, "subject": subject, "relation": "means", "value": value, "confirmed": confirmed, "source": source });
    app.memory_view = Some(crate::app::memory_view::MemoryView {
        facts: vec![
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
    assert!(screen.contains("a add · c confirm · d forget · Esc close"), "{screen}");
    // A long source is cut from its start; the fact stays whole.
    assert!(screen.contains("orders means note An order: status 1 new, 2 paid  · …x"), "{screen}");
    assert!(screen.contains("x/db/migrations"), "{screen}");
}
