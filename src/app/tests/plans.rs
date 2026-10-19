use super::*;

#[tokio::test]
async fn f7_shows_the_plan_without_running() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (id integer primary key, v text); insert into t values (1, 'a')").await;
    app.tab.editor.set_text("delete from t where id = 1");
    app.handle_key(key(KeyCode::F(7)));
    assert!(app.confirm.is_none(), "EXPLAIN of a DELETE runs nothing, so no confirmation");
    finish(&mut app).await;
    // The plan goes to the QUERY PLAN tab; the row is still there.
    assert_eq!(app.tab.result_tab, ResultTab::Plan);
    let plan = app.tab.plan.as_ref().map(|p| p.text.as_str()).expect("plan");
    assert!(plan.contains("SEARCH t"), "{plan}");
    run_sql(&mut app, "select count(*) from t").await;
    assert_eq!(app.tab.result_tab, ResultTab::Rows, "a normal run switches back to the rows");
    assert_eq!(app.tab.grid.results.as_ref().unwrap().rows, vec![vec![Some("1".to_string())]]);
}

#[tokio::test]
async fn f7_during_a_slow_query_shows_the_plan_at_once() {
    for engine in Engine::BUILT {
        let Some(sleep) = crate::db::tests::test_sql(engine).sleep else { continue };
        let Some((mut app, _dir)) = live_app(engine, "podelis_f7").await else { continue };
        finish_catalog(&mut app).await;
        app.tab.editor.set_text(&sleep.replace("{s}", "2"));
        app.handle_key(key(KeyCode::F(5)));
        app.tab.editor.set_text("select 1");
        app.handle_key(key(KeyCode::F(7)));
        // The plan arrives while the query still runs.
        while app.tab.plan.is_none() {
            let event = app.poll_db_event().await.unwrap();
            app.handle_db_event(event);
        }
        assert!(app.run.executing && app.tab.result_tab == ResultTab::Plan, "{engine:?}");
        assert!(app.queued_plan.is_none(), "{engine:?}");
        // The query's result doesn't take the plan out of view.
        finish(&mut app).await;
        assert_eq!(app.tab.result_tab, ResultTab::Plan, "{engine:?}");
        assert_eq!(app.tab.grid.results.as_ref().unwrap().rows.len(), 1, "{engine:?}: the query's own result");
    }
}

#[tokio::test]
async fn explain_moves_focus_to_the_plan() {
    let (mut app, _dir) = connected_app(ConnectionMode::Normal).await;
    run_sql(&mut app, "create table t (x int)").await;
    app.tab.editor.set_text("select * from t");
    app.focus = Focus::Editor;
    app.handle_key(key(KeyCode::F(7)));
    finish(&mut app).await;
    assert_eq!((app.focus, app.tab.result_tab), (Focus::Results, ResultTab::Plan));
    app.handle_key(key(KeyCode::Char('1')));
    assert_eq!(app.tab.result_tab, ResultTab::Rows);
}

#[test]
fn a_plan_remembers_its_statement_and_time() {
    let (mut app, _dir) = test_app();
    app.plan_request = Some(("select * from users where id = 1".into(), true, 0));
    app.show_plan(outcome(&["QUERY PLAN"], &[&[Some("[]")]]));
    let source = app.tab.plan_source.clone().unwrap();
    assert_eq!((source.sql.as_str(), source.analyzed), ("select * from users where id = 1", true));
    assert!((chrono::Local::now() - source.at).num_seconds() < 5);
    assert!(app.plan_request.is_none());
}

#[test]
fn the_statement_to_explain_and_its_line() {
    let (mut app, _dir) = test_app();
    app.tab.editor.set_text("select 1;\n\n  select *\n  from t;\nselect 3");
    app.tab.editor.set_cursor((3, 3));
    assert_eq!(app.statement_to_explain(), Some(("select *\n  from t".to_string(), 2)));
    let source = PlanSource::new("select 3".into(), false, 4);
    assert_eq!(source.find_in("select 3\nx\nx\nx\nselect 3\nselect 3"), Some((15, 4)), "the copy nearest line 5");
    assert_eq!(source.find_in("select 4"), None);
}
