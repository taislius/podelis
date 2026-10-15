# Architecture

Podėlis is one binary with three layers:

```
src/
  main.rs, cli.rs        startup, command line, -e / --test / --import, doctor
  app/                   state and behaviour (no drawing): screens, tabs, keys,
                         editing, running queries, the schema navigator…
  ui/                    drawing with ratatui; reads the app's state
  db/                    talking to databases (below)
  export.rs              results as CSV, TSV, JSON, INSERTs or a table, for
                         exports, pipes and -e alike
  sql.rs, analyze.rs,    SQL text: lexing, statement splitting, analysis,
  complete.rs, params.rs completion, parameters (engine rules come from db)
  parse.rs, scope.rs,    the syntax tree: parsing once, scopes, lint rules
  lint.rs                (see "SQL checks")
```

The app owns all state; the UI only draws it (and records where it drew
clickable things, for the mouse). Background work (connecting, queries,
catalog loads) runs in tokio tasks that report back as events, tagged with a
generation so results from a superseded connection are ignored.

## The app's building blocks

A few pieces in `app/` are shared, so the same thing works the same way
everywhere:

- **Tabs** (`tabs.rs`): the active tab's state is `App::tab`, a `TabState`
  with the editor, the last run and a `ResultGrid` (`grid.rs`: the result
  sets, filter, sort, marks, selection and scroll). Switching tabs swaps
  the whole struct with a parked one, so nothing can leak between tabs.
- **Kept results** (`runs.rs`): each query run opens a `KeptRun` in the
  tab's `runs`. The one shown lives in the tab (`grid`, `last_sql`, its
  infos and parameters, its error), so every key works on it unchanged;
  the others are parked in their slots and swapped in to be shown, the
  way tabs are. A re-run of the shown result (`M`, the refresh after a row
  change, watch mode) replaces it instead (`rerun_shown`). Old unpinned
  ones go past `KEEP` results or `KEEP_ROWS` rows.
- **Popups** (`overlay.rs`): `App::overlays()` lists the open popups front
  first. Key handling, drawing (back to front), mouse blocking and "is the
  user typing" all read it, so a new popup is added in one place.
- **Text inputs** (`input.rs`): `LineInput` is one line with a cursor and
  the same keys everywhere (←/→, Home/End, Ctrl+U, Ctrl+W, paste). Its
  `handle` reports `Changed`, `Moved`, `Submit`, `Cancel` or `Ignored`, and
  each prompt reacts only to what concerns it. Forms edit their fields
  with the same keys through `edit_at_end`.
- **Lists** (`list.rs`): `list::step` moves any list's selection, with a
  `Nav` saying whether it wraps, takes j/k and Home/End, or Tab.
- **Messages** (`mod.rs`): `set_status` puts a message where it belongs.
  Said while handling a key, click or paste with the editor or the results
  focused (no popup, find bar or cell prompt open), it goes to that pane's
  footer (`editor_note`, `results_note`) and to MESSAGES; otherwise to the
  status bar. A run's progress and outcome use `set_result_status`, for the
  results pane's footer. They last until the next key (a run's until it
  ends). The MESSAGES tab is a list (`messages.rs`): one can be selected,
  opened whole and copied.
- **Runs and tabs** (`query.rs`, `tabs.rs`, `events.rs`): one query runs
  at a time (`executing`). A read records its tab (`run_owner`), and other
  tabs can then be shown; a write, a review or a comparison leaves it
  `None`, which keeps every tab in place (`blocks_tabs`). A run's events
  carry its `run_id` rather than the connection's `generation`, so they
  outlive connection switches (not a disconnect), and `in_tab` applies
  them to the owner tab, swapping its state and connection in and back
  out. F7 meanwhile plans on a pool connection (`Database::run_aside`)
  where that sees what the session does.

## Databases: ports and adapters

The app talks to one type, `db::Database` (the port). Each engine is a
self-contained adapter folder:

```
db/
  mod.rs          Database, the Pool/Session enums (the one dispatch point)
  dialect.rs      trait Dialect + Capabilities + LexerRules + Placeholders
  run.rs          running, streaming, cancelling, checking statements
  catalog.rs  details.rs  info.rs  activity.rs
                  Database methods: each is one `match` over the engine that
                  calls that engine's function
  errors.rs  diagnose.rs  tunnel.rs  options.rs
                  shared: error messages, the connection test, SSH tunnels,
                  connection parameters
  postgres/  mysql/  sqlite/
    mod.rs        the Dialect: names, quoting, lexer rules, placeholders,
                  EXPLAIN, plan formatting, capabilities…
    connect.rs    connection options, pool and session
    queries.rs    catalog, table details, routines, session info, TLS,
                  activity, cancel, error classification
    cells.rs      decoding values as text
```

Three concerns are kept apart:

- **Dialect** (`engine.dialect()`): what differs in SQL text, with no I/O —
  identifier quoting and string literals, lexer rules (comment styles,
  quoting, dollar quotes), placeholders (`$1`, `?`), the sqlparser and
  formatter dialects, EXPLAIN statements and plan formatting, import column
  types, default-row INSERTs.
- **Capabilities** (`engine.capabilities()`): what an engine supports —
  file-based, cancel (and so timeouts), activity monitor, schemas, routines,
  server-side transaction state, DDL that commits, CREATE statements in the
  catalog. The app and UI ask these instead of checking which engine it is.
- **Driver** (the engine folder's `connect.rs` / `queries.rs` / `cells.rs`):
  the I/O, through sqlx.

Dispatch is a closed enum, not trait objects: `Database` matches on its
`Pool`/`Session` enum and calls the engine's functions. That keeps calls
static (no boxed futures for async), and the compiler lists every place a new
engine must handle.

Outside `db/`, code doesn't match on `Engine` except for defaults and tests.

### Adding an engine

1. A folder `db/<engine>/` with `mod.rs` (a `Dialect` impl), `connect.rs`,
   `queries.rs` and `cells.rs`, modelled on an existing one.
2. A variant in `Engine` (`options.rs`, plus `Engine::ALL`), in `Pool` and
   `Session` (`mod.rs`), and its static in `dialect.rs`.
3. Build: the compiler points at each `match` that needs the new arm (connect,
   catalog, details, info, activity, run). Capabilities decide what the app
   offers, so unsupported features simply don't show.
4. Add it to the live-database tests (`db/tests.rs`).

## The gateway

`gateway/` is a separate process, `podelis gateway`: the one place for
AI models, the context they're shown, their tools and memory (the plan is
in the private planning notes). Clients talk to it; they don't call
models themselves.

```
gateway/
  protocol.rs   JSON-RPC 2.0, one message per line; requests, responses,
                and notifications the gateway pushes (`stopping`)
  server.rs     one gateway per config folder (a lock file held for the
                process's life), the socket (`gateway/gateway.sock`,
                0600 in a 0700 folder), connections, stopping when no
                client has been connected for `gateway_idle`, the call log
  client.rs     connect and say hello, call, and start the gateway when
                none answers (`connect_or_start`, `spawn_gateway`)
  command.rs    `podelis gateway [status | start | stop]`
```

The first request on a connection must be `hello`, with the client's
name and the protocol version (`PROTOCOL`); anything else, or another
version, is answered with an error and the connection closed. The call
log records the method and the outcome, never the parameters. A gateway
that crashed leaves its socket file behind; the next one holds the lock,
so it knows the file is stale and replaces it.

## SQL checks

Podėlis checks SQL before it is sent: syntax hints, lint warnings in the
editor, and a confirmation before risky statements.

### The pipeline

```
SQL text
   │
   ▼
parse::parse                      split with our lexer, parse each statement once
   │                              (sqlparser, the engine's dialect)
   ├── fails ──► analyze::syntax_hint      gentle hint, never blocks
   │             analyze::classify_words   keyword fallback for the risk check
   ▼
sqlparser AST
   │
   ├──► scope::lower ──► Scope tree        names each SELECT can see
   │                        │
   ├──► lint.rs: Lints visitor + scopes ──► findings { rule, message, span, risk }
   │                                            │
   │                       ┌────────────────────┴───────────────────┐
   │                       ▼                                        ▼
   │              lints: editor underline             lint::risk: confirmation
   │              (unless settings.lint is off)       before running (always)
   │
   ├──► analyze::classify_ast        Kind, and risks that come from the kind
   │                                 of statement (DROP, TRUNCATE, ALTER … DROP)
   └──► analyze::edit_source         which table a result can be edited in
```

Rules:

| Rule | Effect |
|---|---|
| `no-where` (UPDATE/DELETE) | underline, and confirmation |
| `null-compare` | underline |
| `not-in-null` | underline |
| `cross-join` (uses scopes) | underline |
| `limit-no-order` | underline |
| `pipes-or` (MySQL) | underline |
| TRUNCATE, DROP, and ALTER that drops a column, partition, constraint or index (in `classify_ast`; not `DROP DEFAULT` / `DROP NOT NULL`) | confirmation |

What holds it together:

- **The parser is `sqlparser`, not our own.** Dialect coverage is years of
  work.
- **Parse once.** `ParsedScript` holds each statement's text, its offset in
  the script and its tree (or the parse error). The editor's syntax hint and
  lint share one parse; `ParsedStatement::range` turns a node's span into
  script bytes in one place.
- **Failure is quiet.** `sqlparser` doesn't know every dialect feature, so a
  statement it can't parse gets a hint and no lint warnings, never false
  ones. The risk check falls back to keywords so dangerous SQL is still
  caught.
- **Positions are exact.** Warnings underline the node, not the line.
- **A rule is written once.** `no-where` is one rule whose finding carries a
  risk; the editor shows its message, and `analyze` asks for confirmation
  from its risk. Risks only count when the statement runs: `EXPLAIN DELETE …`
  needs no confirmation, `EXPLAIN ANALYZE DELETE …` does. Because the rules
  walk the whole tree, a `DELETE` without `WHERE` inside a CTE is confirmed
  too.

### Dialects

Each database speaks its own SQL. The differences come at three levels, and
each is handled in a different place.

#### Syntax: what text is valid

Backticks (MySQL), `$$` bodies (PostgreSQL), `LIMIT 5, 10` (MySQL),
`RETURNING`, `ON CONFLICT`. **The parser handles this.**

In `sqlparser` a dialect isn't a separate parser: there is one parser, and
the dialect is a trait of switches (about 140 `supports_*` methods) and hooks
for quoting and precedence. Podėlis picks it per connection:
`Dialect::parser()` in each engine's `mod.rs` returns `PostgreSqlDialect`,
`MySqlDialect` or `SQLiteDialect`. Our own lexer in `sql.rs` makes the same
per-engine choices through `LexerRules`, so statement splitting gets dollar
quotes and backticks right even when `sqlparser` gives up.

**There is one tree for all dialects.** `Statement` and `Expr` are a
superset; some variants only come from one dialect
(`LimitClause::OffsetCommaLimit` is MySQL's `LIMIT a, b`). Because of this,
most rules don't need to know the database: a `DELETE` without `WHERE` looks
the same in all three.

#### Meaning: same text, different result

The parser can't handle this level. The tree records how `sqlparser` read
the text, which isn't always what the server will do. In MySQL mode,
`'a' || 'b'` still becomes `BinaryOperator::StringConcat`, but MySQL (without
`PIPES_AS_CONCAT`) treats `||` as logical OR. Other examples:

- **Name case:** PostgreSQL lowercases unquoted names; MySQL table names
  depend on the platform and `lower_case_table_names`; SQLite ignores case.
- **Double quotes:** a string literal in MySQL (without `ANSI_QUOTES`); in
  SQLite, a double-quoted name that matches no column quietly becomes a
  string; an identifier in PostgreSQL.
- **Text comparison:** MySQL's usual collations ignore case, so
  `'a' = 'A'` is true; in PostgreSQL it is false.
- **Types:** SQLite stores `'abc'` in an `INTEGER` column unless the table is
  `STRICT`.

**Dialect rules handle this level**: a rule sees a node and asks the dialect
what it means.

#### Version and settings: what this server accepts

MySQL's `sql_mode`, MariaDB versus MySQL (both use `MySqlDialect`; MariaDB
has `RETURNING` and sequences, MySQL doesn't), the SQLite version,
PostgreSQL extensions. Neither the parser nor the rules can know these.
**The server decides.** Podėlis asks it when that is safe
(`server_syntax_hint` in `app/events.rs`) and uses the local parser
otherwise.

Settings that change *meaning* can be read, though. Podėlis reads MySQL's
`@@SESSION.sql_mode` when it connects and again whenever the catalog
reloads (which happens after a `SET`), keeps it on the `Database`, and
passes it to the rules: `Dialect::pipes_are_or(sql_mode)`. Facts that
depend on settings take them as arguments; facts that don't stay plain
methods.

A lesson from the live tests: the driver can change settings too. sqlx
adds `PIPES_AS_CONCAT` to every MySQL session by default, so in Podėlis
`||` concatenated while the `mysql` client and most applications get a
logical OR. Podėlis now turns that off (`pipes_as_concat(false)` in
`db/mysql/connect.rs`), so a session starts with the server's own mode.

That is why `sqlparser`'s answer is only a hint. Its dialects are
approximations: it rejects some valid SQL and accepts some invalid SQL, so
staying quiet when it fails is the correct response.

#### Where dialect rules live

**The facts go in the engine folders; the rules go in `lint.rs`.**

```rust
// db/dialect.rs: the question, with a default
pub trait Dialect: Sync {
    …
    /// `||` is logical OR, not string concatenation, under `sql_mode`.
    fn pipes_are_or(&self, _sql_mode: &str) -> bool { false }
}

// db/mysql/mod.rs: the fact about MySQL
fn pipes_are_or(&self, sql_mode: &str) -> bool {
    !sql_mode.split(',').any(|m| m.trim().eq_ignore_ascii_case("PIPES_AS_CONCAT"))
}

// lint.rs: the rule, written once
Expr::BinaryOp { op: BinaryOperator::StringConcat, .. } if self.pipes_are_or => {
    self.add("pipes-or", "|| means OR in MySQL, not concatenation: use CONCAT()", expr.span());
}
```

Why not whole rules per engine folder:

- **Most traps apply to more than one database.** Double quotes as strings
  affect MySQL and SQLite; name case is one question with three answers.
  Rules in folders would be copied; facts are answered once per engine.
- **Layers stay apart.** Engine folders never look at the syntax tree; their
  only contact with `sqlparser` is `parser()`. `Dialect` is "what differs in
  SQL text, with no I/O" (see "Databases" above), and a fact like
  `pipes_are_or()` fits that. Tree-walking rules don't.
- **It matches the rest of the code**, which asks `engine.dialect()` and
  `engine.capabilities()` questions instead of matching on the engine.
- **All rules in one place**, each tested against all three engines side by
  side.

Revisit this if one engine gathers many checks of its own (say ten
PostgreSQL-only rules): a `db/postgres/lint.rs` that adds to the shared
visitor would then be reasonable.

### Decisions

Things the usual "build a SQL linter" advice suggests that Podėlis won't do,
and why:

- **No hand-written parser and no full copy of `sqlparser`'s tree.** A copy
  would break on every upgrade; lowering to scopes (`scope.rs`) keeps only
  what the rules need.
- **No `SELECT *` rule.** It's valid and it's what people type at a prompt.
  Rules here catch SQL that probably doesn't do what was meant.
- **No performance guesses** ("missing index", "sequential scan"). Guessing
  from text is often wrong; `EXPLAIN` gives the planner's real answer and
  Podėlis already shows it.
- **No custom rule language.** It's a second product (language, config,
  docs) and only pays off when users need rules nobody anticipated.
- **Rule names, not codes.** `no-where` says what it is; `L002` needs a
  lookup.
- **One visitor, not one struct per rule**, until the visitor gets hard to
  read or rules need switching on and off one at a time.
- **Dialect rules are ordinary rules in `lint.rs`** that ask the dialect;
  the facts they ask about live in the engine folders (see "Where dialect
  rules live"). `pipes-or` is the first; add more as real traps turn up.

## Tests

- Unit tests next to the code; app behaviour in `app/tests.rs`; drawing in
  `ui/tests.rs` with insta snapshots (`ui/snapshots/`).
- Live-database tests in `db/tests.rs` run against real PostgreSQL and MySQL
  when `PODELIS_TEST_PG_PORT` / `PODELIS_TEST_MYSQL_PORT` are set; SQLite
  always runs.
- Property tests (`fuzz_tests.rs`) throw random SQL-ish text at the lexer,
  splitter, analyzer, completion, parameters and editor.
- `scripts/check.sh` runs formatting, clippy (pedantic, warnings as errors),
  all tests and cargo-deny; the git pre-push hook runs it.
