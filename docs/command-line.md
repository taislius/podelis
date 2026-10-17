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
(an array of objects, one per row, values as strings or `null`). CSV
follows RFC 4180: lines end in CRLF, and a field with a comma, quote or line
break is quoted (`NULL` is an empty field). TSV keeps
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
podelis doctor                            # config, keychain, password managers, ssh, terminal, viewers, crash log
podelis doctor -p shop --connections      # …and test every saved connection of a project
```

## Ask in plain language: `ask`

```sh
podelis ask prod "revenue per country last month"           # prints SQL; runs nothing
podelis ask -p shop --model local prod "top 10 products"    # a project, and a model by name
podelis ask prod "orders per day this week" > orders.sql    # stdout has only the SQL
```

An AI model (set up in `config.toml`: [AI models](configuration.md#ai-models))
writes one query for the question over the connection's schema, and
`ask` prints it (in the app, `Ctrl+K q` puts it in the editor). It's never run: read it, then run it as you like
(`podelis prod -e - < orders.sql`, or paste it in the editor). What the
model said about it (what it does, what it assumed) and how many tokens
it took go to stderr.

- **The model sees only what it needs:** the tables that matter for the
  question and their columns, keys and comments, never rows (what
  `podelis gateway context` shows, below).
- **Its SQL is checked against the schema.** A table or column that isn't
  there goes back to the model with what's wrong, up to twice; any still
  unknown are named ("Check before running it: …").
- **Your schema stays on your machine unless you say so.** A project
  shares its schemas only with models on this machine (a local server)
  until you allow more: asking a model at a provider asks first, in the
  terminal, whether to send the table and column names this once or
  always for the project (`share_schema = "all"` on the project in
  `connections.toml`; remove it to go back). Without a terminal it isn't
  sent, and `ask` exits with 3.

Exit codes: 0 SQL printed, 1 no SQL (the model said why), 3 the schema
wasn't sent.

## What your team knows: `memory`

Models write wrong SQL when a column means something they can't see:
`status = 3`, a total with VAT, revenue net of refunds. Tell the project's
memory once, and every model asked about those tables is shown it:

```sh
podelis memory add orders.status 3 = cancelled
podelis memory add revenue is orders.total minus refunds    # is / are / means / equals say how it relates
podelis memory add -p shop customers.tier gold spends over 1000 a year
podelis memory                       # every fact, numbered (podelis memory list refunds: those matching)
podelis memory forget 2
podelis memory learn db/migrations   # what your migrations say: each table's history, the comments above statements
podelis memory confirm 7             # a model's assumption (ask says which) that's right
podelis memory export > memory.txt   # as text: read it, review it, keep it in git
```

**Your migrations know a lot.** `learn` reads a folder of `.sql`
migration files (any tool's: Flyway, Rails, Django, golang-migrate…, in
file-name order) without a model: when each table was created and each
column added, dropped or renamed, dated from the file names
(`20240301_add_refunds.sql`), the `--` comment right above a statement
("Refunds used to be negative orders") as a note on that table or column,
and `COMMENT ON`. Run it again after new migrations: it replaces what it
learned from that folder before. What people said goes first when there's
no room for everything, then the notes, then the histories.

**A model's assumptions wait for you.** When a model had to assume what a
value means ("0 for free orders") and no fact said it, `ask` keeps that
as a fact that isn't confirmed (never shown to models), and says so:
`podelis memory confirm <number>` makes it count, `forget` drops it (and
the same suggestion isn't kept again).

A fact goes to a model with the tables it's about (`orders`,
`orders.status`, `public.orders` and `public.orders.status` all go with
`orders`) or when it matches the question's words, labelled as what the
team says; `podelis gateway context` shows which ones. Facts you add, and
those from your migrations, are confirmed; only confirmed ones are shown.
In the app, `Ctrl+K k` shows them. They're kept per project in `memory/<project-id>.db`, and
the gateway's log never has their text.

## The gateway

The gateway is the background process for what involves AI models:
the models and their keys, and later what they're shown, their tools and
memory. Features that need it start it, and it stops by itself after
`gateway_idle` minutes with no client (see [Configuration](configuration.md)),
so it never has to be started by hand; a gateway left running by an older
Podėlis version (after an update) is stopped and replaced by the next use
(a newer version's is left alone: two installed versions don't stop each
other's), and `podelis gateway stop` stops either. It listens only on a socket in
the config folder that only you can open. The models it may use are set
up in `config.toml` ([AI models](configuration.md#ai-models)); the
features that ask them come next.

**What a model is shown of a database.** Not the whole schema, and never
rows: the tables whose names, column names or comments match the
question's words, then the tables a foreign key joins to them (one that
links two of them first), with their columns, keys, where foreign keys
point, and the database's own comments (`COMMENT ON` in PostgreSQL,
`COMMENT '…'` in MySQL), within about 4,000 tokens. `podelis gateway
context` shows exactly that, with why each table is there, so you can see
it before anything is sent anywhere; `-p` picks the project.

```sh
podelis gateway models                    # the models set up, and where each one's key comes from (without running a key_command)
podelis gateway key claude                # store a model's API key in the OS keychain (asked, not shown)
podelis gateway test                      # ask the default model for one word (costs a few tokens)
podelis gateway test local                # …or a model by name
podelis gateway context prod "revenue per product in September"   # what a model would be shown of prod's schema (sends nothing)
podelis gateway status                    # does it run? (exit 1 if not)
podelis gateway start                     # start it in the background
podelis gateway stop                      # stop it
podelis gateway                           # run it in the foreground (Ctrl+C stops it)
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
