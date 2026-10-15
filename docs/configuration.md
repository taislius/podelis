# Configuration

[← Podėlis](../README.md)

Optional settings go in `config.toml` in the config folder (below):

```toml
row_limit = 1000            # rows kept in memory per result
theme = "dark"              # or "light" for light terminal backgrounds
server_syntax_check = true  # check SQL with the server while typing
lint = true                 # warn about likely mistakes: = NULL, UPDATE without WHERE…
system_tables = false       # list pg_catalog, information_schema, sqlite_schema… in the navigator (`.` toggles)
editor_mode = "standard"    # or "vim"
hints = "minimal"           # status bar key hints: "minimal" (`^K keys` and the pane's menu key) or "full"
export_dir = "~/Downloads"  # where exports go (default: current directory)
restore_session = true      # reopen the last project's tabs and connections
mouse = true                # clicks and the wheel (false: terminal selection without Shift)
viewer = "vd"               # opens result rows (`o`); `{}` is the file. Default: $PAGER, else less -S
keepalive = 0               # seconds between checks of an idle connection (0: off)
leader_double_space = false # Space twice quickly in the editor opens the Ctrl+K menu
notify_after = 10           # a query running this many seconds rings and notifies when done (0: off)
guard_rows = 1000000        # on production, a read of a whole table this big asks first, agents' is refused (0: off)
gateway_idle = 30           # minutes the gateway waits with no client before it stops (0: keeps running)

[keys]                      # rebind query-screen keys (replaces that action's defaults)
run = ["F5", "Ctrl+Enter"]
new_tab = "Ctrl+N"
```

A mistake in the file (a typo in a name, an unknown setting, a value of
the wrong kind) doesn't stop Podėlis: it starts with the defaults and
says which line is wrong. The AI features say it too, rather than
reporting no models set up: `podelis gateway models` and asking show the
error.

Rebindable actions (defaults in brackets): `run` (F5, Ctrl+R, Ctrl+Enter),
`run_statement` (F6), `explain` (F7, Ctrl+K e), `complete` (Ctrl+Space),
`history_prev` / `history_next` (Ctrl+P / Ctrl+N), `undo` / `redo`
(Ctrl+Z / Ctrl+Y), `toggle_schema` (Ctrl+K n, F2), `open_connection`
(Ctrl+K o, Ctrl+O), `disconnect` (Ctrl+K d, Ctrl+D), `new_tab` (Ctrl+T),
`close_tab` (Ctrl+W), `next_tab` / `prev_tab` (Ctrl+PageDown /
Ctrl+PageUp), `save_tab` (Ctrl+S), `rename_tab` (Ctrl+K t, F4), `library`
(Ctrl+K l, F3), `commit` / `rollback` (F8 / Shift+F8, Ctrl+K c / r),
`find` (Ctrl+F), `comment` (Ctrl+K -, Ctrl+/, Ctrl+7), `format` (Ctrl+K f,
F9), `palette` (Ctrl+K), `explain_analyze` (Shift+F7, Ctrl+K a),
`connection_info` (Ctrl+K u), `activity` (Ctrl+K s), `heartbeat`
(Ctrl+K h), `insert_snippet` (Ctrl+K i, Ctrl+E), `menu` (Ctrl+K m,
Shift+F10, F10), `zoom` (Ctrl+K z, F12), `projects` (Ctrl+K p),
`transaction_mode` (Ctrl+K x), `next_warning` (Ctrl+K w). Keys are written like `F5`, `Ctrl+R`,
`Ctrl+Shift+PageUp`, `Ctrl+Space`; `Ctrl+K x` is a letter in the Ctrl+K
menu. Unknown actions or keys are reported at startup, and the help screen
(`F1`) shows the keys in use.

Colours follow the terminal: 24-bit where supported (iTerm2, kitty, WezTerm,
Ghostty, Alacritty, VS Code…), otherwise the nearest of the 256 standard
colours (e.g. macOS Terminal).

## AI models

The models the gateway may use (see [the gateway](command-line.md#the-gateway)),
in `config.toml`, the first being the default:

```toml
[[models]]
name = "claude"                    # what it's called here
api = "anthropic"                  # Anthropic's API
model = "claude-opus-5-5"
# effort = "high"                  # low … max; "" sends none; default: high where the model takes it

[[models]]
name = "local"
api = "openai"                     # OpenAI's format: GPT, and local servers (Ollama, llama.cpp, LM Studio, vLLM)
url = "http://localhost:11434/v1"  # Ollama's; without url, the provider's own address
model = "qwen3:8b"

[[models]]
name = "gpt"
api = "openai"
model = "gpt-5"
key_command = "pass show openai"   # a command that prints the key
# max_tokens = 4000                # the longest answer
```

- **Keys** are looked for in this order: the OS keychain (stored with
  `podelis gateway key <name>`), the model's `key_command` (run without a
  shell, as a connection's password command; it gets the model's name in
  `PODELIS_MODEL`), then `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` for a
  model at the provider's own address. A model on this machine needs
  none. A keychain that can't be read (none on a server, or locked) is
  skipped, not an error. What a `key_command` prints is kept for 15
  minutes, so a password manager isn't asked on every question; a key
  the provider refuses is forgotten at once, so a fixed one is used next
  time. Only the gateway reads keys; they're never logged or shown.
- **Addresses** must use `https`; plain `http` only to this machine
  (`localhost`, `127.0.0.1`, `::1`), so a key never travels unencrypted.
- **Anthropic models** that take it (Opus from 4.5, Sonnet from 4.6, Haiku
  from 5, Fable) get `effort = "high"` unless set (writing SQL is work
  where depth pays); older ones refuse it, so none is sent to them unless
  you set one. The newest ones (Opus 5.5 and 5, Sonnet
  5.5, Fable 5.1) are sent with Anthropic's server-side fallback on: if
  the model declines a request, Anthropic answers it with another model.
- A busy or failing provider (429, 5xx, 529) is asked again up to twice,
  honouring its `retry-after`, as long as no part of the answer has
  come yet.
- The gateway reads these on each request: an edit counts without
  restarting it.
- **Whose schema a model may see:** a project's schemas (table and column
  names, comments) go only to models on this machine unless the project
  says `share_schema = "all"` (in `connections.toml`; `podelis ask` offers
  to set it the first time a provider's model would see them).

## Files

Everything lives in the `podelis` folder of your OS config directory (e.g.
`~/Library/Application Support/podelis` on macOS, `~/.config/podelis` on
Linux), or in `$PODELIS_CONFIG_DIR` if set:

- `connections.toml` — projects and their connections. If it can't be
  parsed, it is moved aside to `connections.toml.bak-<timestamp>` rather
  than overwritten.
- `history/<project-id>.jsonl` — each project's query history: the last
  500 queries, up to 16 MB in all (a query over 1 MB isn't kept).
- `drafts/<project-id>.json` — each project's open tabs, their text,
  connections and cursors, and the rest of the session (written a moment
  after you stop typing, so a crash loses little).
- `queries/<project-id>.json` — each project's saved-query library.
- `agents/<project-id>.jsonl` — what AI agents ran in the project through
  `podelis mcp` (see [For AI agents](agents.md)), up to about 5 MB; and
  `agents/<project-id>.seen`, when you last looked at it.
- `config.toml` — settings (optional, never written by the app).
- `podelis.log` — only with `PODELIS_LOG=debug` (or another
  [filter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
  like `podelis=trace`): what the app and the database driver did,
  **including the SQL run**; `podelis=trace` also logs each key as the
  terminal sent it, for a key that doesn't do what it should. Off by default.
- `crash.log` — if Podėlis ever crashes, what happened (with a backtrace);
  please attach it to a bug report. A crash in a background task is logged
  too and shown in the status bar, without taking the app down.
- `memory/<project-id>.db` — each project's facts for models
  (`podelis memory`), in SQLite.
- `gateway/` — the gateway's socket and lock file while it runs, and
  `calls.jsonl`: each call to it (when, which client, which method, the
  outcome; for a model, its name and the tokens), never what was asked
  or answered, up to about 1 MB.

Files are created readable only by you (they can contain SQL and
connection details).

Passwords are stored in the OS keychain under the service `podelis`, keyed
by project id and connection name, so renaming a project doesn't affect
them. A connection with a password command keeps no password there. On
Linux any Secret Service works: gnome-keyring, KWallet, KeePassXC (with its
Secret Service integration on) or oo7; `secret-tool search service podelis`
lists what's stored.

Upgrading from a version without projects: existing connections (and query
history) are moved into a project named `Default` on first start. Their
saved passwords keep working, and move to the new keychain key the next
time you save the connection with `Ctrl+S` or move it with `m`.
