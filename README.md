# Podėlis

A terminal UI SQL client for PostgreSQL, MySQL and SQLite, built in Rust
with [ratatui](https://ratatui.rs). Runs as a plain terminal app — works
fine inside a regular terminal or inside a Neovim `:terminal` split, the
same way tools like `lazygit` do.

> [!WARNING]
> Podėlis is in early development (0.x). Breaking changes can come with
> any release: settings, saved connections, keys and command-line options
> may change, sometimes without a migration. Use it with caution,
> especially on production databases: prefer read-only connections there,
> and keep backups.

> [!NOTE]
> Supported platforms: **Linux and macOS**, on x86_64 and ARM (aarch64).
> Windows isn't supported.

![Podėlis: running a query, browsing the result, reviewing a write in a transaction](https://raw.githubusercontent.com/taislius/podelis/master/docs/demo.gif)

## Features

- **Engines:** PostgreSQL and MySQL over TCP (hostname, IPv4/IPv6), a Unix
  socket, or an **SSH tunnel**; SQLite files. Selectable SSL mode
- **Projects:** group connections per project (e.g. `shop-backend` with its
  local/staging/prod databases). Each project has its own query history and
  editor draft, and two projects can each have a connection with the same
  name
- **Safety:** mark a connection *production* (shown in red, asks before
  anything that changes data) or *read-only* (refuses writes, and the server
  enforces it too). In every mode, `UPDATE`/`DELETE` without `WHERE`, `DROP`,
  `TRUNCATE` and `ALTER … DROP` ask first — and can instead **run in a
  transaction for review**: see how many rows changed, check with SELECTs,
  then commit or roll back. **Transaction modes** (Ctrl+K x then
  `a`/`m`/`r`, or click them in the editor's footer): *auto-commit*
  commits each statement as it runs, *manual commit* holds every write in
  a transaction for COMMIT or ROLLBACK, and *rollback-only* runs writes,
  shows how many rows they changed and always rolls them back (a dry run).
  Production connections start in manual commit, and a read of a whole
  big table there asks first. Per-connection **query timeouts** cancel runaway
  queries on the server. An open transaction is shown, and disconnecting or
  quitting asks before rolling it back
- **Editor:** syntax highlighting, line numbers, undo/redo, selection, word
  movement, autocomplete for tables, columns and keywords — including CTE
  names, subquery aliases and the columns they produce — run the selection
  or the statement under the cursor, find and replace, comment toggling,
  bracket matching, **SQL formatting**, **one menu for every action**
  (Ctrl+K, then a letter — or type to search),
  optional **vim keys**, **rebindable keys** and **mouse** support
- **Tabs and a query library:** several query tabs per project, each with
  its own results and **its own connection** (so one tab can be on staging
  while another is on prod); keep working in other tabs while a long read
  runs in one. Save a tab to the project's
  library under a name and group (`reports/churn by month`), rename or move
  it, and search everything — names, groups, SQL text and your history —
  from one window (F3)
- **Compare two connections** open in tabs (Ctrl+K, type “compare”):
  - the **schema**: tables, views, columns, indexes and foreign keys only
    on one side or different, with the SQL that makes the other side match
    (drops left commented out) — `e` opens it in a tab on that connection.
    Across engines (say PostgreSQL and MySQL) the same type under another
    name isn't a difference, and the SQL uses the other engine's names
  - the **rows** the statement at the cursor returns on each (reads only):
    matched by the first column when it identifies them, the result shows
    rows only here (`−`), only there (`+`) and changed pairs (`≠`)
  - the schema with a **snapshot** saved earlier ("what changed in prod
    since the release?")
- **Watch a query:** run it again every few seconds, the rows that changed
  marked; **chart a column** (`b`) as bars and a sparkline; **related
  tables** (`f` in the navigator) through foreign keys both ways
- **Sessions:** on startup Podėlis reopens the last project the way you
  left it — tabs, each tab's connection (passwords from the keychain),
  cursor positions, the focused pane and expanded tables. Results aren't
  re-run. If reconnecting takes a moment, a small card shows which
  connections are back; the first run starts with a welcome screen
- **Query parameters:** `:customer_id` (any engine), `$1` (Postgres), `?`
  (MySQL, SQLite) or SQLite's `?2`, `@name` and `$name` placeholders are
  asked for before running, and the values remembered
- **Errors:** the position the server reports is underlined in the editor.
  After a pause in typing, each statement is *prepared* (checked, never run)
  by the connected server, so syntax errors and unknown tables/columns show
  up exactly as your server version sees them. When that isn't possible
  (not connected, a query or transaction in progress), a hint from
  [`sqlparser`](https://crates.io/crates/sqlparser) is shown instead —
  advisory only, as it can be wrong for valid dialect-specific SQL
- **Lint:** when there's no error, valid SQL that likely isn't what you
  meant gets a `LINT` warning: `UPDATE`/`DELETE` without `WHERE`, `= NULL`
  (never true), `NOT IN (subquery)` (no rows if the subquery yields a
  NULL), comma joins with no condition, `LIMIT` without `ORDER BY`
  inside a subquery or a write, and MySQL's `||` (a logical OR unless
  `sql_mode` has `PIPES_AS_CONCAT`). With several, the label reads
  `LINT 1/3` and `Ctrl+K w` goes to the next. `lint = false` turns them off
- **Queries** run on one dedicated session, so `SET`, `BEGIN`/`COMMIT`, `USE`
  and temp tables persist between runs. Run a whole `;`-separated script:
  each statement that returns rows gets its own **result set** (`<` / `>`
  switch, `RESULT 2/3` shows which), the last one first. Cancel a running
  query on the server (`pg_cancel_backend` / `KILL QUERY`; SQLite interrupts it)
- **Results:** type-aware columns, sideways scrolling, sort, filter, a
  full-value view (JSON pretty-printed), copying (Ctrl+C: a cell, rows, marked
  rows as Markdown, CSV, JSON or `INSERT`s), CSV, JSON and SQL `INSERT`
  export,
  and **editing cells in place** (saved by primary key, through the same
  safety checks). A read-only query **streams**: the first 1,000 rows show
  at once and more load as you scroll. MESSAGES and QUERY PLAN tabs hold
  run outcomes and `EXPLAIN` plans (F7, without running the query; made at
  once even while another query runs). The
  plan starts with **what stands out**, each pointing at its step: the
  slowest (or costliest) step, big filtered scans and nested full scans,
  row estimates far off the actual counts, sorts and hashes spilling to
  disk, correlated subqueries. A **record view** (`x`) shows a row
  vertically, and `f` follows a foreign key to the row it points at
- **Long queries:** when one that ran 10 s or more finishes while you're in
  another window, the terminal rings and shows a desktop notification
  (iTerm2, WezTerm, Ghostty, kitty…; `notify_after` sets the
  time)
- **Schema navigator** of tables (with row estimates), views, functions,
  columns and primary keys; expanding a table also shows its indexes and
  where its foreign keys point, and `d` shows the definition of a table,
  view or function; `.` shows the database's own system tables
- **Paste a connection URL** (`postgres://…`, `mysql://…`, `sqlite:…`) into
  the connect form to fill it in
- **Saved connections:** profile metadata stored locally, passwords in the OS
  keychain (Keychain on macOS, Secret Service on Linux) — never written to disk in plaintext.
  Or a **password command** prints the password: `pass`, 1Password's `op`,
  Vault, a cloud CLI
- Values are shown as the server formats them, so every type displays —
  including enums, arrays, intervals, JSON and `NULL`. Binary data is in
  hex, written as each engine writes it (`\xdeadbeef`, `0xDEADBEEF`,
  `X'DEADBEEF'`), so exports, pipes and comparisons get the real bytes, and
  an edited value or an exported `INSERT` goes back as binary
- **For AI agents:** `podelis mcp` gives your coding agent the connections
  you share with it, in read-only transactions unless you allow writes, with
  no passwords and nothing destructive. Agent activity shows everything
  each agent session ran (which agent, which project folder), and the
  status bar says when an agent was refused or changed data. Or **approve
  each write** yourself: Podėlis shows it with its dry run, `y` runs it
- **Works with Unix tools:** pipe a result through `grep`, `sort`, `awk`,
  `jq` or Miller without leaving the app (`|`), open it in `less` or
  VisiData (`o`), or run SQL without the UI (`podelis prod -e "select …"
  -f csv | …`) with the same saved connections, keychain and safety rules
- **Resilient:** a dropped connection is reopened (reads are retried); a
  crash is written to `crash.log`; `PODELIS_LOG=debug` writes a debug log
- **Connection debugging:** test a connection step by step (DNS, TCP,
  login, TLS, latency) and see where it breaks, with plain-language hints
  for common errors; see who you're connected as and with which settings;
  an activity monitor of the server's sessions (cancel or end them);
  `EXPLAIN ANALYZE`; and `podelis doctor` to check your own setup

## Install

With [Homebrew](https://brew.sh) (macOS and Linux):

```sh
brew install taislius/tap/podelis
```

Prebuilt binaries for Linux and macOS are also attached to each
[GitHub release](https://github.com/taislius/podelis/releases). Or build from
source, with Rust 1.94 or newer (distro packages are often older; get it with
[rustup](https://rustup.rs)):

```sh
cargo install --locked --git https://github.com/taislius/podelis
```

## Run

```sh
podelis                         # reopen the last session (or start on the projects)
podelis -p shop-backend         # open a project directly (case-insensitive)
podelis postgres://me@db/shop   # connect to a URL (also mysql://, sqlite:)
podelis app.db                  # a SQLite file (a missing one is created only if you confirm)
podelis -p shop prod            # a saved connection (password from the keychain)
```

Without the UI, for scripts and pipes, see
[Without the UI](docs/command-line.md); for AI agents, `podelis mcp` (see
[For AI agents](docs/agents.md)).

From a checkout, `cargo run` (or `cargo run -- -p shop-backend`). No config
file is needed — the first run starts with a welcome screen.

## Documentation

- [Projects and connections](docs/connections.md): projects, connection
  settings, passwords (keychain, password commands, `.pgpass` /
  `.my.cnf`), databases in Docker
- [The query screen](docs/query-screen.md): the editor, results, tabs and
  the query library, and every key
- [Without the UI](docs/command-line.md): `-e`, `--import`, `--test`,
  `podelis doctor`, exit codes, and use on servers and in CI (also
  `man podelis`)
- [For AI agents](docs/agents.md): `podelis mcp`, sharing connections with
  agents safely, and reviewing what they ran
- [Configuration](docs/configuration.md): `config.toml`, keys, themes, and
  where Podėlis keeps its files
- [Development](docs/development.md): building, tests, checks and CI; how
  the code is organised is in [architecture](docs/architecture.md)

## Licensing

Copyright (C) 2026 Taislius <taislius@proton.me>.

Licensed under the [GNU Affero General Public License v3.0](LICENSE)
(`AGPL-3.0-only`). Pull requests aren't accepted, but bug reports and
ideas are welcome (see [CONTRIBUTING.md](CONTRIBUTING.md)).

Release archives include `THIRD-PARTY-LICENSES.txt` with the licences of
the bundled crates (`./scripts/third-party-licenses.sh` generates it; needs
`cargo install cargo-about --features cli`).
