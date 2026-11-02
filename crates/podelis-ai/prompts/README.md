# What Podėlis says to models

Each file here is one text the gateway sends to a model, compiled in
(`src/prompts.rs`): the prompt always matches the code that sends it, and
every word that goes to a provider is in this repository.

- `ask.md`: the system prompt for writing SQL for a question; `lookups.md` is
  added to it when the model may look for tables itself.
- `explain.md` and `explain_question.md`: the system prompt and the message
  for explaining a query.
- `names_feedback.md`: what goes back when the model's SQL names tables or
  columns the schema hasn't.
- `find_tables.md`, `describe_table.md`: the lookup tools' descriptions.

They read as Markdown does: a single line break is a space (wrap them as you
like), a blank line is a paragraph break, and a ```` ``` ```` block is sent as
written. `{name}`s are filled in by the code, once: a `{name}` in what's
filled in (the schema's text) stays as it is.

**A change here changes what models do.** The snapshot tests
(`src/snapshots/`) show it in review; measure it before keeping it: the
context's eval (`src/context/eval.rs`) and real questions to a model
(`podelis ask`, and the AI test project's `ask-tests.sh`).
