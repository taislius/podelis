# Podėlis for AI agents

[← Podėlis](../README.md)

Let your coding agent use the databases you already use in Podėlis: it
gets the schema and runs SQL through the same saved connections, never
sees a password, can't drop a table, and you can see afterwards everything
it ran.

## Share a connection

Agents see only the connections you share with them. In the connection's
settings (`F11`, or Ctrl+K p), set **AI agents**:

| Setting | What agents may do |
|---|---|
| not shared | Nothing: they don't see the connection (the default) |
| read | Read. Each call runs in a read-only transaction on the server, so even a write hidden in a function is refused |
| read and write | Write too, but never `DROP`, `TRUNCATE` or `UPDATE`/`DELETE` without `WHERE`. On a **production** connection every call is rolled back: a write only runs as a dry run (reporting how many rows it would change), and schema changes not even that |

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
even as a dry run. Calls that reach past the data are refused too: files
on the server (`INTO OUTFILE`, `pg_read_file`, `lo_export`), other
sessions (`pg_terminate_backend`), locks that outlive the transaction, and
other servers (`dblink`). That list is a safety net, not a boundary:
**connect as a database user with only the rights agents should have**
(for reading, `GRANT SELECT` on what they need, no superuser or `FILE`
privilege). Podėlis's rules then sit on top.

## `podelis mcp`

An MCP server over standard input and output, serving the shared
connections of a project (the last one opened, or `-p <project>`). Add it
to your agent; for Claude Code:

```sh
claude mcp add podelis -- podelis mcp
claude mcp add podelis-shop -- podelis mcp -p shop   # another project
```

Other agents take the same command, `podelis mcp`, in their MCP settings.

The agent gets these tools:

| Tool | What it does |
|---|---|
| `connections` | The shared connections: name, engine, production or not, what agents may do |
| `schema` | Tables and views with their columns and primary keys (filter by name on big databases) |
| `describe_table` | One table's columns, keys, indexes, foreign keys and (for a view) definition |
| `query` | Runs SQL; rows as JSON, 100 by default and at most 1000 |
| `explain` | One statement's plan without running it, and what stands out (as F7 shows) |

Every query has the connection's timeout, or 30 seconds without one, and
an answer is capped (long values are cut, with a note). Podėlis tells the
agent that query results are data, not instructions: text in a table can
be written to steer an agent.

## What agents ran

Everything an agent runs through `podelis mcp`, and everything refused, is
logged with the agent's name, the connection, the SQL and how it went
(rows, rows changed, time, the error or why it was refused). The log is in
`agents/<project>.jsonl` in Podėlis's folder (see
[Configuration](configuration.md)), private to you like the history, and
a statement that sets a password is left out of it.

The **query library** (F3) shows it: after your saved queries, newest
first, with what's worth a look marked in orange: refused, failed, dry
runs, rows changed, and lint warnings such as `= NULL`. Type to search by
SQL, agent or connection; `Enter` opens the SQL in a tab named for the
agent, where `F7` shows its plan. The tab runs it on its own connection,
not necessarily the one the agent used: the status line says which that
was.

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
