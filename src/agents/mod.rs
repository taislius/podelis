//! What AI agents may do through `podelis mcp` (here), what they did (the
//! log), their writes waiting for the user (approvals), and the MCP server
//! itself.

mod approvals;
mod log;
pub(crate) mod mcp;

pub use approvals::*;
pub use log::*;

use crate::db::{AgentAccess, ConnectionMode, Engine};
use crate::sql::analyze::Kind;

/// What happens to an agent's SQL. It always runs in a transaction of
/// Podėlis's own, which the agent can't end (it may not send BEGIN,
/// COMMIT or SET): that is what holds, since a session's read-only setting
/// is one the SQL could otherwise switch off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    /// In a read-only transaction, rolled back: writes fail on the server,
    /// even one hidden in a function, and settings changed go back.
    ReadOnly,
    /// In a transaction that's rolled back, saying what it would change.
    DryRun,
    /// In a transaction that's committed if it all ran.
    Commit,
    /// Shown to the user with its dry run, then committed if they approve.
    Ask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Run(Wrap),
    Refuse(String),
}

/// Reads run wherever an agent may look, in a read-only transaction.
/// Writes need write access and a connection that isn't read-only; nothing
/// destructive (DROP, TRUNCATE, a change without WHERE) ever runs; and on
/// production every call is rolled back (a write is only a dry run, and
/// schema changes not even that: MySQL commits them at once). With
/// [`AgentAccess::Approve`] a write waits for the user's yes instead.
pub fn verdict(access: AgentAccess, mode: ConnectionMode, sql: &str, engine: Engine) -> Verdict {
    let infos = match allowed(sql, engine) {
        Ok(infos) => infos,
        Err(why) => return Verdict::Refuse(why),
    };
    let write = infos.iter().find(|i| i.modifies());
    let may_write = matches!(access, AgentAccess::Write | AgentAccess::Approve) && mode != ConnectionMode::ReadOnly;
    if let Some(write) = write
        && !may_write
    {
        return Verdict::Refuse(format!(
            "“{}” changes data, and this connection is shared with agents to read only",
            write.summary
        ));
    }
    if let Some(risky) = infos.iter().find(|i| i.risk.is_some()) {
        return Verdict::Refuse(format!(
            "“{}” is destructive ({}): agents can't run it; ask the user",
            risky.summary,
            risky.risk.unwrap_or_default()
        ));
    }
    if access == AgentAccess::Approve {
        return Verdict::Run(if write.is_some() { Wrap::Ask } else { Wrap::ReadOnly });
    }
    if mode == ConnectionMode::Production {
        if let Some(ddl) = infos.iter().find(|i| i.kind == Kind::Ddl) {
            return Verdict::Refuse(format!(
                "“{}” changes the schema of a production database: agents can't, not even as a dry run",
                ddl.summary
            ));
        }
        return Verdict::Run(if write.is_some() { Wrap::DryRun } else { Wrap::ReadOnly });
    }
    Verdict::Run(if may_write { Wrap::Commit } else { Wrap::ReadOnly })
}

/// Whether `sql` can be planned for an agent: one statement (so nothing can
/// ride along after it), of a kind it may run.
pub fn explainable(sql: &str, engine: Engine) -> Result<(), String> {
    if crate::sql::split_statements(sql, engine).len() != 1 {
        return Err("`explain` plans one statement at a time".to_string());
    }
    allowed(sql, engine).map(drop)
}

/// The statements of `sql` if an agent may send them at all (whether it may
/// write is up to [`verdict`]), by the engine's own constraints
/// (`db::agent_constraints`): each starts with a statement agents may run,
/// none uses anything that reaches past the data, and none changes the session or its
/// transaction (not SET, BEGIN, COMMIT, USE, CALL, DO, COPY, LOCK, GRANT…:
/// they change the session, end Podėlis's transaction, or reach outside).
fn allowed(sql: &str, engine: Engine) -> Result<Vec<crate::sql::analyze::StatementInfo>, String> {
    let rules = engine.dialect().agent_constraints();
    let statements = crate::sql::split_statements(sql, engine);
    if statements.is_empty() {
        return Err("there's no SQL to run".to_string());
    }
    for statement in &statements {
        let words: Vec<String> = crate::sql::tokenize(statement, engine)
            .into_iter()
            .filter(|t| t.kind == crate::sql::TokenKind::Word)
            .map(|t| statement[t.range].to_ascii_lowercase())
            .collect();
        let first = words.first().map_or("", String::as_str);
        if !rules.may_start_with(first) {
            let why = rules.why_not(first, engine.label());
            return Err(format!("“{}”: not {}: {why}", one_line(statement), first.to_uppercase()));
        }
        if let Some((word, does)) = rules.reaching_outside(&words) {
            return Err(format!("“{}” uses {}, which {does}: agents can't", one_line(statement), word.to_uppercase()));
        }
    }
    let infos = crate::sql::analyze::analyze(&crate::sql::parse::parse(sql, engine));
    if let Some(other) = infos.iter().find(|i| matches!(i.kind, Kind::Transaction(_) | Kind::Session)) {
        return Err(format!(
            "“{}”: agents can't change the session or its transaction; Podėlis runs each call in a transaction of its own",
            other.summary
        ));
    }
    Ok(infos)
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 60 { format!("{}…", s.chars().take(60).collect::<String>()) } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Engine;

    /// The rules for every engine, in SQL any of them takes; each engine's
    /// own statements are tested in its folder (`agent_constraints.rs`).
    #[test]
    fn what_agents_may_run() {
        use AgentAccess::{Approve, Read, Write};
        use ConnectionMode::{Normal, Production, ReadOnly};
        for engine in Engine::ALL {
            let v = |access, mode, sql| verdict(access, mode, sql, engine);
            let refused = |access, mode, sql| matches!(v(access, mode, sql), Verdict::Refuse(_));
            assert_eq!(v(Read, Normal, "select * from t"), Verdict::Run(Wrap::ReadOnly), "{engine:?}");
            assert_eq!(v(Read, Production, "select * from t"), Verdict::Run(Wrap::ReadOnly), "{engine:?}");
            assert!(refused(Read, Normal, "update t set x = 1 where id = 2"), "{engine:?}");
            assert!(refused(Write, ReadOnly, "insert into t values (1)"), "{engine:?}");
            assert_eq!(v(Write, Normal, "update t set x = 1 where id = 2"), Verdict::Run(Wrap::Commit), "{engine:?}");
            assert_eq!(v(Write, Normal, "create table u (id int)"), Verdict::Run(Wrap::Commit), "{engine:?}");
            assert_eq!(
                v(Write, Production, "update t set x = 1 where id = 2"),
                Verdict::Run(Wrap::DryRun),
                "{engine:?}"
            );
            // Reads on production are rolled back too: a function may write.
            assert_eq!(v(Write, Production, "select f(1)"), Verdict::Run(Wrap::ReadOnly), "{engine:?}");
            for destructive in ["delete from t", "drop table t", "truncate t", "update t set x = 1"] {
                assert!(refused(Write, Normal, destructive), "{engine:?} {destructive}");
            }
            assert!(refused(Write, Production, "alter table t add column y int"), "{engine:?}");
            // Approved by the user: asked for each write, production included;
            // still never anything destructive.
            assert_eq!(v(Approve, Production, "update t set x = 1 where id = 2"), Verdict::Run(Wrap::Ask));
            assert_eq!(v(Approve, Production, "alter table t add column y int"), Verdict::Run(Wrap::Ask));
            assert_eq!(v(Approve, Normal, "select * from t"), Verdict::Run(Wrap::ReadOnly));
            assert!(refused(Approve, Production, "delete from t"), "{engine:?}");
            assert!(refused(Approve, ReadOnly, "insert into t values (1)"), "{engine:?}");
            // Nothing that changes the session or ends Podėlis's transaction.
            for sql in [
                "set x = 1",
                "begin; insert into t values (1); commit",
                "update t set x = 1 where id = 2; commit",
                "select 1; set x = 1",
                "use other_db",
                "call purge()",
                "lock table t",
                "grant all on t to public",
                "",
            ] {
                for access in [Read, Write, Approve] {
                    assert!(refused(access, Normal, sql), "{engine:?} {sql}");
                }
            }
        }
    }

    /// Each engine's constraints, as it gives them: what reaches past the
    /// data is refused saying what it does, a better way is named, and
    /// another engine's words are names like any other.
    #[test]
    fn each_engine_by_its_own_constraints() {
        for engine in Engine::ALL {
            let rules = engine.dialect().agent_constraints();
            let said = |sql: &str| match verdict(AgentAccess::Write, ConnectionMode::Normal, sql, engine) {
                Verdict::Refuse(said) => said,
                Verdict::Run(wrap) => panic!("{engine:?} ran {sql}: {wrap:?}"),
            };
            for (word, does) in rules.outside {
                assert!(said(&format!("select {word}(1)")).contains(does), "{engine:?} {word}");
            }
            for (word, way) in rules.instead {
                assert!(said(&format!("{word} x")).contains(way), "{engine:?} {word}");
            }
            let theirs =
                Engine::ALL.iter().flat_map(|e| e.dialect().agent_constraints().outside.iter().map(|(w, _)| *w));
            for word in theirs.filter(|w| !rules.outside.iter().any(|(o, _)| o == w)) {
                let sql = format!("select {word} from t");
                let ran = verdict(AgentAccess::Write, ConnectionMode::Normal, &sql, engine);
                assert_eq!(ran, Verdict::Run(Wrap::Commit), "{engine:?} {sql}");
            }
        }
    }

    #[test]
    fn what_agents_may_explain() {
        for engine in Engine::ALL {
            let e = |sql: &str| explainable(sql, engine);
            assert!(e("select * from t where id = 1").is_ok(), "{engine:?}");
            assert!(e("delete from t").is_ok(), "planning it doesn't run it");
            assert!(e("select 1; delete from t").is_err(), "{engine:?}");
            // A message quoting the agent's own SQL can't open the door.
            assert!(e("set x = 'destructive'").is_err(), "{engine:?}");
            let outside = engine.dialect().agent_constraints().outside;
            assert!(outside.iter().all(|(word, _)| e(&format!("select {word}(1)")).is_err()), "{engine:?}");
        }
    }
}
