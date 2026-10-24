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

- **In the app:** `Ctrl+K q`, type the question about the tab's
  connection, `Enter`. The SQL comes to the end of the editor under the
  question and the model's note, as comments; `F6` runs it once you've
  read it. Details in [the query screen](query-screen.md).
- **AI explain:** `Ctrl+K y` on a statement: the model says in plain
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
match its words ("rented" finds `rental`), the tables that join them,
their columns, keys, where foreign keys point, the database's comments,
and what your team has said about them (below). A fact that matches the
question brings the tables it names: with "revenue is payment.amount" in
memory, a question about revenue gets `payment`. A partitioned table is
shown once, as itself, not as each partition. Never rows. `podelis
gateway context prod "your question"` shows exactly that, and sends
nothing. How it's chosen, in the research's terms (schema linking), and
why this way: [the context, in the field's
terms](architecture.md#the-context-in-the-fields-terms).

**Your schema stays on your machine unless you allow more.** A project
shares its table and column names only with models on this machine. The
first time a provider's model would see them, Podėlis asks: this once, or
always for the project (`share_schema = "all"` in `connections.toml`;
remove it to go back).

## What your team knows: memory

Facts a model can't see in the schema (`status = 3` means cancelled,
revenue is net of refunds) go in the project's memory once and are shown
with those tables from then on:

```sh
podelis memory add orders.status 3 = cancelled
podelis memory add customer also called client          # another name: "clients" finds customer
podelis memory add revenue defined as sum(payment.amount)  # a term's formula: used whenever it's asked for
podelis memory learn db/migrations    # each table's history, and the comments in your migrations
```

A fact whose words match a question brings in the tables it names, so
"revenue per store" finds `payment` through the definition, and "how many
clients" finds `customer` through the other name (words match by their
stems: "clients" finds "client"). A model that had to define a term or
read another name suggests such a fact itself, for you to confirm.

In the app, `Ctrl+K k` lists them: `a` adds, `d` forgets. When a model had
to assume what something means, it's kept as a suggestion (orange, `-- assumed:`
above its SQL) that's never shown to models until you confirm it (`c`, or
`podelis memory confirm <number>`). Forget one that's wrong (`d`) and the
same suggestion isn't kept again. Details in [Without the
UI](command-line.md#what-your-team-knows-memory).

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
