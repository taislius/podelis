# AI models

[← Podėlis](../README.md)

Podėlis can have an AI model write SQL from a question in plain language,
over the tables of a saved connection. It never runs what the model
writes: the SQL lands in the editor (or is printed) for you to read first.
Rows never go to a model, and your schema leaves your machine only if you
say so.

## Set up a model

Add one or more to `config.toml` (the first is the default):

```toml
[[models]]
name = "claude"
api = "anthropic"
model = "claude-opus-5-5"

[[models]]
name = "local"
api = "openai"                     # OpenAI's format: GPT, and Ollama, llama.cpp, LM Studio, vLLM
url = "http://localhost:11434/v1"  # Ollama on this machine: nothing leaves it
model = "qwen3:8b"
```

Then the key, for a provider's model (a local one needs none):

```sh
podelis gateway key claude      # asked for, not shown; kept in the OS keychain
pbpaste | podelis gateway key claude   # …or piped in, where there's no terminal to ask in
podelis gateway models          # what's set up, and where each key comes from (a key_command isn't run for this)
podelis gateway test claude     # one word back: the address and key work (a few tokens)
```

A `key_command` (`"pass show anthropic"`; what it prints is kept for 15
minutes, so your password manager isn't asked on every question) or
`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` work too. All the settings: [AI models in
Configuration](configuration.md#ai-models).

## Ask

- **In the app, in the editor among your SQL:** write the question as a
  comment that starts with `?` (or press `Ctrl+K q` on any other line:
  it starts one there), then `Ctrl+K q` with the cursor on it:

  ```sql
  -- ? revenue per city last month
  ```

  The SQL comes right under it, after a `-- ↳ <model>` line and the
  model's notes, and isn't run (`F6` runs it). Ask again, after changing
  the question, and that answer is replaced; SQL you wrote under a
  question yourself is never replaced. `--?` works too, and `/*? … */`
  holds a question over several lines. Being comments, questions are left
  alone by the server, lint and running, so a file of questions and their
  SQL is still SQL: save it to the library, keep it in git.
- **AI explain:** `Ctrl+K y y` on a statement: the model says in plain
  words what it does, in a pop-over, shown the same tables and facts as
  a question about it would be.
- **From the command line:** `podelis ask prod "revenue per country last
  month"` prints the SQL (`--model` picks another model, `-p` another
  project). Details in [Without the UI](command-line.md#ask-in-plain-language-ask).

Before you see it, the SQL's tables and columns are checked against the
real schema; what isn't there goes back to the model (twice at most), and
anything still unknown is marked `-- CHECK:`.

**When the tables shown aren't enough,** a model may look for more itself
("revenue" names no table, so `payment` isn't shown): `find_tables` with
words for what it needs, then `describe_table` for the ones that fit,
which lists a table as the context does (columns, keys, comments), never
its rows. At most two rounds and eight tables, then it must answer. What
it looked up is said with the answer (`-- it also looked at: payment`).
On by default for a provider's models, off for one on this machine
(small local models follow tool calls badly): `tools = true` or `false`
in its `[[models]]` entry. The same sharing rule applies: a provider's
model sees the schema only if the project shares it.

## What the model is shown

Only what the question needs: the tables whose names, columns or comments
match its words ("rented" finds `rental`; money words such as "revenue",
"earned", "spent" or "profit" find `amount`, `price` and `total` columns
and `payment` tables), the tables that join them (not a second way
between tables a key joins already),
their columns, keys, where foreign keys point, the database's comments,
and what your team has said about them (below). A fact that matches the
question brings the tables it names: with "revenue is payment.amount" in
memory, a question about revenue gets `payment`. A partitioned table is
shown once, as itself, not as each partition. A view comes after the
tables, and not just for having a table's name in its own (`film_list`
for "film") or for its columns alone. Never rows. `podelis
gateway context prod "your question"` shows exactly that, and sends
nothing. How it's chosen, in the research's terms (schema linking), and
why this way: [the context, in the field's
terms](architecture.md#the-context-in-the-fields-terms).

**Your schema stays on your machine unless you allow more.** A project
shares its table and column names only with models on this machine. The
first time a provider's model would see them, Podėlis asks: this once, or
always for the project (`share_schema = "all"` in `connections.toml`;
remove it to go back).

### What a model can see, engine by engine

On every engine, a model asked a question is shown: the question; the
chosen tables' and views' names; their columns, each with its type and
whether it may be null; primary keys and where foreign keys point; an
estimate of each table's rows; the database's comments; and your team's
facts about those tables. With AI explain (`Ctrl+K y y`), the statement
you selected, as you wrote it, so any value you typed into it goes too.
What differs is where those come from:

| Engine | A column's type, as shown | The row count | Comments | Reads your data? |
|---|---|---|---|---|
| PostgreSQL | The type's name, with its length or precision (`varchar(30)`, `numeric(10,2)`; an enum as its type's name, `mood`, not its values) | The planner's estimate (`reltuples`) | `COMMENT ON` tables, views, columns | No |
| MySQL | As MySQL writes it, with lengths, and an `ENUM` or `SET` column's allowed values (`enum('new','paid')`) | `information_schema`'s estimate (`table_rows`) | Table and column `COMMENT`s | No |
| SQLite | As declared | Only after `ANALYZE` (`sqlite_stat1`); otherwise none | None (SQLite keeps none) | No |
| DuckDB | As DuckDB writes it, an `ENUM`'s values included (`ENUM('happy', 'sad')`) | Its estimate (`estimated_size`) | `COMMENT ON` tables, views, columns | No |
| MongoDB | One per field, from its values' kind (`string`, `int`, `array`, `object`) | `estimatedDocumentCount` | None | **Yes, a sample**: 100 documents per collection, for their top-level field names; the values are dropped |

Never shown, on any engine: a row or a document's values, column
defaults, `CHECK` constraints, view definitions, indexes, users or grants,
passwords and connection details.

Two of these carry what people typed: comments (whatever their author
wrote) and, on MySQL and DuckDB, enum values (the allowed values, not
which rows have them). On MongoDB a collection keyed by data
(`{"ana@example.com": {…}}`) would show those keys, since they are its
field names.

### Why a row can't reach a model

Not a setting, and not a promise in a prompt: how the code is built.

1. **A question goes one way.** The app's and `podelis ask`'s questions go
   to the gateway as `ask`, with the question, the connection's name, the
   project and the folder: nothing from your results. The gateway is the
   only part that talks to models. (Its other model call, `complete`,
   sends a local client's own messages as they are; in Podėlis only
   `podelis gateway test` uses it, for one word.)
2. **For a question, the gateway reads only the catalog,** on a read-only
   connection of its own: `catalog()` (tables, columns, row estimates),
   `links()` (foreign keys) and `comments()`. On SQL engines that reads no
   table's contents. On MongoDB it samples documents for their field
   names, as above, and keeps no value.
3. **The model's SQL is never run:** it's checked against the schema and
   returned, for you to run.
4. **Its lookups (`find_tables`, `describe_table`) answer from that
   catalog,** never from the tables.

The same gateway runs agents' queries (below), but only for them, never
for a model's question.

**Tested, on every engine:**
- `the_catalog_holds_no_row_on_each_engine` (`crates/podelis-core/src/db/tests.rs`)
  puts a value no schema holds in a table on PostgreSQL, MySQL, SQLite and
  DuckDB, updates the engine's statistics, and fails if `catalog()`,
  `links()` or `comments()` returns it.
- `live_catalog_holds_no_value_mongodb` does the same for MongoDB's
  sampled documents.
- `a_model_is_never_sent_a_row_by_any_path` (`src/gateway/tests.rs`, on
  SQLite) fills every table of a database, has a fake model look up every
  table in its two rounds and explain a `SELECT *`, records every byte it's
  sent (and the text in it, escapes undone), and fails if any value is in
  them; `gateway context`'s output too.

`podelis gateway context <connection> "<question>"` shows you what a model
would be shown, and sends nothing.

### A model is not an agent

| | A model | An agent (`podelis mcp`) |
|---|---|---|
| What it is | The AI Podėlis asks for SQL (`Ctrl+K q`, `podelis ask`) | A program acting by itself, such as Claude Code |
| Runs SQL? | Never: its SQL lands in your editor | Yes, under the connection's rules |
| Sees rows? | Never | Yes, the rows of what it queries |
| Set up in | `config.toml` (`[[models]]`) | Its MCP settings, and each connection's **AI agents** setting |

An agent may pass what it reads to its own model, outside Podėlis. Share
with it only the connections you'd show that provider, and connect it as
a database user that reads only what it should. See [AI agents](agents.md).

## What your team knows: memory

Facts a model can't see in the schema (`status = 3` means cancelled,
revenue is net of refunds) go in the project's memory once and are shown
with those tables from then on:

```sh
podelis memory add orders.status 3 = cancelled
podelis memory add customer also called client          # another name: "clients" finds customer
podelis memory add revenue defined as sum(payment.amount)  # a term's formula: used whenever it's asked for
podelis memory add customer excluded by default active = false   # rows left out unless the question asks for them
podelis memory learn db/migrations    # each table's history, and the comments in your migrations
```

A fact whose words match a question brings in the tables it names, so
"revenue per store" finds `payment` through the definition, and "how many
clients" finds `customer` through the other name (words match by their
stems: "clients" finds "client"). A model that had to define a term, read
another name or leave rows out suggests such a fact itself, for you to
confirm. A fact about a table or column the database doesn't have (since
dropped or renamed) isn't shown to the model; `podelis gateway context`
lists such facts as maybe stale, to fix or forget.

In the app, `Ctrl+K y k` lists them: `a` adds, `e` (or `Enter`) changes the
selected one (`podelis memory edit <number> …` on the command line), `d`
forgets. A fact you change is yours from then on: confirmed, and kept
when `memory learn` reads its folder again; a model's guess you correct
isn't suggested again. Its relation stays unless you change it
(`orders history …` stays a history). When a model had
to assume what something means, it's kept as a suggestion (orange, `-- assumed:`
above its SQL) that's never shown to models until you confirm it (`c`, or
`podelis memory confirm <number>`). Forget one that's wrong (`d`) and the
same suggestion isn't kept again. Details in [Without the
UI](command-line.md#what-your-team-knows-memory); who can read the memory,
in [Configuration](configuration.md#who-can-read-them).

## The gateway

All of this goes through Podėlis's gateway: one background process that
holds the models and their keys, builds what they're shown, and keeps the
memory. Podėlis starts it when it's needed and it stops after 30 minutes
with nothing to do (`gateway_idle`), so you never manage it; one left
running by an older Podėlis is replaced by itself after an update (one of
a newer version is left alone, with a message to use that version). It listens
only on a socket in your config folder, and its log (`gateway/calls.jsonl`)
records which model was asked and how many tokens it took, never your
questions, SQL or facts. `podelis gateway status` says whether it runs.
AI agents connect through it too ([`podelis mcp`](agents.md)).
