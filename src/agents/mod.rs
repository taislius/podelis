//! What AI agents may do through `podelis mcp` (here), what they did (the
//! log), their writes waiting for the user (approvals), and the MCP server
//! itself.

mod approvals;
#[cfg(test)]
mod engines;
mod log;
pub(crate) mod mcp;

pub use approvals::*;
pub use log::*;

use crate::db::{AgentAccess, ConnectionMode, Engine};
use crate::language::TokenKind;
use crate::language::statements::Kind;

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
    /// In the connection's sandbox: its transaction (begun by the first
    /// call), after a savepoint of its own; never committed.
    Sandbox,
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
    let may_write = matches!(access, AgentAccess::Write | AgentAccess::Approve | AgentAccess::Sandbox)
        && mode != ConnectionMode::ReadOnly;
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
    // Without SQL a write isn't in a transaction of Podėlis's (a dry run's
    // aside): each statement stands as it runs, so one a call, not a
    // script that could stop half-way.
    if engine.dialect().is_none() && write.is_some() && infos.len() > 1 {
        return Verdict::Refuse(format!(
            "one statement a call when it writes on {}: each stands as it runs, so a script could stop half-way",
            engine.label()
        ));
    }
    if access == AgentAccess::Approve {
        return Verdict::Run(if write.is_some() { Wrap::Ask } else { Wrap::ReadOnly });
    }
    // In a sandbox everything runs in its transaction (reads see its
    // writes); nothing is committed, so production too. Schema changes
    // only where they can be rolled back.
    if access == AgentAccess::Sandbox && may_write {
        // Its calls each after a savepoint, in one transaction left open.
        if engine.dialect().is_none() {
            return Verdict::Refuse(format!(
                "a sandbox needs savepoints and a transaction kept open between calls, which {} doesn't have: \
                 ask the user to share this connection to read and write, or to approve writes",
                engine.label()
            ));
        }
        if engine.capabilities().ddl_commits
            && let Some(ddl) = infos.iter().find(|i| i.kind == Kind::Ddl)
        {
            return Verdict::Refuse(format!(
                "“{}” changes the schema, which {} commits at once: not in a sandbox",
                ddl.summary,
                engine.label()
            ));
        }
        return Verdict::Run(Wrap::Sandbox);
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
    if crate::language::split_statements(sql, engine).len() != 1 {
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
fn allowed(sql: &str, engine: Engine) -> Result<Vec<crate::language::statements::StatementInfo>, String> {
    let rules = engine.backend().agent_constraints();
    // MySQL runs what's in `/*! … */` (MariaDB `/*M! … */` too): code the
    // checks below wouldn't see (`/*! INTO OUTFILE '…' */`). The whole text,
    // as the server gets it: a statement's comments around it aren't in
    // what `split_statements` gives back.
    if engine == Engine::MySql
        && let Some(t) = crate::language::tokenize(sql, engine).into_iter().find(|t| {
            t.kind == crate::language::TokenKind::Comment && {
                let c = &sql[t.range.clone()];
                c.starts_with("/*!") || c.starts_with("/*M!") || c.starts_with("/*m!")
            }
        })
    {
        return Err(format!(
            "“{}” is an executable comment, which MySQL runs as SQL: agents can't send them",
            one_line(&sql[t.range])
        ));
    }
    // Where a string ends after `\'` depends on the server's sql_mode
    // (NO_BACKSLASH_ESCAPES, ANSI_QUOTES), which the checks here don't
    // know: code could hide in what they'd take for a string.
    if engine == Engine::MySql && (sql.contains("\\'") || sql.contains("\\\"")) {
        return Err("a backslash before a quote (\\' or \\\"): its meaning depends on the server's sql_mode; \
             write a quote inside a string doubled ('') instead"
            .to_string());
    }
    let statements = crate::language::split_statements(sql, engine);
    if statements.is_empty() {
        return Err(if engine.dialect().is_some() { "there's no SQL to run" } else { "there's nothing to run" }.into());
    }
    for statement in &statements {
        let words = words(statement, engine);
        let verb = crate::language::verb(engine, statement).unwrap_or_default();
        if !rules.may_start_with(&verb) {
            let why = rules.why_not(&verb, engine.label());
            return Err(match (engine.dialect(), verb.as_str()) {
                (Some(_), _) => format!("“{}”: not {}: {why}", one_line(statement), verb.to_uppercase()),
                (None, "") => format!("“{}”: not a call Podėlis knows: {why}", one_line(statement)),
                (None, _) => format!("“{}”: not {verb}(): {why}", one_line(statement)),
            });
        }
        if let Some((word, does)) = rules.reaching_outside(&words) {
            return Err(format!("“{}” uses {}, which {does}: agents can't", one_line(statement), word.to_uppercase()));
        }
    }
    let infos = crate::language::statements::analyze(&crate::sql::parse::parse(sql, engine));
    if let Some(other) = infos.iter().find(|i| matches!(i.kind, Kind::Transaction(_) | Kind::Session)) {
        return Err(format!(
            "“{}”: agents can't change the session or its transaction; Podėlis runs each call in a transaction of its own",
            other.summary
        ));
    }
    Ok(infos)
}

/// Why `sql` is refused on a database `service` runs (Supabase's): it names
/// a schema agents are kept out of there (`vault.secrets`, `"auth"."users"`).
pub fn kept_out(sql: &str, engine: Engine, service: Option<&dyn crate::db::services::Service>) -> Option<String> {
    use crate::language::TokenKind;
    let service = service?;
    let kept = service.agents_kept_out();
    let tokens: Vec<_> = crate::language::tokenize(sql, engine)
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Comment))
        .collect();
    tokens.windows(2).find_map(|pair| {
        let name = match pair[0].kind {
            TokenKind::Word => sql[pair[0].range.clone()].to_ascii_lowercase(),
            TokenKind::QuotedIdent => sql[pair[0].range.clone()].trim_matches(['"', '`']).replace("\"\"", "\""),
            _ => return None,
        };
        let qualifies = pair[1].kind == TokenKind::Punct && &sql[pair[1].range.clone()] == ".";
        let (schema, holds) = kept.iter().find(|(schema, _)| qualifies && *schema == name)?;
        Some(format!("{}'s {schema} schema {holds}: agents can't use it", service.name()))
    })
}

/// A statement's words, lower case where SQL's, to find what reaches past
/// the data. Without SQL a key may be in quotes (`{"$where": …}`), escaped
/// too (`"\u0024where"`): strings are words there, read as the server would.
fn words(statement: &str, engine: Engine) -> Vec<String> {
    let sql = engine.dialect().is_some();
    crate::language::tokenize(statement, engine)
        .into_iter()
        .filter_map(|t| {
            let text = &statement[t.range];
            match t.kind {
                TokenKind::Word if sql => Some(text.to_ascii_lowercase()),
                TokenKind::Word => Some(text.to_string()),
                TokenKind::String if !sql => Some(unescaped(text.get(1..text.len().saturating_sub(1)).unwrap_or(""))),
                _ => None,
            }
        })
        .collect()
}

/// A string's text with its escapes undone (`\u0024` is `$`).
fn unescaped(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
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
    /// own statements are tested in `engines.rs`.
    #[test]
    fn what_agents_may_run() {
        use AgentAccess::{Approve, Read, Write};
        use ConnectionMode::{Normal, Production, ReadOnly};
        for engine in Engine::ALL.into_iter().filter(|e| e.dialect().is_some()) {
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
        for engine in Engine::ALL.into_iter().filter(|e| e.dialect().is_some()) {
            let rules = engine.backend().agent_constraints();
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
            let theirs = Engine::ALL
                .iter()
                .filter(|e| e.dialect().is_some())
                .flat_map(|e| e.backend().agent_constraints().outside.iter().map(|(w, _)| *w));
            for word in theirs.filter(|w| !rules.outside.iter().any(|(o, _)| o == w)) {
                let sql = format!("select {word} from t");
                let ran = verdict(AgentAccess::Write, ConnectionMode::Normal, &sql, engine);
                assert_eq!(ran, Verdict::Run(Wrap::Commit), "{engine:?} {sql}");
            }
        }
    }

    #[test]
    fn what_agents_may_explain() {
        for engine in Engine::ALL.into_iter().filter(|e| e.dialect().is_some()) {
            let e = |sql: &str| explainable(sql, engine);
            assert!(e("select * from t where id = 1").is_ok(), "{engine:?}");
            assert!(e("delete from t").is_ok(), "planning it doesn't run it");
            assert!(e("select 1; delete from t").is_err(), "{engine:?}");
            // A message quoting the agent's own SQL can't open the door.
            assert!(e("set x = 'destructive'").is_err(), "{engine:?}");
            let outside = engine.backend().agent_constraints().outside;
            assert!(outside.iter().all(|(word, _)| e(&format!("select {word}(1)")).is_err()), "{engine:?}");
        }
    }

    /// A sandbox on an engine that commits schema changes at once (MySQL)
    /// runs writes in its transaction but refuses the schema change, by
    /// name, even beside a write.
    #[test]
    fn a_sandbox_refuses_only_schema_changes_that_commit() {
        let engines: Vec<Engine> =
            Engine::ALL.into_iter().filter(|e| e.capabilities().ddl_commits && e.dialect().is_some()).collect();
        assert!(!engines.is_empty(), "an engine whose DDL commits (MySQL)");
        for engine in engines {
            let v = |sql| verdict(AgentAccess::Sandbox, ConnectionMode::Normal, sql, engine);
            assert_eq!(v("insert into t values (1)"), Verdict::Run(Wrap::Sandbox), "{engine:?}");
            match v("insert into t values (1); create table u (id int)") {
                Verdict::Refuse(said) => {
                    assert!(said.contains("changes the schema"), "{engine:?}: {said}");
                    assert!(said.to_lowercase().contains("create table"), "{engine:?}: names the CREATE: {said}");
                }
                Verdict::Run(wrap) => panic!("{engine:?}: ran a CREATE in a sandbox: {wrap:?}"),
            }
        }
    }

    /// A statement quoted in a refusal: one line, at most 60 characters
    /// and an ellipsis.
    #[test]
    fn quoted_statements_fit_on_a_line() {
        assert_eq!(one_line("select  *\n  from t"), "select * from t");
        let sixty = "x".repeat(60);
        assert_eq!(one_line(&sixty), sixty, "60 fit");
        assert_eq!(one_line(&format!("{sixty}y")), format!("{sixty}…"));
        assert_eq!(one_line(&"ą".repeat(61)), format!("{}…", "ą".repeat(60)), "characters, not bytes");
    }

    /// On Supabase its secrets, keys and sign-ins are out of agents' reach,
    /// however the name is written; elsewhere the same names are any others.
    #[test]
    fn supabase_keeps_agents_out_of_its_secrets() {
        let supabase = crate::db::services::serving(Engine::Postgres, "db.abcdefghijklmnop.supabase.co");
        let out = |sql: &str| kept_out(sql, Engine::Postgres, supabase);
        for sql in [
            "select * from vault.decrypted_secrets",
            "select email, encrypted_password from auth.users",
            "select * from \"auth\" . \"refresh_tokens\"",
            "select vault.create_secret('x')",
            "select * from public.t join AUTH.sessions s on true",
            "select * from pgsodium.key",
        ] {
            let why = out(sql).unwrap_or_else(|| panic!("ran {sql}"));
            assert!(why.contains("Supabase's") && why.contains("agents can't"), "{why}");
        }
        for sql in ["select auth from t", "select * from public.vault", "select 'vault.secrets'", "select * from t"] {
            assert_eq!(out(sql), None, "{sql}");
        }
        let elsewhere = crate::db::services::serving(Engine::Postgres, "db.example.com");
        assert_eq!(kept_out("select * from vault.secrets", Engine::Postgres, elsewhere), None);
    }

    /// On an engine whose language isn't SQL the same rules hold, by what
    /// its statements do: reads anywhere, writes where allowed (on
    /// production a dry run), one a call; never a sandbox, which needs
    /// savepoints.
    #[test]
    fn engines_without_sql_by_the_same_rules() {
        use AgentAccess::{Approve, Read, Sandbox, Write};
        use ConnectionMode::{Normal, Production, ReadOnly};
        for engine in Engine::ALL.into_iter().filter(|e| e.dialect().is_none()) {
            let v = |access, mode, sql| verdict(access, mode, sql, engine);
            let said = |access, mode, sql| match v(access, mode, sql) {
                Verdict::Refuse(said) => said,
                Verdict::Run(wrap) => panic!("{engine:?} ran {sql}: {wrap:?}"),
            };
            let (read, write) = ("db.users.find({age: {$gt: 30}})", "db.users.updateOne({_id: 1}, {$set: {x: 1}})");
            assert_eq!(v(Read, Normal, read), Verdict::Run(Wrap::ReadOnly));
            assert_eq!(v(Read, Production, read), Verdict::Run(Wrap::ReadOnly));
            assert!(said(Read, Normal, write).contains("read only"));
            assert!(said(Write, ReadOnly, write).contains("read only"));
            assert_eq!(v(Write, Normal, write), Verdict::Run(Wrap::Commit));
            assert_eq!(v(Write, Production, write), Verdict::Run(Wrap::DryRun));
            assert_eq!(v(Approve, Production, write), Verdict::Run(Wrap::Ask));
            let (writes, reads) = (format!("{write}; {write}"), format!("{read}; {read}"));
            assert!(said(Write, Normal, &writes).contains("one statement a call"));
            assert_eq!(v(Read, Normal, &reads), Verdict::Run(Wrap::ReadOnly), "reads, any number");
            assert!(said(Sandbox, Normal, write).contains("savepoints"));
            assert!(explainable(read, engine).is_ok());
            assert!(explainable(&format!("{read}; {read}"), engine).is_err());
        }
    }
}
