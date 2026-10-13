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
hints = "minimal"           # status bar key hints: "minimal" (just `^K keys`) or "full"
export_dir = "~/Downloads"  # where exports go (default: current directory)
restore_session = true      # reopen the last project's tabs and connections
mouse = true                # clicks and the wheel (false: terminal selection without Shift)
viewer = "vd"               # opens result rows (`o`); `{}` is the file. Default: $PAGER, else less -S
keepalive = 0               # seconds between checks of an idle connection (0: off)
leader_double_space = false # Space twice quickly in the editor opens the Ctrl+K menu
notify_after = 10           # a query running this many seconds rings and notifies when done (0: off)
guard_rows = 1000000        # on production, a read of a whole table this big asks first, agents' is refused (0: off)

[keys]                      # rebind query-screen keys (replaces that action's defaults)
run = ["F5", "Ctrl+Enter"]
new_tab = "Ctrl+N"
```

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
  **including the SQL run**. Off by default.
- `crash.log` — if Podėlis ever crashes, what happened (with a backtrace);
  please attach it to a bug report. A crash in a background task is logged
  too and shown in the status bar, without taking the app down.

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
