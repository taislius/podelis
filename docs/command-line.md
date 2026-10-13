# Podėlis without the UI

[← Podėlis](../README.md)

Podėlis also runs from the command line with no UI. It uses the same saved
connections, keychain passwords, connection modes and timeouts as the app.
It prints results to stdout and messages to stderr, and its exit code
tells a script what happened. The same reference is in `man podelis`
(installed by Homebrew; from a checkout,
`man ./docs/podelis.1`) and, in short, in `podelis --help`.

## Run SQL: `-e`

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

## Load a file: `--import`

```sh
podelis app.db --import users.csv                  # into the table "users"
podelis prod --import export.tsv --table events -y # into "events", on a production connection
podelis app.db --import title.basics.tsv.gz        # compressed, into "title_basics"
podelis app.db --import data.txt --delimiter '|'   # another separator
```

`--import file.csv` creates the table (named after the file, or
`--table`) if it doesn't exist, and appends to it if it does. Each new
column gets the narrowest type its values fit: integer, decimal, boolean
(`true`/`false`), date (`2026-10-13`), timestamp (`2026-10-13 10:00`, with
seconds and fractions or a `T`) or text. A timestamp with a time zone
(`…Z`, `…+02:00`) is `TIMESTAMPTZ` on PostgreSQL and stays text on MySQL
and SQLite, which would drop the offset. Numbers with leading zeros
(`007`) stay text. It all happens in one transaction, so a bad line changes
nothing (on MySQL, a newly created table stays), and the error names the
line. PostgreSQL loads the rows with `COPY`, which is many times faster.
On a terminal, a line shows how far it's got (`loading 45% · 2.7M rows`);
the file is read twice, once to guess the types and once to load it.

The first line must name the columns, and an empty field is NULL. `.tsv`
and `.tab` files are tab-separated and not quoted: a `"` is part of the
value, and `\N` is NULL, as in IMDb's files and PostgreSQL's `COPY`
output. Other files are separated by whichever of `;`, tab and `,` their
first line has most of; `--delimiter` sets it instead (one character, or
`tab`). Files separated by `;` usually come from where `,` is the decimal
mark (Excel in Lithuanian, German…), so there `12,50` is read as 12.50. A
name ending in `.gz` is read compressed, as published data sets often
come. The file must be UTF-8; convert others first, e.g. `iconv -f
windows-1257 -t utf-8 old.csv > new.csv`.

Podėlis talks to databases and leaves files to the tools made for them:
query CSV/JSON files with [Miller](https://miller.readthedocs.io), `jq`,
`xsv` or [VisiData](https://www.visidata.org), and pipe Podėlis's output
into them.

## Check connections and your setup

```sh
podelis --test prod                       # connect step by step, show where it fails
podelis doctor                            # config, keychain, password managers, ssh, terminal, viewer, crash log
podelis doctor -p shop --connections      # …and test every saved connection of a project
```

## Exit codes

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

## Safety rules

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

## On servers and in CI

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
