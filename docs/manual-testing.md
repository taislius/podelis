# Manual testing

The automated tests (`./scripts/check.sh`) cover the logic and the drawing,
but not real terminals, real servers over a real network, or tables with
millions of rows. Before a release, go through the checklist below against
some of the data sets here.

## Test data

### Generated: any size, in seconds

**PostgreSQL: pgbench** comes with Postgres:

```sh
createdb bench
pgbench -i -s 100 bench   # pgbench_accounts: 10M rows (scale 1 = 100k), ~1.5 GB
```

`pgbench_accounts` has only a primary key, so filtering on any other column
is a big sequential scan.

**PostgreSQL: `generate_series`**

```sql
create table events as
select g as id, now() - g * interval '1 second' as at,
       md5(g::text) as payload, (random() * 1000)::int as user_id
from generate_series(1, 5000000) g;
```

**MySQL: sysbench**

```sh
sysbench oltp_read_only --mysql-user=root --mysql-db=bench --table-size=10000000 prepare
```

**SQLite: a recursive CTE**

```sql
create table big as
with recursive n(i) as (select 1 union all select i + 1 from n where i < 5000000)
select i as id, hex(randomblob(16)) as payload, abs(random()) % 1000 as user_id from n;
```

**TPC-H** (`dbgen`): the standard benchmark schema, for any engine. At
scale factor 1 it's about 1 GB, with 6M rows in `lineitem`, and its joins
give interesting plans.

**MySQL: employees** (github.com/datacharmer/test_db): `mysql < employees.sql`
loads about 300k employees and 2.8M salaries, with real foreign keys.

### Public servers with real data

Read-only servers that scientific projects run for anyone to query. They're
also how to test over a slow, remote link. The logins are the ones the
providers publish; if one fails, check their documentation, since they
change now and then. Keep queries reasonable: these servers are shared, and
some cancel long queries.

| Database | Engine | Connect |
|---|---|---|
| RNAcentral (RNA sequences, tens of millions of rows) | PostgreSQL | `podelis postgres://reader:NWDMCE5xdipIjRrp@hh-pgsql-public.ebi.ac.uk:5432/pfmegrnargs` |
| Ensembl (genomes, hundreds of databases) | MySQL | `podelis mysql://anonymous@ensembldb.ensembl.org:3306`, then `SHOW DATABASES` and `USE` one |
| UCSC Genome Browser | MySQL | `podelis mysql://genome@genome-mysql.soe.ucsc.edu/hg38` |
| Rfam (RNA families) | MySQL | `podelis mysql://rfamro@mysql-rfam-public.ebi.ac.uk:4497/Rfam` |

### Real data to load yourself

For writes, transactions and comparisons, which the public servers don't
allow:

- **MusicBrainz**: the music encyclopedia as official PostgreSQL dumps,
  tens of GB with real foreign keys. The best real Postgres database to test
  against.
- **Stack Exchange data dump** (archive.org): posts, users and votes as XML.
  Start with a small site; Stack Overflow itself is hundreds of GB.
- **OpenStreetMap**: country extracts from Geofabrik, loaded with
  `osm2pgsql`.
- **IMDb datasets** (datasets.imdbws.com): TSV files with up to tens of
  millions of rows. Load them with `podelis --import`, which tests the
  import too.
- **NYC taxi trips** (NYC TLC): hundreds of millions of rows as Parquet or
  CSV; load one month.
- **Wikipedia dumps** (dumps.wikimedia.org): `page`, `categorylinks` and
  more as MySQL dumps; a small-language wiki is a manageable size.
- **data.gov.lt**: Lithuanian open data, much of it CSV for `--import`.

## Checklist

Each item says what to do and what should happen. Note the terminal, the
OS and the server version when something is off.

### Connecting

- [ ] Connect with a URL from the command line, with a saved connection
  (`podelis -p <project> <name>`) and to a SQLite file.
- [ ] `Ctrl+K p` (or a click on the project's name): projects, connections
  and settings side by side. `→`/`←`
  move between the panes, the settings follow the selected connection,
  `Enter` on a connection connects (a new tab if this one is connected),
  `e` edits it in place, `Esc` goes back to the tabs unchanged. At 80
  columns two panes show, and they don't overlap.
- [ ] Paste a URL into the connection form: the fields fill in.
- [ ] `podelis --test <name>` and `Ctrl+T` in the connection form: each step
  (SSH, DNS, TCP, login, TLS, latency) with its timing. With a wrong
  password or host, the failing step comes with a hint.
- [ ] Through an SSH tunnel.
- [ ] Quit with tabs on two connections and start again: tabs, connections,
  cursor positions and expanded tables come back.
- [ ] `podelis doctor`.

### Big results

- [ ] `select * from pgbench_accounts` (10M rows): the first rows show at
  once, scrolling loads more, `M` loads more at once (the first new row is
  selected, so they're in sight, sorted or filtered too; `M` while a page
  is coming says so, and its rows follow that page), and memory stays
  reasonable.
- [ ] Run another query while rows are still streaming: the stream is
  cancelled on the server (check with the activity monitor, `Ctrl+K s`).
- [ ] Sort (`s`), filter (`/`), export (`e` / `J` / `i`), and pipe through
  `sort | uniq -c` (`|`) on a large result.
- [ ] Open the rows (`o`) as each format, on macOS and on Linux: CSV in a
  spreadsheet, JSON and SQL in an editor, each in its own window while
  Podėlis keeps the terminal; a `[viewers]` entry with `terminal = true`
  (`vd`) runs in the terminal and Podėlis comes back after it. A program
  that isn't installed says so in the status bar; `podelis doctor` lists
  the viewers.
- [ ] The ASCII table (`+--+`): from `export ▾` to a `.txt` file, copied
  (menu, Ctrl+K "Copy the rows as an ASCII table") and opened (`o`), it
  lines up with Lithuanian and wide (CJK) text, `NULL`s say so, a cell
  with line breaks (or a log's ANSI colours) stays one line and lines up,
  and a huge JSON cell is cut with `…` at
  120 columns (a long column name stays whole). It opens with `[viewers] ascii`, else with what `txt` uses.
  "Export all rows" doesn't offer it.
- [ ] A wide table (many columns): sideways scrolling, and the record view
  (`x`).
- [ ] Over a slow link (a public server): the UI stays responsive while a
  query runs, and `Esc` cancels it. Zoomed (`Ctrl+K z`, or `Shift+F12` on
  the statement), the first `Esc` leaves the zoom and the status bar says
  the query still runs; the second cancels it.
- [ ] A dropped link (turn the VPN off, or `docker pause` the server's
  container): after a minute idle, F5 or F7 fails in about 5 s with "the
  server doesn't answer" and *connection lost* in red; during a query,
  `Esc` says the cancel didn't get through (orange) and `Esc` again stops
  waiting. Back online, the next run reconnects and the dot is green.
- [ ] *Connection lost*, then the server back without touching anything:
  within a few seconds the dot is green and the status bar says "The
  connection is back". A server stopped when Podėlis starts (the session
  reopened): F5 once it's up connects and runs, no restart needed.
- [ ] A server that's down: connecting to PostgreSQL and to MySQL says
  "can't reach …: connection refused" at once, not "pool timed out" after
  10 s (MySQL's `performance_schema.host_cache` shows no handshake errors
  afterwards).
- [ ] A long query in one tab, another tab (another connection) shown:
  `Esc` there doesn't stop it, it says "Tab “…” on <connection> is running a
  query: Alt+N goes to it, Esc there stops it" (N: that tab's number); in its own tab `Esc` stops it (MySQL: no
  "Unknown thread id").

### Scripts and result sets

- [ ] Run `select count(*) from a; insert …; select * from b where false;
  select 1` as one script: `RESULT 3/3` shows, and `<` / `>` (or clicking
  the RESULT tab) step through the three sets. The empty one shows its
  columns.
- [ ] Run three queries one after another, one of them failing: `#1`
  and `#2` show above the rows with the statement and `lines …`, and the
  failed one says why where its rows would be (the next run takes its
  place). `,` / `.` and clicking a number switch; clicking `lines …`
  selects the statement in the editor. On an earlier result, `c` edits
  its own table and `M` re-runs its own query (with its own `:param`
  values). `p` pins one: it stays after ten more queries.
- [ ] `podelis <conn> -e "select 1; select 2"` prints both tables; with
  `-f json` two arrays (`| jq -s length` says 2); with `-f csv` only the
  last one, and stderr says one earlier set was left out.
- [ ] Exit codes (`echo $?`): a SQL error 1, an unknown saved connection
  2, `drop table t` without `-y` 3, a host that doesn't answer 4, `select
  pg_sleep(…)` past the connection's timeout 5.

### Errors, hints and lint

- [ ] Paste a multi-megabyte script (a dump's `INSERT`s): typing, moving
  the cursor and scrolling stay smooth at the top and at the bottom, and
  hints still appear after a pause.

- [ ] A typo in a table name: after a pause the server's error is
  underlined (`SERVER`). Inside an open transaction, the parser's hint
  (`HINT`) is shown instead.
- [ ] `select * from t where x = null`, `delete from t`,
  `select * from a, b`, `… not in (select …)`,
  `… = (select id from t limit 1)`: each gets a `LINT` warning that points
  at the right words. `lint = false` turns them off.
- [ ] Several of them in one script: the label reads `LINT 1/3`, and
  `Ctrl+K w` moves the cursor to each in turn, back to the first after
  the last.
- [ ] MySQL: `select 'a' || 'b'` gets a `pipes-or` warning and returns `0`,
  as in the `mysql` client. After `set sql_mode = 'ANSI'` and a pause, no
  warning, and it returns `ab`.
- [ ] Confirmations: `with d as (delete from t returning *) select * from d`
  (PostgreSQL) asks first; `explain delete from t` doesn't;
  `alter table t alter column c drop default` doesn't;
  `alter table t drop column c` does.

### Query plans

- [ ] F7 on `select * from events where user_id = 5`: the findings at the
  top point (`◀ 1`) at the sequential scan. After
  `create index on events (user_id)`, F7 again: the scan finding is gone.
- [ ] Shift+F7 (EXPLAIN ANALYZE) on a join of big tables: the slowest step
  with its share of the time. After a big `insert` without `ANALYZE`, a row
  estimate far off the actual count.
- [ ] A sort bigger than `work_mem` (`set work_mem = '64kB'`, then sort a
  big table): "Sort spilled to disk".
- [ ] MySQL: F7 and Shift+F7 on a full table scan of a big table.
- [ ] SQLite: F7 on a join without an index on the inner table: "Full scan
  of … for every row of the outer loop".
- [ ] A long plan: click a line, drag past the bottom (it scrolls, and stops
  at the last line), scroll back up, `y`: every selected line is copied, the
  ones out of sight too; `Shift+↓` extends, `Esc` lets go, `Y` copies all.

### Writes and safety

- [ ] On a production connection, an `update` asks first; `t` runs it in a
  transaction to review, then F8 commits or Shift+F8 rolls back.
- [ ] A read-only connection refuses writes, and the server enforces it too.
- [ ] Transaction modes (`Ctrl+K x`): manual commit holds writes until F8;
  rollback-only shows how many rows changed and rolls them back.
- [ ] Quitting or disconnecting with a transaction open asks first.
  Disconnecting (`Ctrl+K d`) always asks, in orange when it would roll back
  a transaction or cancel the query running in that tab (and says so).
- [ ] Edit a cell (`c`), add a row (`a`), delete marked rows (`d`) on a
  table with a primary key.
- [ ] A connection's query timeout cancels a long query on the server.

### Comparing connections

Use two databases with the same tables, e.g. `pgbench -i -s 10 a` and
`pgbench -i -s 20 b`, then change one: add a column, an index and a table.

- [ ] Open both in tabs (`Ctrl+O`), then Ctrl+K and type "compare":
  "Compare the schema with …" lists what's only on one side and what
  differs, with the SQL to align the other side. `y` copies it; `e` opens
  it in a tab on the other connection, where running it makes the schemas
  match (compare again: they match).
- [ ] "Compare the query's rows with …" on `select * from pgbench_branches`
  after updating a row on one side: the changed pair (`≠`), and rows only
  on one side (`−` / `+`).
- [ ] Comparing a write is refused.
- [ ] A schema with hundreds of tables (MusicBrainz): how long the
  comparison takes.

### Long-query notification

- [ ] Run `select pg_sleep(15)` (MySQL: `select sleep(15)`) and switch to
  another window: when it ends, the terminal rings and shows a desktop
  notification. Staying in the terminal: no notification.
- [ ] Try it in each terminal below; note which show the notification.

### Schema navigator and server tools

- [ ] A big catalog (Ensembl, MusicBrainz): the navigator loads and filters
  (`/`) quickly, the best matches first (`produc`: the names starting with
  it, `production_log` and `products`, then those with a word starting
  with it, `alternative_produce`, then the rest); expanding a table shows
  indexes and foreign keys; `d` shows definitions.
- [ ] SQLite: a mistyped file path in the form says it doesn't exist and
  creates nothing; Enter again creates and opens it. `podelis typo.db -e
  "select 1"` refuses without `--create` (`--yes` isn't enough). `Ctrl+T` on
  a missing file creates nothing. Delete a database file a session uses and
  restart: "doesn't exist (moved or deleted?)", and no empty file appears.
- [ ] System tables (`.` in the navigator): a SYSTEM section with
  `pg_catalog.*` and `information_schema.*` (PostgreSQL), the four system
  databases (MySQL) or `sqlite_schema` (SQLite); `.` again hides it. On a
  big PostgreSQL catalog, note how long loading takes with them shown;
  completion offers them only while shown.
- [ ] Follow a foreign key (`f`) on the employees database: the row shows
  in a pop-over (also from a join of two tables), `e` opens it in a tab;
  on a column that isn't one, the message lists those that are.
- [ ] A script of several statements: `Ctrl+K v` (and right-click →
  *Select the statement*) selects the one under the cursor or pointer;
  `Shift+F12` shows it alone, `F5` there runs only it, moving out of it
  shows the next, `Esc` returns. Run the script: the line above each result
  set shows the statement that made it, and a click there selects only
  that statement. Drag a selection up past the editor's top edge: it
  scrolls up and keeps selecting (past the bottom, down), and goes on
  scrolling with the pointer held still there. The mouse wheel
  scrolls the editor
  without losing a selection (click the statement line above a plan, then
  scroll).
- [ ] The activity monitor (`Ctrl+K s`): sessions show, and cancelling or
  ending one works.
- [ ] Connection info (`Ctrl+K u`).

### AI models and the gateway

With a real key (a few cents) and, if you have it, Ollama:

- [ ] `podelis gateway models` lists the models and where each key comes
  from; `podelis gateway test` answers in seconds; a wrong key says "the
  key was refused".
- [ ] `podelis gateway context prod "…"` on a big schema (TPC-H, IMDb):
  the tables are the ones the question needs, with their joins, in a few
  thousand tokens.
- [ ] `Ctrl+K q` in the app: the answer's progress in the status bar, the
  SQL at the end of the editor under the question, not run; `F6` runs it.
  `Esc` while asking stops it; switching tabs meanwhile still puts the SQL
  in the tab asked from.
- [ ] A provider's model on a project that doesn't share its schema: the
  question to send it (`y` once, `a` always; `share_schema = "all"` saved).
- [ ] `podelis memory learn` on a real repository's migrations; a fact
  added with `podelis memory add` shows in `podelis gateway context` for a
  question about its table; a model's `-- assumed:` fact waits in
  `Ctrl+K k` (orange) until `c`.
- [ ] An agent through `podelis mcp` (Claude Code): the tools work, a
  write is approved in the app, Agent activity shows the session. With the
  gateway stopped first, the agent's first call starts it.
- [ ] Refusals say why, engine by engine: `PRAGMA table_info(t)` on SQLite
  ("describe_table tells…"), `SELECT load_file('/etc/passwd')` on MySQL
  ("reads a file on the server"), `SET …` anywhere (its own transaction),
  while a PostgreSQL column called `attach` reads fine.
- [ ] `podelis gateway status` after 30 min with nothing connected: not
  running.
- [ ] A typo in a `[[models]]` entry (`api = "ollama"`): `podelis gateway
  models` shows the parse error, not "no models are set up".

### Tabs

- [ ] A dozen tabs with long names: the active one shows its whole name (up
  to 40 characters), the others shortened; `‹N` and `›N` say how many
  don't fit (a click on one lists every tab, to search by name and pick
  from, as `Ctrl+K g` does), and the active tab
  is always in sight (Ctrl+PageUp/PageDown through them all). In a narrow
  window its name shortens too, and the `+` stays.

### Terminals

Run the basics (connect, run, scroll, a popup, the mouse, copying with
`y` and `Ctrl+C`, pasting with `Ctrl+V` and the terminal's paste) in each:

- [ ] macOS Terminal, iTerm2, WezTerm, Ghostty, kitty, Alacritty
- [ ] A Linux desktop terminal
- [ ] Inside tmux, and over SSH (clipboard via OSC 52)
- [ ] Run a slow query (`select pg_sleep(5)`): RESULT shows at once; the
  editor's title doesn't say running, the results pane does. Edit the SQL
  and press F7: the editor's footer says the plan follows, and it does when
  the query ends. Click the editor's tab, then right-click there: the
  editor's menu.
- [ ] Run `select pg_sleep(10)`, then Ctrl+T: the new tab works (run
  something else there: it says the other tab is running), the first tab's
  name shows `…`, and when it ends the status bar says so; its results are
  in its own tab. With a tab on another connection too. A write
  (`insert … ; select pg_sleep(5)`) keeps the tabs where they are.
- [ ] F7 on PostgreSQL/MySQL during a slow query: the plan shows at once.
  After `set search_path …` (or `use …`) it waits for the query instead.
- [ ] Navigator: `y` on a table copies `schema.table`; results menu "Copy
  rows as CSV"; vim mode: `yy` then Cmd+V elsewhere pastes the line.
- [ ] Copy and paste: in the editor `Ctrl+C` copies the selection, or the
  cursor's line with none, and never quits (`Ctrl+Q` does); in the results
  it copies the row, or the marked rows a line each. `Ctrl+V` pastes in
  the editor and the connection form. Right-click the selection: it stays,
  and the menu offers Copy, Cut and Paste; right-click the results on the
  MESSAGES tab: a results menu, not the editor's.
- [ ] MESSAGES: `↑` selects the newest message, `Enter` shows it whole
  (`y` there copies it), `y` / `Ctrl+C` copy it, `Y` copies all; a click
  selects one, a double click opens it.
- [ ] `export ▾` and `menu ▾` right of the results' tabs open their menus
  under them; the status bar shows `m  menu · right-click` (`^K m` in the
  editor), not while a filter is being typed.
- [ ] Navigator: `/`, type a few letters, then click a table and press
  `d`: its definition shows, the filter keeps its letters.
- [ ] Editor: `Ctrl+←` / `Ctrl+→` move by word without selecting
  (`Ctrl+Shift` selects), in each terminal above. If one doesn't,
  `PODELIS_LOG=podelis=trace` logs each key as the terminal sent it.
- [ ] A narrow window (80 columns or less): the navigator hides and nothing
  overlaps; resize while a query runs
- [ ] `theme = "light"` on a light background; `editor_mode = "vim"`
