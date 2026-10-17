use super::*;

#[test]
fn memory_changes_name_the_fact() {
    use crate::app::memory_view::GatewayPurpose;
    let (mut app, _dir) = test_app();
    let project = project_id(&app);
    app.current_project = Some(project.clone());
    // Confirmed or forgotten: the fact as the window showed it (the answer
    // has only its number).
    let shown = Some("orders.total unit euro cents".to_string());
    app.gateway_done(
        GatewayPurpose::MemoryChanged("Confirmed", project.clone(), shown),
        Ok(serde_json::json!({ "id": 8 })),
    );
    assert_eq!(app.notes.status, "Confirmed orders.total unit euro cents");
    // Added: the fact the gateway kept, worded the same way.
    let kept = serde_json::json!({ "id": 9, "subject": "customers.email", "relation": "means", "value": "unique, lower-case" });
    app.gateway_done(GatewayPurpose::MemoryChanged("Remembered", project, None), Ok(kept));
    assert_eq!(app.notes.status, "Remembered customers.email means unique, lower-case");
}

/// The agent activity window: sessions, a session's calls, a call in full,
/// opening one in a tab; and the status bar's note until it's looked at.
#[test]
fn agent_activity_by_session() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "dev".into(), agents: crate::db::AgentAccess::Read, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let log = log_path(dir.path(), &project_id(&app));
    let entry = |session: &str, at: &str, sql: &str| Entry {
        session: session.into(),
        at: format!("2026-10-13T{at}:00+03:00"),
        agent: "claude-code".into(),
        agent_version: Some("2.0".into()),
        dir: Some("/src/shop".into()),
        connection: "dev".into(),
        tool: "query".into(),
        sql: sql.into(),
        ..Entry::default()
    };
    append(&log, entry("a", "09:00", "select 1")).unwrap();
    append(&log, Entry { name: Some("billing".into()), ..entry("b", "10:00", "select * from invoices") }).unwrap();
    append(&log, Entry { refused: Some("destructive".into()), ..entry("a", "11:00", "delete from users") }).unwrap();

    // Nothing looked at yet: the note says what's worth a look.
    app.agents.checked = None;
    app.update_agent_alerts();
    assert_eq!(app.agent_note().as_deref(), Some("agents: 1 refused"));

    app.open_agents();
    assert_eq!(app.agent_note(), None, "looked at");
    let labels: Vec<&str> = app.agents_rows().iter().map(|&i| app.agents.sessions[i].label()).collect();
    assert_eq!(labels, ["claude-code", "billing"], "the most recently active first");
    // Only what's worth a look; then search.
    app.handle_agents_key(key(KeyCode::Char('!')));
    assert_eq!(app.agents_rows().len(), 1);
    app.handle_agents_key(key(KeyCode::Char('!')));
    app.handle_agents_key(key(KeyCode::Char('/')));
    for c in "billing".chars() {
        app.handle_agents_key(key(KeyCode::Char(c)));
    }
    app.handle_agents_key(key(KeyCode::Enter));
    assert_eq!(app.agents_rows().len(), 1);
    app.handle_agents_key(key(KeyCode::Char('/')));
    app.handle_agents_key(key(KeyCode::Esc));

    // A session's calls, newest first, and one in full.
    app.handle_agents_key(key(KeyCode::Enter));
    let calls: Vec<&str> = app.agents_rows().iter().map(|&i| app.agents.log[i].entry.sql.as_str()).collect();
    assert_eq!(calls, ["delete from users", "select 1"]);
    app.handle_agents_key(key(KeyCode::Enter));
    let detail = &app.cell_view.as_ref().unwrap().text;
    assert!(detail.contains("Agent       claude-code 2.0") && detail.contains("Refused: destructive"), "{detail}");
    assert_eq!(app.top_overlay(), Some(Overlay::CellView), "in front of the window");
    app.cell_view = None;

    // Open in a tab: named for the agent; the status says where it ran.
    app.handle_agents_key(key(KeyCode::Char('o')));
    assert!(app.agents.view.is_none());
    assert_eq!(app.tab.editor.text(), "delete from users");
    assert_eq!(app.tabs[app.active_tab].name, "claude-code");
    assert!(app.notes.status.contains("ran this on dev"), "{}", app.notes.status);

    // Something new after looking brings the note back.
    append(&log, Entry { changed: Some(3), ..entry("a", "12:00", "update t set x = 1 where id = 1") }).unwrap();
    app.agents.checked = None;
    app.update_agent_alerts();
    assert_eq!(app.agent_note().as_deref(), Some("agents: 1 wrote"));

    // Another project, nothing shared: no note, nothing from this one.
    let other = app.store.add_project("other").unwrap();
    app.open_project(&other);
    assert_eq!(app.agent_note(), None);
    assert!(app.agents.log.is_empty() && app.agents.view.is_none());
}

/// An agent's write waiting for the user: asked about with its SQL and dry
/// run (and a bell); y approves, anything else says no.
#[test]
fn agent_writes_wait_for_your_answer() {
    use crate::agents::{Request, answer, ask, pending_dir, read_answer};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "prod".into(), agents: crate::db::AgentAccess::Approve, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let pending = pending_dir(dir.path(), &project_id(&app));
    let request = |id: &str| Request {
        id: id.into(),
        agent: "billing".into(),
        connection: "prod".into(),
        production: true,
        sql: "update invoices set paid = true\nwhere id = 7".into(),
        dry_run: "dry run: would change 1 row(s)".into(),
    };
    ask(&pending, &request("1")).unwrap();
    ask(&pending, &request("2")).unwrap();
    app.approvals_tick();
    // A key typed as it appears, or any but y / n / Esc, doesn't answer.
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(read_answer(&pending, "1"), None, "too soon");
    app.agents.asked = None;
    app.handle_key(key(KeyCode::Char('x')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(read_answer(&pending, "1"), None);
    let confirm = app.confirm.as_ref().unwrap();
    assert_eq!(confirm.message, "billing asks to write on prod (PRODUCTION). Allow it?");
    assert_eq!(
        confirm.details,
        ["update invoices set paid = true", "where id = 7", "", "dry run: would change 1 row(s)"]
    );
    assert!(confirm.danger);
    assert!(app.take_notification().is_some_and(|n| n.contains("billing asks to write")));
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(read_answer(&pending, "1"), Some(true));
    // The next one, once this is answered; Esc says no.
    app.approvals_tick();
    app.agents.asked = None;
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(read_answer(&pending, "2"), Some(false));
    app.approvals_tick();
    assert!(app.confirm.is_none());
    // The agent gave up while the question was up.
    ask(&pending, &request("3")).unwrap();
    app.approvals_tick();
    app.agents.asked = None;
    crate::agents::forget(&pending, "3");
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.notes.status.contains("stopped waiting"), "{}", app.notes.status);
    assert!(!answer(&pending, "3", true));
}

/// While the agent window is open, new calls don't move its rows; Esc out
/// of a session found by searching goes back to it, still searched.
#[test]
fn agent_window_stays_put() {
    use crate::agents::{Entry, append, log_path};
    let (mut app, dir) = test_app();
    app.set_data_dir(dir.path().to_path_buf());
    let shared = ConnectForm { name: "dev".into(), agents: crate::db::AgentAccess::Read, ..ConnectForm::new() };
    app.store.upsert(&project_id(&app), None, shared.to_profile()).unwrap();
    let log = log_path(dir.path(), &project_id(&app));
    for (session, at, name) in [("a", "09", "alpha"), ("b", "10", "billing"), ("c", "11", "cron")] {
        let at = format!("2026-10-13T{at}:00:00+03:00");
        let entry =
            Entry { session: session.into(), at, agent: "bot".into(), name: Some(name.into()), ..Entry::default() };
        append(&log, entry).unwrap();
    }
    app.open_agents();
    app.handle_agents_key(key(KeyCode::Char('/')));
    for c in "bill".chars() {
        app.handle_agents_key(key(KeyCode::Char(c)));
    }
    app.handle_agents_key(key(KeyCode::Enter));
    app.handle_agents_key(key(KeyCode::Enter));
    assert_eq!(app.agents.view.as_ref().unwrap().session.map(|i| app.agents.sessions[i].label()), Some("billing"));

    // Another agent call meanwhile: the open window isn't reloaded.
    let late = Entry { session: "d".into(), at: "2026-10-13T12:00:00+03:00".into(), ..Entry::default() };
    append(&log, late).unwrap();
    app.agents.checked = None;
    app.agents_tick();
    assert_eq!(app.agents.sessions.len(), 3);

    app.handle_agents_key(key(KeyCode::Esc));
    let view = app.agents.view.as_ref().unwrap();
    assert_eq!(view.filter.text(), "bill");
    let rows = app.agents_rows();
    assert_eq!(app.agents.sessions[rows[view.selected]].label(), "billing");
}

#[tokio::test]
async fn a_question_puts_the_models_sql_in_the_editor_without_running_it() {
    let answer = "```sql\nSELECT c.country, count(*) AS orders FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.country\n```\nCounts orders per country.";
    let (url, _server) =
        crate::gateway::tests::fake_model(vec![(200, crate::gateway::tests::claude_answer(answer))]).await;
    let dir = tempfile::tempdir().unwrap();
    let config =
        format!("[[models]]\nname = \"near\"\napi = \"anthropic\"\nurl = \"{url}\"\nmodel = \"claude-opus-5-5\"\n");
    let mut app = asking_app(dir.path(), &config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.tab.editor.set_text("select 1;");
    app.run_action(Action::Ask);
    assert!(app.ask_prompt.is_some());
    type_text(&mut app, "orders per country");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.asking.is_some() && said(&app).starts_with("Asking…"));
    until_answered(&mut app).await;
    assert_eq!(
        app.tab.editor.text(),
        "select 1;\n\n-- orders per country\n-- Counts orders per country.\n\
         SELECT c.country, count(*) AS orders FROM orders o JOIN customers c ON c.id = o.customer_id GROUP BY c.country;"
    );
    assert!(said(&app).contains("nothing ran"), "{}", said(&app));
    assert!(!app.run.executing && app.tab.grid.results.is_none(), "nothing ran");
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn asking_needs_a_saved_connection() {
    let (mut app, _dir) = test_app();
    app.screen = Screen::Query;
    app.run_action(Action::Ask);
    assert!(app.ask_prompt.is_none());
    assert!(said_error(&app), "{}", said(&app));
}

#[tokio::test]
async fn the_memory_window_adds_confirms_and_forgets_facts() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(dir.path(), "").await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    // A model's guess, waiting.
    let mut conn = crate::gateway::memory::open(dir.path(), &app.project().unwrap().id).await.unwrap();
    crate::gateway::memory::add(&mut conn, "orders.total", "means", "VAT included", "model:near", false).await.unwrap();
    drop(conn);
    app.run_action(Action::Memory);
    until_memory_loaded(&mut app, 1).await;
    assert_eq!(app.memory_view.as_ref().unwrap().facts[0]["confirmed"], serde_json::json!(false));
    // `a`: as `podelis memory add`.
    app.handle_key(key(KeyCode::Char('a')));
    type_text(&mut app, "revenue is orders.total minus refunds");
    app.handle_key(key(KeyCode::Enter));
    until_memory_loaded(&mut app, 2).await;
    let added = &app.memory_view.as_ref().unwrap().facts[1];
    assert_eq!((added["relation"].as_str(), added["value"].as_str()), (Some("is"), Some("orders.total minus refunds")));
    // `c` on the guess confirms it.
    app.handle_key(key(KeyCode::Char('c')));
    for _ in 0..50 {
        if app.memory_view.as_ref().unwrap().facts[0]["confirmed"] == serde_json::json!(true) {
            break;
        }
        let event = tokio::time::timeout(Duration::from_secs(10), app.poll_db_event()).await.unwrap().unwrap();
        app.handle_db_event(event);
    }
    assert_eq!(app.memory_view.as_ref().unwrap().facts[0]["confirmed"], serde_json::json!(true));
    // `d` forgets the selected one.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char('d')));
    until_memory_loaded(&mut app, 1).await;
    app.handle_key(key(KeyCode::Esc));
    assert!(app.memory_view.is_none());
}

#[tokio::test]
async fn allowing_the_schema_asks_again_about_the_tab_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.handle_key(key(KeyCode::Enter));
    // Another tab meanwhile, not connected.
    app.new_tab(None, "", None);
    app.switch_conn(None);
    assert_eq!(app.active_tab, 1);
    until_answered(&mut app).await;
    assert!(app.confirm.is_some());
    settle(&mut app);
    app.handle_key(key(KeyCode::Char('y')));
    let asking = app.asking.as_ref().expect("asked again");
    assert_eq!((asking.question.tab, asking.question.connection.as_str()), (0, "shop"));
    // Leaving the project stops it: no answer for a tab of another one.
    app.leave_project();
    assert!(app.asking.is_none());
}

#[tokio::test]
async fn the_sharing_question_waits_for_a_confirmation_already_open() {
    let dir = tempfile::tempdir().unwrap();
    let config = "[[models]]\nname = \"far\"\napi = \"openai\"\nurl = \"https://models.invalid/v1\"\nmodel = \"x\"\nkey_command = \"echo sk-x\"\n";
    let mut app = asking_app(dir.path(), config).await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers by country");
    app.handle_key(key(KeyCode::Enter));
    // Meanwhile, the user asks to close the tab (it asks first).
    app.confirm = Some(Confirm {
        message: "Close?".into(),
        details: Vec::new(),
        danger: false,
        action: ConfirmAction::CloseTab,
        offer_transaction: false,
    });
    until_answered(&mut app).await;
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::CloseTab), "not replaced");
    // Answered: then the sharing question shows, at once.
    app.handle_key(key(KeyCode::Char('n')));
    assert!(matches!(app.confirm.as_ref().unwrap().action, ConfirmAction::ShareSchema { .. }));
}

#[tokio::test]
async fn esc_while_asking_does_its_own_job_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(
        dir.path(),
        "[[models]]\nname = \"m\"\napi = \"openai\"\nurl = \"http://127.0.0.1:9/v1\"\nmodel = \"x\"\n",
    )
    .await;
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.asking.is_some());
    // A selection in the editor: Esc drops it, the question goes on.
    app.tab.editor.set_text("select 1");
    app.tab.editor.select_all();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.asking.is_some() && app.tab.editor.selection().is_none());
    // Nothing else to do: Esc stops asking, and says so.
    app.handle_key(key(KeyCode::Esc));
    assert!(app.asking.is_none());
    assert_eq!(said(&app), "Stopped asking");
}

#[test]
fn a_late_memory_answer_for_another_project_is_dropped() {
    let (mut app, _dir) = test_app();
    let shown = app.current_project.clone().unwrap();
    app.memory_view = Some(crate::app::memory_view::MemoryView { project_id: shown, ..Default::default() });
    let facts = serde_json::json!({ "facts": [{ "id": 1, "subject": "x" }] });
    let other = crate::app::memory_view::GatewayPurpose::MemoryList("another-project".into());
    app.gateway_done(other, Ok(facts.clone()));
    assert!(app.memory_view.as_ref().unwrap().facts.is_empty());
    let mine = crate::app::memory_view::GatewayPurpose::MemoryList(app.current_project.clone().unwrap());
    app.gateway_done(mine, Ok(facts));
    assert_eq!(app.memory_view.as_ref().unwrap().facts.len(), 1);
}

#[test]
fn deleting_a_project_deletes_its_memory() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = ConnectionStore::load_from(dir.path().join("c.toml"));
    let id = store.add_project("gone").unwrap();
    store.add_project("kept").unwrap();
    let mut app = App::new(store, None, Some(dir.path().to_path_buf()));
    let memory = crate::gateway::memory::path(dir.path(), &id);
    std::fs::create_dir_all(memory.parent().unwrap()).unwrap();
    std::fs::write(&memory, "facts").unwrap();
    app.manager.project_selected = app.store.projects.iter().position(|p| p.id == id).unwrap();
    app.delete_selected_project();
    assert!(!memory.exists());
}

#[tokio::test]
async fn closing_another_tab_while_asking_keeps_the_question() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = asking_app(
        dir.path(),
        "[[models]]\nname = \"m\"\napi = \"openai\"\nurl = \"http://127.0.0.1:9/v1\"\nmodel = \"x\"\n",
    )
    .await;
    app.new_tab(None, "", None);
    app.new_tab(None, "", None);
    app.switch_tab(1);
    app.connection.form.name = "shop".into();
    let _gateway = crate::gateway::tests::start(dir.path(), None);
    app.run_action(Action::Ask);
    type_text(&mut app, "customers");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.asking.as_ref().unwrap().question.tab, 1);
    // Tab 0 closes: the question follows its tab to place 0.
    app.switch_tab(0);
    app.close_tab();
    assert_eq!(app.asking.as_ref().map(|a| a.question.tab), Some(0));
    // Its own tab closes: it stops.
    app.switch_tab(0);
    app.close_tab();
    assert!(app.asking.is_none());
}
