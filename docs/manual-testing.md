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
  once, scrolling loads more, `M` loads more at once, and memory stays
  reasonable.
- [ ] Run another query while rows are still streaming: the stream is
  cancelled on the server (check with the activity monitor, `Ctrl+K s`).
- [ ] Sort (`s`), filter (`/`), export (`e` / `J` / `i`), pipe through
  `sort | uniq -c` (`|`) and open in the viewer (`o`) on a large result.
- [ ] A wide table (many columns): sideways scrolling, and the record view
  (`x`).
- [ ] Over a slow link (a public server): the UI stays responsive while a
  query runs, and `Esc` cancels it.

### Scripts and result sets

- [ ] Run `select count(*) from a; insert …; select * from b where false;
  select 1` as one script: `RESULT 3/3` shows, and `<` / `>` (or clicking
  the RESULT tab) step through the three sets. The empty one shows its
  columns.
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

### Writes and safety

- [ ] On a production connection, an `update` asks first; `t` runs it in a
  transaction to review, then F8 commits or Shift+F8 rolls back.
- [ ] A read-only connection refuses writes, and the server enforces it too.
- [ ] Transaction modes (`Ctrl+K x`): manual commit holds writes until F8;
  rollback-only shows how many rows changed and rolls them back.
- [ ] Quitting or disconnecting with a transaction open asks first.
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
  (`/`) quickly; expanding a table shows indexes and foreign keys; `d`
  shows definitions.
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
- [ ] Follow a foreign key (`f`) on the employees database.
- [ ] The activity monitor (`Ctrl+K s`): sessions show, and cancelling or
  ending one works.
- [ ] Connection info (`Ctrl+K u`).

### Terminals

Run the basics (connect, run, scroll, a popup, the mouse, copying with
`y`) in each:

- [ ] macOS Terminal, iTerm2, WezTerm, Ghostty, kitty, Alacritty
- [ ] A Linux desktop terminal
- [ ] Inside tmux, and over SSH (clipboard via OSC 52)
- [ ] A narrow window (80 columns or less): the navigator hides and nothing
  overlaps; resize while a query runs
- [ ] `theme = "light"` on a light background; `editor_mode = "vim"`
