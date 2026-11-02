# Security

## Reporting a problem

Please report a security problem privately, not in a public issue:
**Security → Report a vulnerability** on
[the repository](https://github.com/taislius/podelis/security/advisories/new).
Only you and the maintainer see it there.

Say what you did, what happened, and what you expected: the SQL or keys
involved, the database and its version, and `podelis doctor`'s output help.
You'll hear back, and the fix and its release are worked out with you
there; you're credited unless you'd rather not be.

## Versions

Fixes go into the next release; only the latest version gets them. Update
with `brew upgrade podelis`, or `cargo install` again (see the README).

## What counts

- **Secrets leaving where they belong**: a password or a model's key
  written to a log, the gateway's call log (`calls.jsonl`), a crash
  report, the screen, or sent to a model; the keychain or a password
  command's output reaching anyone but the database.
- **Another user on the machine reaching yours**: the gateway's socket,
  the config folder and its files are meant to be the user's alone.
- **Schemas or data sent to a model** a project's `share_schema` doesn't
  allow.
- **A way past the rules for AI agents** (`podelis mcp`): reading where
  a connection is shared to read only, writing a file on the server, or
  reaching other sessions. These rules are a safety net, not the
  boundary: the database user's rights are ([docs/agents.md](../docs/agents.md)).
  A way past them is still fixed first and told in the release notes
  (as in 0.7.3).

Not a security problem, but welcome as an ordinary issue: the guards for
people (production mode, destructive statements, read-only connections),
which catch mistakes and say so ([docs/command-line.md](../docs/command-line.md)).
