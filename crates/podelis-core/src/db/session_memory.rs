//! What the user's statements changed on the session, so a reconnect can
//! say what's gone with the old one. Nothing is run again: a new session
//! has the connection's own settings, and it's the user's to set them
//! again. Engine-free: which statements change a session is each engine's
//! own (`db/<engine>/session.rs`: `remember`).

use super::Engine;
use crate::language::TokenKind;

/// The session's changes, latest per setting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMemory {
    /// What was set, in the order last set: the current database first, if
    /// one was chosen.
    pub settings: Vec<Setting>,
    /// Temporary tables were made.
    pub temporary_tables: bool,
    /// Statements were prepared.
    pub prepared: bool,
}

/// One change to the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// What it sets (lowercase): a later change to the same replaces it.
    pub key: String,
    /// How it's said: `database shop`, `sql_mode`, `@id`, `role`.
    pub label: String,
}

/// At most this many settings are kept (the oldest go first; the database
/// stays).
const MAX_SETTINGS: usize = 64;

impl SessionMemory {
    /// `key` was set (said as `label`): kept instead of what set it before,
    /// last; the database first.
    pub fn set(&mut self, key: &str, label: &str) {
        let setting = Setting { key: key.to_string(), label: label.to_string() };
        self.settings.retain(|s| s.key != key);
        if key == DATABASE {
            self.settings.insert(0, setting);
        } else {
            self.settings.push(setting);
        }
        if self.settings.len() > MAX_SETTINGS {
            let oldest = usize::from(self.settings[0].key == DATABASE);
            self.settings.remove(oldest);
        }
    }

    /// `key` back to the connection's own (`RESET name`).
    pub fn reset(&mut self, key: &str) {
        self.settings.retain(|s| s.key != key);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty() && !self.temporary_tables && !self.prepared
    }

    /// What the session had that a new one hasn't, as it's said.
    pub fn lost(&self) -> Vec<String> {
        let mut lost: Vec<String> = self.settings.iter().map(|s| s.label.clone()).collect();
        if self.temporary_tables {
            lost.push("temporary tables".to_string());
        }
        if self.prepared {
            lost.push("prepared statements".to_string());
        }
        lost
    }
}

/// Of `statements` (one run's), the ones whose changes to the session
/// stand: where a `ROLLBACK` undoes them (`undone`), what's run in a
/// transaction stands only once it commits in the same run (none of what
/// came before a `ROLLBACK TO` a savepoint); left open (or
/// open from an earlier run, `open_at_start`), whether it stands isn't
/// known, and it's left out. Elsewhere, every one stands.
pub fn settled<'a>(statements: &[&'a str], engine: Engine, open_at_start: bool, undone: bool) -> Vec<&'a str> {
    if !undone {
        return statements.to_vec();
    }
    let mut kept = Vec::new();
    let mut pending = Vec::new();
    let mut in_transaction = open_at_start;
    for &statement in statements {
        match transaction_control(statement, engine) {
            Some(Control::Begin) => in_transaction = true,
            Some(Control::Commit) if in_transaction => {
                in_transaction = false;
                kept.append(&mut pending);
            }
            Some(Control::Rollback) if in_transaction => {
                in_transaction = false;
                pending.clear();
            }
            // Undoes what came after its savepoint, and which that was
            // isn't followed here: none of it's kept (the transaction goes
            // on).
            Some(Control::RollbackToSavepoint) if in_transaction => pending.clear(),
            _ if in_transaction => pending.push(statement),
            _ => kept.push(statement),
        }
    }
    kept
}

/// A statement that starts or ends a transaction, or rolls back to a
/// savepoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Begin,
    Commit,
    Rollback,
    RollbackToSavepoint,
}

/// Which transaction control `statement` is, if any: `ROLLBACK [WORK |
/// TRANSACTION] TO [SAVEPOINT] s` a savepoint's, not the transaction's end.
fn transaction_control(statement: &str, engine: Engine) -> Option<Control> {
    let words = leading_words(statement, engine, 3);
    let word = |i: usize| words.get(i).map_or("", String::as_str);
    match word(0) {
        "begin" | "start" => Some(Control::Begin),
        "commit" | "end" => Some(Control::Commit),
        "rollback" | "abort" => {
            let to = if matches!(word(1), "work" | "transaction") { word(2) } else { word(1) };
            Some(if to == "to" { Control::RollbackToSavepoint } else { Control::Rollback })
        }
        _ => None,
    }
}

/// First words of the statements that commit by themselves where DDL
/// commits (MySQL's implicit commit), ending an open transaction; not a
/// temporary table's `CREATE` or `DROP`.
pub const COMMITTING: &[&str] = &["create", "alter", "drop", "rename", "truncate", "grant", "revoke", "lock"];

/// Whether a transaction is open after `statements` ran, `open` before
/// them, by their first words: `BEGIN` (`START TRANSACTION`) opens one,
/// `COMMIT` and `ROLLBACK` end it (not `ROLLBACK TO`), and where
/// `ddl_commits`, so does DDL.
pub fn transaction_after(statements: &[&str], engine: Engine, open: bool, ddl_commits: bool) -> bool {
    statements.iter().fold(open, |open, s| {
        let words = leading_words(s, engine, 2);
        let word = |i: usize| words.get(i).map_or("", String::as_str);
        match transaction_control(s, engine) {
            // `START REPLICA` isn't a transaction's.
            Some(Control::Begin) if word(0) == "begin" || word(1) == "transaction" => true,
            Some(Control::Commit | Control::Rollback) => false,
            _ if ddl_commits && COMMITTING.contains(&word(0)) && word(1) != "temporary" => false,
            _ => open,
        }
    })
}

/// Of a failed run's statements that ran, the ones a failure doesn't undo
/// where the statements sent together are one transaction: up to the
/// last that ended one (`COMMIT`, `ROLLBACK`; not `ROLLBACK TO`).
pub fn before_last_end<'a>(statements: &[&'a str], engine: Engine) -> Vec<&'a str> {
    let ends = |s: &str| matches!(transaction_control(s, engine), Some(Control::Commit | Control::Rollback));
    let end = statements.iter().rposition(|s| ends(s)).map_or(0, |i| i + 1);
    statements[..end].to_vec()
}

/// `statement` cut at its commas outside brackets and strings (MySQL's
/// `SET a = 1, b = 2` sets two: `SET a = 1` and ` b = 2`).
pub fn top_level_parts(statement: &str, engine: Engine) -> Vec<&str> {
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut from = 0;
    for token in crate::language::tokenize(statement, engine) {
        if token.kind != TokenKind::Punct {
            continue;
        }
        match &statement[token.range.clone()] {
            "(" | "[" => depth += 1,
            ")" | "]" => depth -= 1,
            "," if depth == 0 => {
                parts.push(&statement[from..token.range.start]);
                from = token.range.end;
            }
            _ => {}
        }
    }
    parts.push(&statement[from..]);
    parts
}

/// How much of a statement's start `leading_words` reads.
const LEADING_BYTES: usize = 2048;

/// The current database's key.
pub const DATABASE: &str = "database";

/// The first `n` words of `statement` (comments and spaces left out),
/// lowercase, with names unquoted and a `@` kept on the word after it, for
/// an engine's `remember` to match on.
pub fn leading_words(statement: &str, engine: Engine, n: usize) -> Vec<String> {
    // Only its start is read (a long INSERT isn't tokenized whole for its
    // first words): comments before them aside, they're in the first few
    // hundred bytes.
    let mut end = statement.len().min(LEADING_BYTES);
    while !statement.is_char_boundary(end) {
        end -= 1;
    }
    let statement = &statement[..end];
    let mut words: Vec<String> = Vec::new();
    let mut at = false;
    for token in crate::language::tokenize(statement, engine) {
        let text = &statement[token.range.clone()];
        match token.kind {
            TokenKind::Whitespace | TokenKind::Comment => continue,
            TokenKind::Semicolon => break,
            TokenKind::Punct if text.chars().all(|c| c == '@') => {
                // `@@` may come as two `@`s.
                match words.last_mut() {
                    Some(last) if at => last.push_str(text),
                    _ => words.push(text.to_string()),
                }
                at = true;
                continue;
            }
            TokenKind::QuotedIdent => words.push(unquote(text)),
            _ => words.push(text.to_lowercase()),
        }
        // `@` `id` read as one word, `@id` (`@@` `session` as `@@session`).
        if at && words.len() >= 2 {
            let word = words.pop().unwrap_or_default();
            let sign = words.pop().unwrap_or_default();
            words.push(format!("{sign}{word}"));
        }
        at = false;
        if words.len() >= n {
            break;
        }
    }
    words
}

/// A quoted name without its quotes (`"x"`, `` `x` ``, `[x]`).
fn unquote(text: &str) -> String {
    let inner = text.get(1..text.len().saturating_sub(1)).unwrap_or(text);
    inner.replace("``", "`").replace("\"\"", "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "postgres")]
    #[test]
    fn a_statements_leading_words_leave_out_comments_and_unquote_names() {
        let words = |sql| leading_words(sql, Engine::Postgres, 4);
        assert_eq!(words("/* why */ SET  search_path -- the app's\n TO app"), ["set", "search_path", "to", "app"]);
        assert_eq!(words("SET \"Odd Name\" = 1"), ["set", "Odd Name", "=", "1"]);
        assert_eq!(words("select 1; set x = 2"), ["select", "1"], "one statement: stops at its end");
        // A long one: only its start is read.
        let long = format!("INSERT INTO t VALUES {}", "(1, 'x'),".repeat(500_000));
        let started = std::time::Instant::now();
        assert_eq!(words(&long), ["insert", "into", "t", "values"]);
        assert!(started.elapsed() < std::time::Duration::from_millis(50), "{:?}", started.elapsed());
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn mysqls_at_signs_stay_on_their_names() {
        let words = |sql| leading_words(sql, Engine::MySql, 3);
        assert_eq!(words("SET @id = 1"), ["set", "@id", "="]);
        assert_eq!(words("set @@session.sql_mode = ''"), ["set", "@@session", "."]);
        assert_eq!(words("USE `my db`"), ["use", "my db"]);
    }

    /// Where a `ROLLBACK` undoes them, a transaction's changes stand only
    /// once it commits in the same run.
    #[cfg(feature = "postgres")]
    #[test]
    fn a_transactions_changes_stand_once_it_commits() {
        let pg = Engine::Postgres;
        let settled = |sql: &[&'static str], open: bool| settled(sql, pg, open, true);
        assert_eq!(settled(&["SET a TO 1", "SELECT 1"], false), ["SET a TO 1", "SELECT 1"], "no transaction");
        assert_eq!(settled(&["BEGIN", "SET a TO 1", "COMMIT"], false), ["SET a TO 1"]);
        assert!(settled(&["BEGIN", "SET a TO 1", "ROLLBACK"], false).is_empty());
        assert_eq!(
            settled(&["BEGIN", "SAVEPOINT s", "SET a TO 1", "ROLLBACK TO s", "SET b TO 2", "COMMIT"], false),
            ["SET b TO 2"],
            "a rollback to a savepoint undoes what came before it (not followed: none kept); the transaction goes on"
        );
        // Said in full, a savepoint's rollback all the same, and the final
        // ROLLBACK ends it: nothing kept.
        assert!(
            settled(
                &["BEGIN", "SAVEPOINT s", "ROLLBACK WORK TO SAVEPOINT s", "SET search_path TO reporting", "ROLLBACK"],
                false
            )
            .is_empty()
        );
        assert!(settled(&["BEGIN", "SET a TO 1"], false).is_empty(), "left open: not known");
        assert!(settled(&["SET a TO 1"], true).is_empty(), "open from an earlier run");
        assert_eq!(settled(&["SET a TO 1", "COMMIT"], true), ["SET a TO 1"], "committed in this run");
        // Where a rollback doesn't undo them, every one stands.
        assert_eq!(settled_all(&["BEGIN", "SET a = 1", "ROLLBACK"]), ["BEGIN", "SET a = 1", "ROLLBACK"]);
    }

    #[cfg(feature = "postgres")]
    fn settled_all<'a>(statements: &[&'a str]) -> Vec<&'a str> {
        settled(statements, Engine::Postgres, false, false)
    }

    #[cfg(feature = "mysql")]
    #[test]
    fn a_comma_counts_only_outside_brackets_and_strings() {
        let my = Engine::MySql;
        assert_eq!(top_level_parts("SET a = 1, b = 2", my), ["SET a = 1", " b = 2"]);
        assert_eq!(top_level_parts("SET a = CONCAT('x', 'y')", my).len(), 1);
        assert_eq!(top_level_parts("SET a = 'x, y'", my).len(), 1);
    }

    #[test]
    fn the_database_first_the_latest_of_each_and_bounded() {
        let mut memory = SessionMemory::default();
        memory.set("sql_mode", "sql_mode");
        memory.set(DATABASE, "database shop");
        memory.set("time_zone", "time_zone");
        memory.set("sql_mode", "sql_mode");
        memory.temporary_tables = true;
        assert_eq!(memory.lost(), ["database shop", "time_zone", "sql_mode", "temporary tables"]);
        memory.reset("time_zone");
        assert_eq!(memory.lost(), ["database shop", "sql_mode", "temporary tables"]);
        // However many are set, a bounded few are kept, the database too.
        for i in 0..10_000 {
            memory.set(&format!("@v{i}"), &format!("@v{i}"));
        }
        assert_eq!(memory.settings.len(), MAX_SETTINGS);
        assert_eq!(memory.settings[0].label, "database shop");
        assert_eq!(memory.settings.last().unwrap().label, "@v9999");
    }

    #[test]
    fn a_failed_run_keeps_only_what_a_transaction_end_settled() {
        let kept = |statements: &[&str]| before_last_end(statements, Engine::Postgres).len();
        assert_eq!(kept(&["SET search_path TO app"]), 0, "undone with the failure");
        assert_eq!(kept(&["SET a = 1", "COMMIT", "SET b = 2"]), 2, "up to the commit");
        assert_eq!(kept(&["BEGIN", "SET a = 1", "ROLLBACK TO s", "SET b = 2"]), 0, "a savepoint's isn't an end");
        assert_eq!(kept(&["SET a = 1", "ROLLBACK", "SET b = 2"]), 2);
        assert_eq!(kept(&["BEGIN", "ROLLBACK WORK TO SAVEPOINT s", "SET b = 2"]), 0, "nor is ROLLBACK WORK TO");
    }
}
