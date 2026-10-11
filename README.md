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
  Production connections start in manual commit. Per-connection **query timeouts** cancel runaway
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
  while another is on prod). Save a tab to the project's
  library under a name and group (`reports/churn by month`), rename or move
  it, and search everything — names, groups, SQL text and your history —
  from one window (F3)
- **Compare two connections** open in tabs (Ctrl+K, type “compare”):
  - the **schema**: tables, views, columns, indexes and foreign keys only
    on one side or different, with the SQL that makes the other side match
    (drops left commented out) — `e` opens it in a tab on that connection
  - the **rows** the statement at the cursor returns on each (reads only):
    matched by the first column when it identifies them, the result shows
    rows only here (`−`), only there (`+`) and changed pairs (`≠`)
- **Sessions:** on startup Podėlis reopens the last project the way you
  left it — tabs, each tab's connection (passwords from the keychain),
  cursor positions, the focused pane and expanded tables. Results aren't
  re-run. If reconnecting takes a moment, a small card shows which
  connections are back; the first run starts with a welcome screen
- **Query parameters:** `:customer_id` (any engine), `$1` (Postgres) or `?`
  (MySQL, SQLite) placeholders are asked for before running, and the values
  remembered
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
  query on the server (`pg_cancel_backend` / `KILL QUERY`)
- **Results:** type-aware columns, sideways scrolling, sort, filter, a
  full-value view (JSON pretty-printed), copy cell/row, CSV, JSON and SQL
  `INSERT` export,
  and **editing cells in place** (saved by primary key, through the same
  safety checks). A read-only query **streams**: the first 1,000 rows show
  at once and more load as you scroll. MESSAGES and QUERY PLAN tabs hold
  run outcomes and `EXPLAIN` plans (F7, without running the query). The
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
  including enums, arrays, intervals, JSON and `NULL`
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

Prebuilt binaries for Linux and macOS are attached to each
[GitHub release](https://github.com/taislius/podelis/releases). Or build from
source:

```sh
cargo install --git https://github.com/taislius/podelis
```

## Run

```sh
podelis                         # reopen the last session (or start on the projects)
podelis -p shop-backend         # open a project directly (case-insensitive)
podelis postgres://me@db/shop   # connect to a URL (also mysql://, sqlite:)
podelis app.db                  # a SQLite file (a missing one is created only if you confirm)
podelis -p shop prod            # a saved connection (password from the keychain)
```

Without the UI, for scripts, pipes and agents, see
[Without the UI](#without-the-ui).

From a checkout, `cargo run` (or `cargo run -- -p shop-backend`). No config
file is needed — the first run starts with a welcome screen.

## Without the UI

Podėlis also runs from the command line with no UI. It uses the same saved
connections, keychain passwords, connection modes and timeouts as the app.
It prints results to stdout and messages to stderr, and its exit code
tells a script what happened. The same reference is in `man podelis`
(installed by Homebrew; from a checkout,
`man ./docs/podelis.1`) and, in short, in `podelis --help`.

### Run SQL: `-e`

```sh
podelis prod -e "select id, email from users limit 5"     # aligned table on a terminal
podelis prod -e "select * from orders" -f csv > orders.csv # csv, tsv, json or table
echo "select count(*) from events" | podelis app.db -e -   # SQL from stdin
podelis -p shop prod -e "select 1"                          # a saved connection of another project
podelis -e "select 1"                                       # uses $DATABASE_URL
podelis prod -e "select * from events" --limit 1000         # at most 1000 rows
```

The connection is a URL, a SQLite file or a saved connection's name (in
the last project opened, or in `-p`). Without one, `-e`, `--import` and
`--test` use `$DATABASE_URL`.

**Output.** `-f` picks `table` (aligned, like psql), `csv`, `tsv` or `json`
(an array of objects, one per row, values as strings or `null`). TSV keeps
each row on one line: tabs and line breaks inside values become a space and
`\n`, and `NULL` an empty field (as when piping rows with `|`). The
default is a table on a terminal and CSV when piped. A script whose
statements return no result set (no `SELECT`, no `RETURNING`) prints
`OK: … row(s) affected` to stderr.

**Scripts.** `-e` can run several statements. A table shows every result
set in turn. JSON prints one array per result set, one after another, so
`jq` reads them as a stream (`jq -s` collects them into one array). CSV
and TSV can hold only one table, so they print the last result set and
say on stderr how many earlier ones they left out. With `--limit`, a cut
result says `(first N of M rows)` on stderr.

### Load a file: `--import`

```sh
podelis app.db --import users.csv                  # into the table "users"
podelis prod --import export.tsv --table events -y # into "events", on a production connection
```

`--import file.csv` creates the table (named after the file, or
`--table`) with guessed column types if it doesn't exist, and appends to
it if it does. It all happens in one transaction, so a bad line changes
nothing (on MySQL, a newly created table stays). The first line must name
the columns; `.tsv` and `.tab` files are tab-separated.

Podėlis talks to databases and leaves files to the tools made for them:
query CSV/JSON files with [Miller](https://miller.readthedocs.io), `jq`,
`xsv` or [VisiData](https://www.visidata.org), and pipe Podėlis's output
into them.

### Check connections and your setup

```sh
podelis --test prod                       # connect step by step, show where it fails
podelis doctor                            # config, keychain, password managers, ssh, terminal, viewer, crash log
podelis doctor -p shop --connections      # …and test every saved connection of a project
```

### Exit codes

| Code | `-e`, `--import`, `--test` |
| --- | --- |
| 0 | Done |
| 1 | The SQL (or the import) failed on the server |
| 2 | Usage: a bad option, an unknown project or connection, a missing SQLite file |
| 3 | Refused: a write on a read-only connection, or a production or destructive change without `--yes` |
| 4 | Couldn't connect: network, login, keychain or SSH (`--test`: a step failed) |
| 5 | The connection's query timeout ran out (the query is cancelled) |

Retrying can help with 4 and 5. It won't help with 1–3. A write whose
connection broke (4) may or may not have happened, so check before
running it again.

### Safety rules

`-e` and `--import` follow the connection's mode:

- **Read-only** connections refuse statements that change data, before
  sending them. The session is also opened read-only on the server
  (PostgreSQL's `default_transaction_read_only`, MySQL's `SET SESSION
  TRANSACTION READ ONLY`, a read-only SQLite file).
- **Production** connections need `--yes` (`-y`) to change data.
- **Destructive** statements (`DROP`, `TRUNCATE`, `DELETE` or `UPDATE`
  without `WHERE`, an `ALTER` that drops a column or a key) need `--yes` on any
  connection.
- A SQLite file that doesn't exist is created only with `--create` (a
  missing one is usually a typo).

`--yes` allows both production writes and destructive statements, so a
script that needs it for one harmless reason allows `DROP TABLE` too.
Keep it out of scripts that build SQL from their input.

These rules catch mistakes; they aren't a security boundary. Podėlis reads
the SQL to decide what changes data, so a function that writes looks like
a read, and a script can switch its own session back to read-write. For
unattended scripts, cron jobs and AI agents, connect as a database user
that only has the rights the job needs (for example `GRANT SELECT` only).
Then use a read-only connection in Podėlis as a second layer.

### On servers and in CI

Saved connections read their password from the OS keychain. A server, an
SSH session or a container may have no keychain, or one that's locked or
asks for permission. Podėlis then connects without a saved password (and
says so), so these still work:

- A **password command** on the connection (*Passwd cmd* in its settings),
  e.g. `pass show db/prod`, `op read op://Ops/prod-db/password` or
  `vault kv get -field=password secret/db/prod`.
- PostgreSQL's own `~/.pgpass` file or `$PGPASSWORD`.
- MySQL's `$MYSQL_PWD`, or the `password` in `~/.my.cnf`'s `[client]` or
  `[mysql]` section. Stricter than the mysql client, so the password
  never goes to a server the file wasn't written for (a server can ask
  for the password itself): a file that names a `host` is used for that
  server only, one that doesn't for this machine only (`localhost`,
  `127.0.0.1`, a socket). Its `user` must match the connection's; with
  no User set, the file's user is logged in as, but only on the host it
  names. A file anyone can write to is ignored; `!include` files and
  `~/.mylogin.cnf` aren't read.

The connection test (`Ctrl+T`, `--test`) shows where the password came
from: given (typed, saved or in the URL), the password command, one of
these, or none.

At a terminal, a wrong or missing password is asked for once (not shown as
you type), as `psql` does; with input piped in, it fails instead (exit 4),
so scripts never hang. In the app, the cursor moves to an empty Password
field: type it and press Enter (`Ctrl+S` saves it).

Or pass a URL, or set `$DATABASE_URL` (from your CI's secrets), instead of a
saved name:

```sh
DATABASE_URL=postgres://report:$PGPASSWORD@db/shop podelis -e "select count(*) from orders" -f csv
```

## Usage

### Projects and connections

One screen with three panes, side by side: **projects**, the open
project's **connections**, and the selected connection's **settings**.
`Ctrl+K p` or clicking the project's name in the top bar shows it
with the projects focused; `Ctrl+K o` with the connections focused.
`Tab` / `Shift+Tab` move to the next / previous pane, round all three
(out of the settings, unsaved changes are dropped). `←`/`→` move between the
lists too, and `Esc` goes back to your tabs as you left them. A tab opened to pick a connection (`Ctrl+K o`
on a connected tab), or one you disconnected, is dropped on the way back
if it's still empty and unconnected. A `●` marks the open project and the
connections open in tabs. On a narrow terminal the focused pane and the
next one show. Clicking a row selects it; clicking it again (or a double
click) opens it, and clicking a field in the settings focuses it.
Quitting from here (`q`, `Ctrl+C`) asks first if a transaction is open.

**Projects:**

| Key | Action |
|---|---|
| `↑`/`↓` | Move selection (the open project, or the last one you opened, is preselected) |
| `Enter` | Open the project (its tabs come back and reconnect, as you left them), or create a new one. Switching saves this project's tabs and closes its connections (asks if a transaction is open). On the open project, `Enter` goes to its connections |
| `→` / `Tab` | The project's connections, opening it first if it isn't the open one (its tabs come back behind them) |
| `n` | New project |
| `r` | Rename project |
| `d` | Delete project, with all its connections and their saved passwords (asks for confirmation; not the open one) |
| `Esc` | Back to the project's tabs (with none open: to the project you left, or quit) |
| `q` | Quit (asks if a transaction is open) |

**Connections** (another project selected shows its connections, to look
at; `→` opens it):

| Key | Action |
|---|---|
| `↑`/`↓` | Move selection; the settings pane shows the selected connection |
| `Enter` | Connect (in a new tab if this one is connected), or add a new connection |
| `e` / `→` | Edit the connection's settings in the settings pane (your tab keeps its connection) |
| `n` | New connection |
| `d` | Delete selected connection (asks for confirmation) |
| `m` | Move selected connection (and its saved password) to another project |
| `c` | Find running PostgreSQL and MySQL containers (Docker or Podman): `↵` fills in a new connection from one, with its published port, user and database, and a password command that reads the password from the container (`docker exec … printenv POSTGRES_PASSWORD`) rather than a copy |
| `←` | Back to the projects |
| `Esc` | Back to the project's tabs (connected or not) |
| `q` | Quit (asks if a transaction is open) |

**Settings** (while editing or adding a connection):

| Key | Action |
|---|---|
| `↑`/`↓` | Move between fields (or click one) |
| `←`/`→` | Change Engine, SSL mode or Mode when that field is focused |
| `Enter` | Connect (in a new tab if this one is connected; the tab you were on keeps its connection) |
| `Ctrl+S` | Save this connection (settings locally; password in the OS keychain, unless a password command supplies it). Changing the name and saving renames the profile. A new connection is also saved once it connects (unnamed: after its host/database or file) |
| `Ctrl+T` | Test the connection step by step: SSH, DNS, TCP, login, TLS, session, latency — each with its timing, the failing step with a hint |
| `Esc` | Cancel a connection attempt in progress, otherwise back to the connections (unsaved changes are dropped) |

- **Host** can be a hostname, an IP address (IPv6 with or without brackets)
  or a Unix socket path starting with `/`. Leave User/Password empty to use
  the driver defaults (e.g. your OS user for Postgres). For **SQLite** the
  form asks only for a file path or `:memory:`. A path that doesn't exist
  is usually a typo, so the form says so and creates a new database only
  when you press Enter again.
- **Passwd cmd:** optional. A command that prints the password, run each
  time the connection opens: `pass show db/prod`, `op read
  op://Ops/prod-db/password`, `vault kv get -field=password secret/db/prod`,
  `aws rds generate-db-auth-token …`. It replaces the Password field (and
  the keychain). It runs without a shell (quote words with spaces), gets no
  input (a password manager asks in its own window), and has 60 seconds.
  The first line it prints is the password. It gets the connection's
  details in `PODELIS_ENGINE`, `PODELIS_HOST`, `PODELIS_PORT`,
  `PODELIS_USER` and `PODELIS_DATABASE`, so one script can serve several
  connections. In `connections.toml` it's a list of words:
  `password_command = ["pass", "show", "db/prod"]`.

  With **`pass` and GPG**, the passphrase has to come from gpg-agent's
  cache or a graphical pinentry (`pinentry-mac`, `pinentry-gnome3`,
  `pinentry-qt`): a terminal one (`pinentry-curses`, `pinentry-tty`) can't
  ask while Podėlis has the screen. On a server over SSH, unlock the agent
  first (`pass show db/prod >/dev/null` in your shell); `default-cache-ttl`
  and `max-cache-ttl` in `~/.gnupg/gpg-agent.conf` set how long it stays
  unlocked.
- **Mode:** `read-write`, `production` or `read-only` (see Features →
  Safety). Read-only is enforced by the server too:
  `default_transaction_read_only` on Postgres, `SET SESSION TRANSACTION
  READ ONLY` on MySQL, a read-only open on SQLite.
- **Transaction:** the transaction mode the connection starts in:
  `auto-commit`, `manual commit` or `rollback-only`. Unless you pick one,
  production connections start in manual commit and others in
  auto-commit. `Ctrl+K x` switches it while connected.
- **Timeout:** optional, in seconds. A query running longer is cancelled
  on the server (PostgreSQL, MySQL).
- **SSH via:** optional. `user@host`, `user@host:port` or a host alias from
  `~/.ssh/config`. Podėlis runs your system `ssh -N -L …`, so your keys,
  agent, `~/.ssh/config` and known_hosts all apply. It can't answer
  password or host-key prompts, so use key authentication and connect to the
  host once from a shell first. Host and Port are as seen *from* the SSH
  host (often `localhost`). Through a tunnel, `verify-full` SSL can't match
  the server's hostname; use `require`.

**Databases in Docker.** A container's published port (`-p 5433:5432`) is
reached at `localhost` and that port:

- **The wrong server answering:** if a local PostgreSQL or MySQL already
  uses the port, `localhost:5432` reaches it, not the container, and the
  login fails with the container's password. Publish the container on
  another port.
- **Just started:** "connection refused" or "the database system is
  starting up" until the server is ready; try again in a few seconds.
- **SSL:** the official images don't enable TLS, so SSL mode `require`
  fails; use `prefer` or `disable`.
- **The password from the container**, without saving it, as a password
  command: `docker exec my-postgres printenv POSTGRES_PASSWORD` (or
  `MYSQL_ROOT_PASSWORD`). It needs access to Docker, which is as good as
  root on that machine: for development containers.
- **Password files:** a published container counts as this machine, so a
  `localhost` line in `~/.pgpass` and a `~/.my.cnf` that names no host are
  used for it. Running images you don't trust? Name the `host` in
  `~/.my.cnf`, so its password only goes to the server it's for.
- **From inside Compose,** use the service name (`db`) as Host; in
  `~/.my.cnf` that's `host = db`. From inside a container to a database
  on your own machine (Docker Desktop): `host.docker.internal`.

### Query screen

Most actions are behind one key: **`Ctrl+K`** (or `Space` in the schema
pane and vim's Normal mode; Space twice with `leader_double_space`), then a
letter — `Ctrl+K x r` switches to rollback-only. If you pause
after `Ctrl+K`, a menu shows the letters and the focused pane's own keys
(`?` outside the editor shows it at once). Type anything else (or `/`) to
search every command by name. `F1` shows every key.

| `Ctrl+K`, then | Action |
|---|---|
| `x`, then `a` / `m` / `r` | Transaction mode: *auto-commit* (each statement is committed as it runs), *manual commit* (changes wait for `F8` COMMIT / `Shift+F8` ROLLBACK) or *rollback-only* (statements run, then are always rolled back). Clicking the modes in the editor's footer cycles them |
| `c` / `r` | Commit / roll back the open transaction (also `F8` / `Shift+F8`) |
| `e` / `a` | Explain / explain analyze (also `F7` / `Shift+F7`) |
| `f` / `-` | Format the selection, or everything / comment or uncomment lines |
| `i` | Insert a snippet or saved query at the cursor |
| `n` | Show/hide the navigator |
| `z` | Zoom the focused pane to the whole screen; again or `Esc` returns. Tab still moves between panes (also `z` in results/schema, or double-click a pane's title) |
| `m` | Menu of what you can do in the focused pane (also right-click) |
| `l` / `t` | Search saved queries and history / rename the tab |
| `o` / `d` | Pick a connection for this tab (opens a new tab if this one is connected) / disconnect this tab |
| `p` | Projects and connections (or click the project's name in the top bar): see [Projects and connections](#projects-and-connections) |
| `u` / `s` / `h` | Connection info / the server's sessions / connection heartbeat on or off |
| `1`…`9` | Jump to a tab (the menu lists the open tabs by number; typing a tab's name finds it too) |
| `?` | Every key |

Keys that work directly:

| Key | Action |
|---|---|
| `F5` / `Ctrl+R` / `Ctrl+Enter`¹ | Run the selection, or everything in the editor (or click **▶ F5 Run** right of the tabs; *Statement* and *Explain* are there too) |
| `F6` | Run the selection, or the statement under the cursor |
| `F7` | Show the plan for the selection / statement under the cursor (not executed) |
| `Shift+F7` | `EXPLAIN ANALYZE`: runs the statement and shows actual times and rows (writes ask first and are rolled back) |
| `F8` / `Shift+F8` | Commit / roll back the open transaction |
| `Esc` | Cancel the running query (not supported for SQLite) |
| `Ctrl+P` / `Ctrl+N` | Previous / next query from history |
| `Tab` / `Shift+Tab` | Cycle focus: editor → results → schema (`Tab` right after a word completes it instead; `Esc` then `Tab` still moves on) |
| `Ctrl+C` | Quit (copies instead when text is selected in the editor) |
| `F1` | Every key |

The function keys from earlier versions still work: `F2` navigator, `F3`
library, `F4` rename tab, `F9` format,
`F10` / `Shift+F10` menu, `F12` zoom, and `Ctrl+O`,
`Ctrl+D`, `Ctrl+E`.

**Tabs and the query library** (from any pane):

| Key | Action |
|---|---|
| `Ctrl+T` / `Ctrl+W` | New tab on the same connection / close tab (asks if it has unsaved changes) |
| `Ctrl+PgUp` / `Ctrl+PgDn`, `Ctrl+K 1…9` | Previous / next tab, or jump to one |
| `Ctrl+S` | Save the tab to the library; the first time asks for `group/name` (e.g. `reports/churn by month`), later saves update it |
| `Ctrl+K t` | Rename the tab and its saved query; a different `group/` moves it |
| `Ctrl+K l` | Search saved queries (name, group and SQL) and history; `Enter` opens, `F4` renames/moves, `Ctrl+D` deletes |

A tab's `SQL` label is orange while the tab isn't saved to the library
(its text is still kept as a draft), and a `●` on a saved query's tab means
its text has changed since it was saved. Tabs, their
connections and cursors come back after a restart (unless
`restore_session = false`); results don't, press `F5` to re-run.

**Query parameters.** Running SQL with placeholders — `:name` in any
engine, `$1` on Postgres, `?` on MySQL/SQLite — first asks for their values
(placeholders in strings, comments and `::type` casts don't count). Numbers
and `NULL` go in as written, anything else as a quoted string, and a value
starting with `=` is inserted as raw SQL (`=now()`). Values are remembered
for the next run. History keeps the placeholders, not the values.

¹ `Ctrl+Enter` needs a terminal that supports the kitty keyboard protocol
(kitty, WezTerm, Ghostty, foot, iTerm2 with CSI u enabled). Elsewhere it
arrives as a plain `Enter`; use `F5` or `Ctrl+R`.

**Editor** (when focused):

| Key | Action |
|---|---|
| `←`/`→`/`↑`/`↓`/`Home`/`End`/`PageUp`/`PageDown` | Move |
| `Ctrl+←`/`Ctrl+→` (or `Alt`/`Option`) | Move by word |
| `Ctrl+Home`/`Ctrl+End` | Start / end of the text |
| `Shift` + any movement | Select |
| `Ctrl+A` | Select all |
| `Ctrl+C` / `Ctrl+X` | Copy / cut the selection |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Tab` (after a word) / `Ctrl+Space` | Autocomplete; typing `.` after a table, alias or schema opens it too |
| `Esc`, then `Tab` | Next pane, even right after a word (the status bar hint says `ESC TAB` when plain Tab would complete) |
| `Space` `Space` | The `Ctrl+K` menu, when `leader_double_space = true`: two quick Spaces, and the first is taken back |
| `Ctrl+F` | Find and replace: `Enter`/`↓` next, `↑` previous, `Tab` to the replacement, `Enter` replaces, `Ctrl+A` replaces all |
| `Ctrl+/` | Comment / uncomment the selected lines (`Ctrl+7` in terminals that send that) |
| `Ctrl+K i` / `Ctrl+E` | Insert a snippet or saved query at the cursor (the library window, starting at the `snippets` group) |
| `Ctrl+K m` / right-click | Menu of what you can do in the editor |
| `Ctrl+K f` | Format the selection, or everything |
| `↑`/`↓`, `Tab`/`Enter`, `Esc` | In the completion list: choose, accept, close |

Pasting is supported (bracketed paste), including multi-line SQL.

**Vim keys** (`editor_mode = "vim"` in `config.toml`): Normal, Insert,
Visual and Visual-line modes, shown in the status bar. Supported: counts,
`h j k l w b e 0 ^ $ gg G`, `i a I A o O`, `x X dd D cc C yy p P J r ~ u`,
operators with motions (`dw`, `c$`, `y3w`…), `iw` (`ciw`, `diw`, `yiw`) and
`v`/`V` with `d y c x`. Redo is `Ctrl+Y`, since `Ctrl+R` runs the query.

**Results** (when focused):

| Key | Action |
|---|---|
| `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` | Move between rows |
| `←`/`→` | Move between columns (scrolls sideways) |
| `Enter` / `v` | Show the full value of the cell (JSON pretty-printed) |
| `x` | Show the row vertically, one `column  value` line each; `←`/`→` previous / next row |
| `f` | Follow the foreign key in the selected cell: opens the referenced row in a new tab |
| `\|` | Pipe the rows shown through a shell command (`grep -i vilnius`, `sort -t$'\t' -k3 -n`, `jq '.[].email'`, `mlr --itsv --opprint stats1 -a sum -f total`…) and show its output |
| `o` | Open the rows shown in the viewer: `$PAGER` (else `less -S`), or `viewer` from `config.toml`, e.g. VisiData |
| `s` | Sort by the selected column: ascending → descending → off (numbers by value, then text, then NULLs) |
| `/` | Filter rows (case-insensitive, any column); `Esc` clears |
| `c` | Edit the cell: type the new value, `Enter` saves, `Ctrl+N` sets NULL, `Esc` cancels |
| `a` | Add a row: a form of the table's columns (empty fields get their default; `NULL`; `=expr` for raw SQL) |
| `Space` / `d` | Mark rows / delete the marked rows (or the current one); always asks first, `t` reviews it in a transaction |
| `C` | Copy the marked rows (or all rows shown) as a Markdown table |
| `m` / right-click | Menu of everything you can do here, with each action's key |
| `y` / `Y` | Copy the selected cell / row (tab-separated) |
| `e` / `J` / `i` | Export the rows shown as CSV / JSON / SQL `INSERT` statements to `podelis-export-<timestamp>.<ext>` (in the current directory, or `export_dir`) |
| `M` | Load 10× more rows (streamed results fetch the next pages; scripts are re-run with a bigger limit, read-only ones only) |
| `[` / `]`, `1` `2` `3` (or click a tab) | Switch between the RESULT, MESSAGES and QUERY PLAN tabs (`F7` opens the plan with the results focused; `1` goes back) |

**Snippets** are saved queries in the `snippets` group (save one with
`Ctrl+S` as `snippets/recent orders`). Typing the start of a snippet's name
offers it in autocomplete, and choosing it inserts its SQL; `:parameters` in
it are asked for when you run.

**Menus.** Right-click a pane, or press `m` in the results or schema
(`Shift+F10` anywhere), for a small menu of what you can do there, each
item with its key — a quick way to learn the shortcuts.

Cells can be edited, and rows added or deleted, when the result comes from
a plain `SELECT` on one table (no joins, grouping or computed columns) that includes the table's
primary key. The generated `UPDATE … WHERE <primary key> = …` goes through
the usual checks: production connections ask first, read-only ones refuse.

**Reviewing writes.** When a write needs confirming (production
connections, or `UPDATE`/`DELETE` without `WHERE`, `DROP`…), press `t`
instead of `y`: it runs after a `BEGIN`, then shows how many rows changed —
`c` commits, `r` rolls back, `Esc` leaves the transaction open so you can
check the changes with SELECTs first (`F8` commits, `Shift+F8` rolls back).

**Connection info and activity** (command palette, or bind keys to
`connection_info` / `activity`): the info panel shows the server version,
the user and database you're really on, schemas, time zone, encoding,
isolation level, timeouts, read-only state, backend id, TLS version and
cipher, and the current latency — click the connection status in the top
bar to open it. The activity monitor lists the server's sessions (Postgres
`pg_stat_activity`, MySQL's process list) with their state, time, what
they wait on and who blocks them, refreshing every 2 s: `Enter` shows a
session's full query, `c` cancels its query, `x` ends the session (both ask
first; read-only connections refuse). The **connection heartbeat** checks
an idle connection regularly, so a drop is noticed (and reconnected) before
your next query: switch it on or off from the palette ("Connection
heartbeat", or bind `heartbeat`), with `keepalive = 60` in `config.toml`
as the interval and default (0 = off). While it runs, the status bar
shows `♥` before the connection; a failed check turns it red with
**connection lost** until the next successful check or query. "Copy diagnostics" in the palette
copies versions, terminal and settings for a bug report — no hosts, users
or SQL.

**Pipes and viewers.** `|` sends the rows shown (after filtering and
sorting) to a command's standard input as TSV with a header line — one row
per line, tabs and line breaks inside values written as a space and `\n`,
`NULL` as an empty field. Commands named `jq`, `gron`, `jaq`, `gojq` or `fx`
get JSON instead. The command runs through your shell (`sh -c`), so
pipelines work: `| grep LT | sort | uniq -c`. `o` writes
the rows to a private temporary file and opens it in the viewer; Podėlis
hands it the terminal and comes back when it exits.

**Mouse:** click a pane to focus it, a tab to switch to it, `+` for a new
tab, the editor to place the cursor (drag to select), a result cell or a
schema item to select it; double-click opens a cell or expands a table; the
wheel scrolls. Your terminal's own text selection then needs `Shift` (or
`Option` on macOS); `mouse = false` in `config.toml` turns this off.

Copying uses the OSC 52 terminal escape sequence, which works locally and
over SSH in most modern terminals. In tmux, enable it with
`set -g set-clipboard on`.

**Schema browser** (when focused):

| Key | Action |
|---|---|
| `↑`/`↓` | Move |
| `/` | Filter tables, views and functions by name |
| `Enter` / `→` | Expand a table: its columns (with `→ table.column` for foreign keys) and indexes; on a function, show its definition |
| `←` | Collapse |
| `d` | Show the definition: `CREATE VIEW`, the function source, or a table's columns, keys and indexes (SQLite: its `CREATE TABLE`) |
| `s` | Insert `SELECT * FROM <table> LIMIT 100;` into the editor |
| `r` | Refresh (also happens automatically after DDL, `USE` and `SET`) |
| `.` | Show or hide system tables (`pg_catalog` and `information_schema`; MySQL's `information_schema`, `performance_schema`, `mysql` and `sys`; SQLite's `sqlite_schema` and `sqlite_*`) in a SYSTEM section |
| `Space` | The `Ctrl+K` command menu |

## Configuration

Optional settings go in `config.toml` in the config folder (below):

```toml
row_limit = 1000            # rows kept in memory per result
theme = "dark"              # or "light" for light terminal backgrounds
server_syntax_check = true  # check SQL with the server while typing
lint = true                 # warn about likely mistakes: = NULL, UPDATE without WHERE…
system_tables = false       # list pg_catalog, information_schema, sqlite_schema… in the navigator (`.` toggles)
editor_mode = "standard"    # or "vim"
hints = "minimal"           # status bar key hints: "minimal" (just `^K keys`) or "full"
export_dir = "~/Downloads"  # where exports go (default: current directory)
restore_session = true      # reopen the last project's tabs and connections
mouse = true                # clicks and the wheel (false: terminal selection without Shift)
viewer = "vd"               # opens result rows (`o`); `{}` is the file. Default: $PAGER, else less -S
keepalive = 0               # seconds between checks of an idle connection (0: off)
leader_double_space = false # Space twice quickly in the editor opens the Ctrl+K menu
notify_after = 10           # a query running this many seconds rings and notifies when done (0: off)

[keys]                      # rebind query-screen keys (replaces that action's defaults)
run = ["F5", "Ctrl+Enter"]
new_tab = "Ctrl+N"
```

Rebindable actions (defaults in brackets): `run` (F5, Ctrl+R, Ctrl+Enter),
`run_statement` (F6), `explain` (F7, Ctrl+K e), `complete` (Ctrl+Space),
`history_prev` / `history_next` (Ctrl+P / Ctrl+N), `undo` / `redo`
(Ctrl+Z / Ctrl+Y), `toggle_schema` (Ctrl+K n, F2), `open_connection`
(Ctrl+K o, Ctrl+O), `disconnect` (Ctrl+K d, Ctrl+D), `new_tab` (Ctrl+T),
`close_tab` (Ctrl+W), `next_tab` / `prev_tab` (Ctrl+PageDown /
Ctrl+PageUp), `save_tab` (Ctrl+S), `rename_tab` (Ctrl+K t, F4), `library`
(Ctrl+K l, F3), `commit` / `rollback` (F8 / Shift+F8, Ctrl+K c / r),
`find` (Ctrl+F), `comment` (Ctrl+K -, Ctrl+/, Ctrl+7), `format` (Ctrl+K f,
F9), `palette` (Ctrl+K), `explain_analyze` (Shift+F7, Ctrl+K a),
`connection_info` (Ctrl+K u), `activity` (Ctrl+K s), `heartbeat`
(Ctrl+K h), `insert_snippet` (Ctrl+K i, Ctrl+E), `menu` (Ctrl+K m,
Shift+F10, F10), `zoom` (Ctrl+K z, F12), `projects` (Ctrl+K p),
`transaction_mode` (Ctrl+K x), `next_warning` (Ctrl+K w). Keys are written like `F5`, `Ctrl+R`,
`Ctrl+Shift+PageUp`, `Ctrl+Space`; `Ctrl+K x` is a letter in the Ctrl+K
menu. Unknown actions or keys are reported at startup, and the help screen
(`F1`) shows the keys in use.

Colours follow the terminal: 24-bit where supported (iTerm2, kitty, WezTerm,
Ghostty, Alacritty, VS Code…), otherwise the nearest of the 256 standard
colours (e.g. macOS Terminal).

### Files

Everything lives in the `podelis` folder of your OS config directory (e.g.
`~/Library/Application Support/podelis` on macOS, `~/.config/podelis` on
Linux), or in `$PODELIS_CONFIG_DIR` if set:

- `connections.toml` — projects and their connections. If it can't be
  parsed, it is moved aside to `connections.toml.bak-<timestamp>` rather
  than overwritten.
- `history/<project-id>.jsonl` — each project's query history: the last
  500 queries, up to 16 MB in all (a query over 1 MB isn't kept).
- `drafts/<project-id>.json` — each project's open tabs, their text,
  connections and cursors, and the rest of the session (written a moment
  after you stop typing, so a crash loses little).
- `queries/<project-id>.json` — each project's saved-query library.
- `config.toml` — settings (optional, never written by the app).
- `podelis.log` — only with `PODELIS_LOG=debug` (or another
  [filter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
  like `podelis=trace`): what the app and the database driver did,
  **including the SQL run**. Off by default.
- `crash.log` — if Podėlis ever crashes, what happened (with a backtrace);
  please attach it to a bug report. A crash in a background task is logged
  too and shown in the status bar, without taking the app down.

Files are created readable only by you (they can contain SQL and
connection details).

Passwords are stored in the OS keychain under the service `podelis`, keyed
by project id and connection name, so renaming a project doesn't affect
them. A connection with a password command keeps no password there. On
Linux any Secret Service works: gnome-keyring, KWallet, KeePassXC (with its
Secret Service integration on) or oo7; `secret-tool search service podelis`
lists what's stored.

Upgrading from a version without projects: existing connections (and query
history) are moved into a project named `Default` on first start. Their
saved passwords keep working, and move to the new keychain key the next
time you save the connection with `Ctrl+S` or move it with `m`.

## Development

```sh
cargo install cargo-deny --locked   # once: check.sh requires it
./scripts/check.sh   # rustfmt, clippy (pedantic, warnings as errors), tests, cargo-deny
```

How the code is organised — the app/UI/database layers, and the database
adapters (one folder per engine, a `Dialect` and capabilities per engine) —
is in [docs/architecture.md](docs/architecture.md), including how to add an
engine.

Run `./scripts/install-hooks.sh` once to have git run `check.sh` before
every push (skip once with `git push --no-verify`).

Besides unit, snapshot and live-database tests, property tests
([proptest](https://crates.io/crates/proptest), `src/fuzz_tests.rs`) feed
random SQL-ish text to the lexer, splitter, analyzer, completion, parameter
handling and editor; failures they find are saved in `proptest-regressions/`
and re-run first.

The Rust version is pinned in `rust-toolchain.toml` (rustup installs it on
first use), so local builds and CI see the same lints. Lints are configured
in `Cargo.toml` (`[lints]`), formatting in `rustfmt.toml`, and
licence/advisory policy in `deny.toml`.

GitHub Actions, kept small to save minutes:

- `ci.yml` — on pushes to master (not docs-only ones): format,
  clippy and tests on Linux against real PostgreSQL and MySQL; a newer push
  cancels the older run.
- `platforms.yml` — tests on macOS weekly, on release tags, or
  by hand (Actions → Platforms → Run workflow).
- `deny.yml` — `cargo deny` when dependencies change, and weekly.
- `release.yml` — on a `v*` tag, builds binaries for Linux and macOS and
  attaches them to the release; `packaging/homebrew/podelis.rb`
  is a formula template for a Homebrew tap.

What the automated tests can't cover (real terminals, remote servers, big
tables) is in [docs/manual-testing.md](docs/manual-testing.md): data sets
to test with, from generated to public servers with real data, and a
checklist to go through before a release.

UI rendering is covered by snapshot tests (`src/ui/snapshots/`). After an
intended UI change, review and accept them with `cargo insta review` (or
rerun with `INSTA_UPDATE=always` and check the diff).

SQLite tests always run. Tests against other live services are opt-in:

| Variable | Enables |
|---|---|
| `PODELIS_TEST_PG_PORT=5432` | PostgreSQL tests (also `_HOST`, `_USER`, `_PASSWORD`, `_DB` with the same prefix; defaults `127.0.0.1`, `postgres`, empty, none) |
| `PODELIS_TEST_MYSQL_PORT=3306` | MySQL tests (same extra variables; default user `root`). Creates and drops `podelis_test` and `podelis_ro` databases |
| `PODELIS_TEST_SSH_DEST=host` | SSH tunnel test: reaches the MySQL (or Postgres) test server through `ssh` to this host |
| `PODELIS_TEST_KEYCHAIN=1` | Round-trips throwaway entries through the real OS keychain |

## Licensing

Copyright (C) 2026 Taislius <taislius@proton.me>.

Licensed under the [GNU Affero General Public License v3.0](LICENSE)
(`AGPL-3.0-only`). Pull requests aren't accepted, but bug reports and
ideas are welcome (see [CONTRIBUTING.md](CONTRIBUTING.md)).

Release archives include `THIRD-PARTY-LICENSES.txt` with the licences of
the bundled crates (`./scripts/third-party-licenses.sh` generates it; needs
`cargo install cargo-about --features cli`).
