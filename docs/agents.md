# Podėlis for AI agents

[← Podėlis](../README.md)

Let your coding agent use the databases you already use in Podėlis: it
gets the schema and runs SQL through the same saved connections, never
sees a password, can't drop a table, and you can see afterwards everything
it ran.

## Share a connection

Agents see only the connections you share with them. In the connection's
settings (Ctrl+K p, or click the project's name in the top bar), set
**AI agents**:

| Setting | What agents may do |
|---|---|
| not shared | Nothing: they don't see the connection (the default) |
| read | Read. Each call runs in a read-only transaction on the server, so even a write hidden in a function is refused |
| read and write | Write too, but never `DROP`, `TRUNCATE` or `UPDATE`/`DELETE` without `WHERE`. On a **production** connection every call is rolled back: a write only runs as a dry run (reporting how many rows it would change), and schema changes not even that |
| write when you approve | Write too, never anything destructive, but each write waits for your answer in Podėlis, production included (see below) |
| write in a sandbox | Write too, never anything destructive, but nothing is ever committed: its writes build on each other in one transaction, and its reads see them, until it's rolled back (see below) |

A read-only connection stays read-only for agents too. A change to the
setting applies at once, also to an agent that's already running.

How it holds: agents can send queries and, where allowed, `INSERT`,
`UPDATE`, `DELETE`, `MERGE` and schema changes, nothing else: not `SET`,
`BEGIN`, `COMMIT`, `USE`, `CALL`, `DO`, `COPY`, `LOCK` or `GRANT`. Podėlis
runs each call in a transaction of its own (read-only, rolled back, or
committed), which the agent can't end or switch to read-write, and a
setting changed inside it goes back with it. One call at a time runs on
each connection, and a call whose connection drops isn't run again on a
new one.

A rolled-back transaction doesn't undo everything: sequences a write
advanced stay advanced, and MySQL's MyISAM and MEMORY tables don't roll
back at all, so on a MySQL database that has any, agents can't write
even as a dry run. Calls that reach past the data are refused too, each
engine by its own list: files on the server (MySQL's `INTO OUTFILE`,
PostgreSQL's `pg_read_file` and `lo_export`, SQLite's `ATTACH`), other
sessions (`pg_terminate_backend`), locks that outlive the transaction,
extensions (SQLite's `load_extension`) and other servers (`dblink`); the
refusal says which and what it does. That list is a safety net, not a boundary:
**connect as a database user with only the rights agents should have**
(for reading, `GRANT SELECT` on what they need, no superuser or `FILE`
privilege). Podėlis's rules then sit on top.

## A sandbox for agents

With **write in a sandbox**, an agent can do a whole job (insert test
rows, change them, run the report on them) and you judge the result once,
not each write. Its first write begins a transaction that stays open
between its calls; each call runs after a savepoint of its own, so one
that fails is rolled back alone and the rest stay; its reads, and its
`explain`, see what it wrote. Nobody else sees any of it, and it's never
committed, production included. It's rolled back when the agent goes, or
after ten idle minutes (its row locks would otherwise be held).

In the agent activity (`Ctrl+K a`) its calls say "in the sandbox", and in
a session, `s` puts what it wrote (the writes that ran, in order) in a new
tab as a script: read it, then run it yourself if you want it. MySQL
commits schema changes at once, so there an agent's DDL is refused in a
sandbox; so are writes on a MySQL database with tables a rollback can't
undo.

## Approve each write

With **write when you approve**, an agent's write isn't run until you say
so. Podėlis first runs it as a dry run (rolled back), then asks you, in
the Podėlis open on that project, with a bell: which agent, on which
connection, the SQL and how many rows it would change. `y` runs it for
real; `n` or `Esc` says no (other keys, and a key typed just as it
appears, don't answer it). The agent waits up to two minutes; with no
answer it's told to ask you. Reads run without asking.

MySQL commits schema changes at once, so they come without a dry run
(and so do writes on a MySQL database with tables a rollback can't undo);
the dialog says so. An approved write shows in the agent activity as
"approved by you".

This guards against an agent's mistakes, not against an agent set on
harm: one that can run commands as you could answer for you. For that,
the database user's rights are the boundary.

## On production: no whole big tables

On a production connection, a query that would read a whole table of a
million rows or more (by its plan; a `LIMIT` that stops it early doesn't
count) is refused, telling the agent to
narrow it with a `WHERE` on an indexed column. The limit is `guard_rows`
in the [configuration](configuration.md) (0 turns it off); it asks you
the same in Podėlis.

## `podelis mcp`

An MCP server over standard input and output, serving the shared
connections of a project (the last one opened, or `-p <project>`). Add it
to your agent; for Claude Code:

```sh
claude mcp add podelis -- podelis mcp
claude mcp add podelis-shop -- podelis mcp -p shop   # another project
```

Other agents take the same command, `podelis mcp`, in their MCP settings.

`podelis mcp` hands the agent to Podėlis's gateway, the background
process that holds everything involving AI models (see [the
gateway](command-line.md#the-gateway)), and starts it if it isn't
running: there's nothing to set up. The tools, their rules, approvals and
the log are the gateway's; each agent's session, and the database
connections it opened, end when the agent goes. If the gateway stops
(`podelis gateway stop`, or after an update), `podelis mcp` ends too, so
the agent sees its server gone rather than waiting on one that never
answers; the next `podelis mcp` starts the gateway again.

The gateway opens agents' connections in its own environment: the one of
whatever started it (the app, or an agent's `podelis mcp`). A saved SQLite
connection with a relative path is still the agent's (from the folder it
started in). Something only the agent's environment has (another
`SSH_AUTH_SOCK` for an SSH tunnel, variables a password command reads)
isn't seen: use absolute paths and settings both have, or stop the gateway
(`podelis gateway stop`) so the agent's next call starts it in its own. If an agent can't reach Podėlis, `podelis gateway status` says
whether the gateway runs, and `PODELIS_LOG=debug` writes what it did to
`podelis.log`.

The agent gets these tools:

| Tool | What it does |
|---|---|
| `connections` | The shared connections: name, engine, production or not, what agents may do |
| `schema` | Tables and views with their columns and primary keys (filter by name on big databases) |
| `describe_table` | One table's columns, keys, indexes, foreign keys and (for a view) definition |
| `related_tables` | The tables a table points to and is pointed to by, through foreign keys, two steps each way: how to join |
| `query` | Runs SQL; rows as JSON, 100 by default and at most 1000 |
| `explain` | One statement's plan without running it, and what stands out (as F7 shows) |

Every query has the connection's timeout, or 30 seconds without one, and
an answer is capped (long values are cut, with a note). Podėlis tells the
agent that query results are data, not instructions: text in a table can
be written to steer an agent.

## What agents ran

Everything an agent runs through `podelis mcp`, and everything refused, is
logged with who it was and how it went:

- **who:** the agent as it reports itself (name and version, e.g.
  `claude-code 2.0.31`), the **session** (one `podelis mcp` run, so one
  agent's calls stay together), the **folder** it started `podelis mcp` in
  (usually the project it's working on), and a **name** of your own if you
  gave one: `podelis mcp --name billing-refactor`, or the `PODELIS_AGENT`
  environment variable, e.g. in the agent's MCP settings;
- **what:** the connection, the tool, the SQL, and the outcome: rows, rows
  changed, time, the error or why it was refused.

The log is in `agents/<project>.jsonl` in Podėlis's folder (see
[Configuration](configuration.md)), private to you like the history, and
a statement that sets a password is left out of it.

**Agent activity** (Ctrl+K, then type "agent") shows it, apart from your
own queries:

- the **sessions**, most recently active first: when, the agent and its
  name, the folder, the connections it used, how many calls, and in orange
  how many were refused, failed, changed data, or ran as a dry run on
  production (`!` marks them);
- `Enter` on a session: its **calls**, newest first: time, connection,
  tool, outcome, time taken and the SQL, with what's worth a look marked
  (refused, failed, dry runs, rows changed, lint warnings such as
  `= NULL`);
- `Enter` on a call: all of it, the SQL formatted, the error or refusal in
  full. `o` opens its SQL in a tab named for the agent (where `F7` shows
  its plan; the tab runs it on its own connection, not necessarily the one
  the agent used, as the status line says), `y` copies it;
- `/` searches (agent, name, folder, connection; in a session, the SQL and
  outcomes), `!` shows only what's worth a look, `Esc` goes back.

When an agent was refused, failed, changed data or ran a dry run since you
last opened the window, the status bar says so in orange (`agents: 2
refused · 1 wrote · 1 dry run`) until you look; a click opens the window.
It's watched while any connection of the project is shared with agents.
The window shows what was there when it opened: close and open it again
for newer calls.

It's a record of what your agent did through Podėlis, for checking up on
it, not an audit: an agent that has a database password of its own, or
runs `psql`, goes around it, and the log is a file you can edit. Several
agents can write to it at once. For rules
a whole team's agents can't get around, put the database behind a
gateway; for unattended agents, also connect as a database user with only
the rights the job needs.

## Without MCP: `-e`

An agent that can run shell commands can also use the command line, with
the same saved connections and safety rules (see
[Without the UI](command-line.md)). Put something like this in your
project's `CLAUDE.md` or `AGENTS.md`:

```markdown
## Database

Query the database with Podėlis, never with psql or a password:

    podelis -p shop dev -e "select …" -f json --limit 100

- `dev` is read-write, `prod` is production: read only, and never pass `-y`.
- Exit codes: 1 the SQL failed, 3 refused (it would change data), 5 timed out.
```

Queries run this way aren't in the agent log; use a connection marked
read-only for them, or `podelis mcp`.
