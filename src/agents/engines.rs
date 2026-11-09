//! Each engine's own statements under the agents' rules, in its syntax
//! (what's refused there and why, how what runs is wrapped). The rules
//! every engine shares are `tests` in `mod.rs`; what each engine refuses is
//! its `db/<engine>/agent_constraints.rs`.

use crate::agents::{Verdict, Wrap, verdict};
use crate::db::{AgentAccess, ConnectionMode, Engine};

fn verdict_on(engine: Engine, sql: &str) -> Verdict {
    verdict(AgentAccess::Write, ConnectionMode::Normal, sql, engine)
}

/// Why it's refused.
fn refused(engine: Engine, sql: &str) -> String {
    match verdict_on(engine, sql) {
        Verdict::Refuse(said) => said,
        Verdict::Run(wrap) => panic!("{engine:?} ran {sql}: {wrap:?}"),
    }
}

/// How it runs.
fn run(engine: Engine, sql: &str) -> Wrap {
    match verdict_on(engine, sql) {
        Verdict::Run(wrap) => wrap,
        Verdict::Refuse(said) => panic!("{engine:?} refused {sql}: {said}"),
    }
}

mod postgres {
    use super::*;

    const ENGINE: Engine = Engine::Postgres;

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("do $$ begin delete from t; end $$", "not DO"),
            ("copy t to program 'rm -rf /'", "not COPY"),
            ("select pg_terminate_backend(pid) from pg_stat_activity", "stops other sessions"),
            ("select lo_export(1, '/tmp/x')", "reaches the server's files"),
            ("select pg_advisory_lock(1)", "holds a lock past the transaction"),
            ("SET default_transaction_read_only = off", "transaction of its own"),
            ("select set_config('standard_conforming_strings', 'off', false)", "changes the session's settings"),
            ("BEGIN READ WRITE; insert into t values (1); COMMIT", "transaction of its own"),
            // MySQL's and SQLite's, not PostgreSQL's.
            ("replace into t values (1)", "schema changes on PostgreSQL"),
        ] {
            assert!(refused(ENGINE, sql).contains(why), "{sql}: {}", refused(ENGINE, sql));
        }
        assert_eq!(run(ENGINE, "merge into t using u on t.id = u.id when matched then update set x = 1"), Wrap::Commit);
        assert_eq!(run(ENGINE, "table t"), Wrap::Commit);
    }
}

mod mysql {
    use super::*;

    const ENGINE: Engine = Engine::MySql;

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("select * from users into outfile '/tmp/x'", "writes a file on the server"),
            ("select 1 into dumpfile '/tmp/x'", "writes a file on the server"),
            ("select load_file('/etc/passwd')", "reads a file on the server"),
            ("select get_lock('x', 10)", "holds a lock past the transaction"),
            ("set autocommit = 1", "not SET: Podėlis runs each call in a transaction of its own"),
            ("lock tables t write", "transaction of its own"),
            // MySQL runs an executable comment's code: what the words don't
            // see (found 2026-11-01: a read-only agent wrote a file this way).
            ("select 1 /*! into outfile '/tmp/x' */", "executable comment"),
            ("select 1 /*!80000 for update */", "executable comment"),
            ("select 1 /*M! , load_file('/etc/passwd') */", "executable comment"),
            // With NO_BACKSLASH_ESCAPES the server ends the string at `\'`.
            ("select 'a\\' , load_file('/etc/passwd') -- '", "backslash before a quote"),
            ("select \"a\\\" , load_file('/etc/passwd') -- \"", "backslash before a quote"),
        ] {
            assert!(refused(ENGINE, sql).contains(why), "{sql}: {}", refused(ENGINE, sql));
        }
        assert_eq!(run(ENGINE, "replace into t values (1)"), Wrap::Commit);
        assert_eq!(run(ENGINE, "describe t"), Wrap::Commit);
        // Plain comments and optimizer hints are fine.
        assert_eq!(run(ENGINE, "select /*+ MAX_EXECUTION_TIME(1000) */ 1 /* why */ -- note"), Wrap::Commit);
    }
}

mod sqlite {
    use super::*;

    const ENGINE: Engine = Engine::Sqlite;

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("select load_extension('x')", "loads code into the database"),
            ("attach 'other.db' as o", "not ATTACH: a connection is one database file"),
            ("pragma writable_schema = 1", "not PRAGMA: describe_table tells"),
        ] {
            assert!(refused(ENGINE, sql).contains(why), "{sql}: {}", refused(ENGINE, sql));
        }
        assert_eq!(run(ENGINE, "replace into t values (1)"), Wrap::Commit);
        assert_eq!(run(ENGINE, "values (1), (2)"), Wrap::Commit);
    }
}

mod duckdb {
    use super::*;

    const ENGINE: Engine = Engine::DuckDb;

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("select * from read_csv('/etc/passwd')", "reads files on this machine"),
            ("copy t to '/tmp/x.csv'", "not COPY"),
            ("install httpfs", "not INSTALL"),
            ("attach 'other.duckdb' as o", "a connection is one database file"),
            ("pragma database_list", "describe_table tells"),
            ("set enable_external_access = true", "transaction of its own"),
            ("select * from glob('*')", "lists files on this machine"),
        ] {
            assert!(refused(ENGINE, sql).contains(why), "{sql}: {}", refused(ENGINE, sql));
        }
        assert_eq!(run(ENGINE, "from t"), Wrap::Commit);
        assert_eq!(run(ENGINE, "summarize t"), Wrap::Commit);
    }
}

mod mongodb {
    use super::*;

    const ENGINE: Engine = Engine::MongoDb;

    #[test]
    fn its_own_statements() {
        for (sql, why) in [
            ("db.users.deleteMany({})", "destructive"),
            ("db.users.updateMany({}, {$set: {x: 1}})", "destructive"),
            // Empty, whatever comments are in it (found in review, 2026-11-09).
            ("db.users.deleteMany({/**/})", "destructive"),
            ("db.users.deleteMany({ // all\n })", "destructive"),
            ("db.users.updateMany({} /*x*/, {$set: {x: 1}})", "destructive"),
            ("db.users.aggregate([{/**/$out: 'copy'}])", "writes the result into a collection"),
            ("db.users.drop()", "not drop()"),
            ("db.users.aggregate([{$match: {}}, {$out: 'copy'}])", "writes the result into a collection"),
            ("db.users.aggregate([{$merge: {into: 'copy'}}])", "writes the result into a collection"),
            ("db.users.find({$where: 'sleep(100)'})", "runs JavaScript on the server"),
            // A key in quotes, or escaped there, is the same key to the server.
            ("db.users.find({\"$where\": 'sleep(100)'})", "runs JavaScript on the server"),
            ("db.users.find({'\\u0024where': 'sleep(100)'})", "runs JavaScript on the server"),
            ("db.users.aggregate([{$group: {_id: 1, x: {$accumulator: {}}}}])", "runs JavaScript on the server"),
            ("db.runCommand({dropDatabase: 1})", "the schema tool tells"),
            ("db.users.createIndex({x: 1})", "not createIndex()"),
            ("db.startTransaction()", "not startTransaction()"),
            ("db.users.bulkWrite([])", "not bulkWrite()"),
            ("select * from users", "not a call Podėlis knows: agents can call find, findOne"),
        ] {
            assert!(refused(ENGINE, sql).contains(why), "{sql}: {}", refused(ENGINE, sql));
        }
        assert_eq!(run(ENGINE, "db.users.insertOne({name: 'Ada'})"), Wrap::Commit);
        assert_eq!(run(ENGINE, "db.getCollection('my orders').countDocuments({})"), Wrap::Commit);
        assert_eq!(run(ENGINE, "db.users.aggregate([{$group: {_id: '$city', n: {$sum: 1}}}])"), Wrap::Commit);
        assert_eq!(run(ENGINE, "db.users.deleteOne({_id: 1})"), Wrap::Commit);
    }
}
