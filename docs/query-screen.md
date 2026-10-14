# The query screen

[← Podėlis](../README.md)

## The command menu

Most actions are behind one key: **`Ctrl+K`** (or `Space` in the schema
pane and vim's Normal mode; Space twice with `leader_double_space`), then a
letter — `Ctrl+K x r` switches to rollback-only. If you pause
after `Ctrl+K`, a menu shows the letters and the focused pane's own keys
(`?` outside the editor shows it at once). Type anything else (or `/`) to
search every command by name. `F1` shows every key.

| `Ctrl+K`, then | Action |
|---|---|
| `x`, then `a` / `m` / `r` | Transaction mode: *auto-commit* (each statement is committed as it runs), *manual commit* (changes wait for `F8` COMMIT / `Shift+F8` ROLLBACK) or *rollback-only* (statements run, then are always rolled back). Clicking the modes in the editor's footer cycles them |
| `c` / `r` | Commit / roll back the open transaction (also `F8` / `Shift+F8`) |
| `e` / `a` | Explain / explain analyze (also `F7` / `Shift+F7`) |
| `f` / `-` | Format the selection, or everything / comment or uncomment lines |
| `i` | Insert a snippet or saved query at the cursor |
| `n` | Show/hide the navigator |
| `z` | Zoom the focused pane to the whole screen; again or `Esc` returns. Tab still moves between panes (also `z` in results/schema, or double-click a pane's title) |
| `m` | Menu of what you can do in the focused pane (also right-click) |
| `l` / `t` | Search saved queries and history / rename the tab |
| `o` / `d` | Pick a connection for this tab (opens a new tab if this one is connected) / disconnect this tab |
| `p` | Projects and connections (or click the project's name in the top bar): see [Projects and connections](connections.md) |
| `u` / `s` / `h` | Connection info / the server's sessions / connection heartbeat on or off |
| `1`…`9` | Jump to a tab (the menu lists the open tabs by number; typing a tab's name finds it too) |
| `?` | Every key |

Keys that work directly:

| Key | Action |
|---|---|
| `F5` / `Ctrl+R` / `Ctrl+Enter`¹ | Run the selection, or everything in the editor (or click **▶ F5 Run** right of the tabs; *Statement* and *Explain* are there too) |
| `F6` | Run the selection, or the statement under the cursor. With several statements in the editor, its footer says which one that is (`stmt 2/3`) |
| `F7` | Show the plan for the selection / statement under the cursor (not executed). MySQL 8.0.16+ shows `FORMAT=TREE`; older MySQL and MariaDB show the classic `EXPLAIN` table |
| `Shift+F7` | `EXPLAIN ANALYZE`: runs the statement and shows actual times and rows (writes ask first and are rolled back). MySQL needs 8.0.18+; MariaDB runs `ANALYZE` |
| `F8` / `Shift+F8` | Commit / roll back the open transaction |
| `Esc` | Cancel the running query |
| `Ctrl+P` / `Ctrl+N` | Previous / next query from history |
| `Tab` / `Shift+Tab` | Cycle focus: editor → results → schema (`Tab` right after a word completes it instead; `Esc` then `Tab` still moves on) |
| `Ctrl+↑` / `Ctrl+↓` | Move the border between the editor and the results up / down |
| `Ctrl+←` / `Ctrl+→` | Move the navigator's border left / right (from the navigator or the results; in the editor they move by word) |
| `Ctrl+Q` | Quit (`Ctrl+C` copies on this screen: see the editor and results keys) |
| `F1` | Every key |

The function keys from earlier versions still work: `F2` navigator, `F3`
library, `F4` rename tab, `F9` format,
`F10` / `Shift+F10` menu, `F12` zoom, and `Ctrl+O`,
`Ctrl+D`, `Ctrl+E`.

## Tabs and the query library

From any pane:

| Key | Action |
|---|---|
| `Ctrl+T` / `Ctrl+W` | New tab on the same connection / close tab (asks if it has unsaved changes) |
| `Ctrl+PgUp` / `Ctrl+PgDn`, `Ctrl+K 1…9` | Previous / next tab, or jump to one |
| `Ctrl+S` | Save the tab to the library; the first time asks for `group/name` (e.g. `reports/churn by month`), later saves update it |
| `Ctrl+K t` | Rename the tab and its saved query; a different `group/` moves it |
| `Ctrl+K l` | Search saved queries (name, group and SQL) and history; `Enter` opens, `F4` renames/moves, `Ctrl+D` deletes |

A tab's `SQL` label is orange while the tab isn't saved to the library
(its text is still kept as a draft), and a `●` on a saved query's tab means
its text has changed since it was saved. Tabs, their
connections and cursors come back after a restart (unless
`restore_session = false`); results don't, press `F5` to re-run.

**Query parameters.** Running SQL with placeholders — `:name` in any
engine, `$1` on Postgres, `?` on MySQL/SQLite, and SQLite's `?2`, `@name`
and `$name` — first asks for their values
(placeholders in strings, comments and `::type` casts don't count). Numbers
and `NULL` go in as written, anything else as a quoted string, and a value
starting with `=` is inserted as raw SQL (`=now()`). Values are remembered
for the next run. History keeps the placeholders, not the values, and leaves
out statements that set a password (`CREATE`/`ALTER USER` or `ROLE … PASSWORD`,
`IDENTIFIED BY`, `SET PASSWORD`), as the `mysql` client does.

¹ `Ctrl+Enter` needs a terminal that supports the kitty keyboard protocol
(kitty, WezTerm, Ghostty, foot, iTerm2 with CSI u enabled). Elsewhere it
arrives as a plain `Enter`; use `F5` or `Ctrl+R`.

## Editor

When focused:

| Key | Action |
|---|---|
| `←`/`→`/`↑`/`↓`/`Home`/`End`/`PageUp`/`PageDown` | Move |
| `Ctrl+←`/`Ctrl+→` (or `Alt`/`Option`) | Move by word |
| `Ctrl+Home`/`Ctrl+End` | Start / end of the text |
| `Shift` + any movement | Select |
| `Ctrl+A` | Select all |
| `Ctrl+C` / `Ctrl+X` | Copy / cut the selection; with nothing selected, `Ctrl+C` copies the cursor's line |
| `Ctrl+V` | Paste from the system clipboard (`pbpaste`, `wl-paste`, `xclip` or `xsel`), for terminals where `Ctrl+V` doesn't paste; the terminal's own paste (`Cmd+V` on macOS) works too |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Tab` (after a word) / `Ctrl+Space` | Autocomplete; typing `.` after a table, alias or schema opens it too |
| `Esc`, then `Tab` | Next pane, even right after a word (the status bar hint says `ESC TAB` when plain Tab would complete) |
| `Space` `Space` | The `Ctrl+K` menu, when `leader_double_space = true`: two quick Spaces, and the first is taken back |
| `Ctrl+F` | Find and replace: `Enter`/`↓` next, `↑` previous, `Tab` to the replacement, `Enter` replaces, `Ctrl+A` replaces all |
| `Ctrl+/` | Comment / uncomment the selected lines (`Ctrl+7` in terminals that send that) |
| `Ctrl+K i` / `Ctrl+E` | Insert a snippet or saved query at the cursor (the library window, starting at the `snippets` group) |
| `Ctrl+K m` / right-click | Menu of what you can do in the editor: copy, cut and paste first (right-clicking the selection keeps it) |
| `Ctrl+K f` | Format the selection, or everything. Strings, quoted names and block comments stay exactly as written; if formatting would change anything but spacing, it's refused |
| `↑`/`↓`, `Tab`/`Enter`, `Esc` | In the completion list: choose, accept, close |

Pasting is supported, including multi-line SQL: the terminal's own paste
(`Cmd+V` on macOS, `Ctrl+Shift+V` in most Linux terminals) or `Ctrl+V`,
which reads the system clipboard itself (`pbpaste`, `wl-paste`, `xclip` or
`xsel`). Over SSH use the terminal's paste: those tools would read the
server's clipboard.

**Vim keys** (`editor_mode = "vim"` in `config.toml`): Normal, Insert,
Visual and Visual-line modes, shown in the status bar. Supported: counts,
`h j k l w b e 0 ^ $ gg G`, `i a I A o O`, `x X dd D cc C yy p P J r ~ u`,
operators with motions (`dw`, `c$`, `y3w`…), `iw` (`ciw`, `diw`, `yiw`) and
`v`/`V` with `d y c x`. Redo is `Ctrl+Y`, since `Ctrl+R` runs the query.

## Results

When focused:

| Key | Action |
|---|---|
| `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` | Move between rows |
| `←`/`→`, `0` / `$` | Move between columns, or to the first / last (scrolls sideways; `Shift`+wheel or a sideways wheel too). The footer shows which columns are on screen (`◀ cols 4–9 of 30 ▶`) |
| `Enter` / `v` | Show the full value of the cell (JSON pretty-printed) |
| `x` | Show the row vertically, one `column  value` line each. `↑`/`↓` scroll it and then go on to the previous / next row; `←`/`→` go straight to the previous / next row |
| `X` | All rows one column per line, like the mysql client's `\G` (`*** 1. row ***`, then `name: value`), or back to the table. `↑`/`↓` go field by field into the next row, `PageUp`/`PageDown` a row at a time. Ending a statement with `\G` instead of `;` (`select * from users\G`) shows its result this way; the next run without it goes back to the table |
| `f` | Follow the foreign key in the selected cell: opens the referenced row in a new tab |
| `b` | Chart the selected column of the rows shown: count, sum, min, max and average, the trend as a sparkline, and a bar per row labelled by another column (the first 200) |
| `\|` | Pipe the rows shown through a shell command (`grep -i vilnius`, `sort -t$'\t' -k3 -n`, `jq '.[].email'`, `mlr --itsv --opprint stats1 -a sum -f total`…) and show its output |
| `o` | Open the rows shown in the viewer: `$PAGER` (else `less -S`), or `viewer` from `config.toml`, e.g. VisiData |
| `s` | Sort by the selected column: ascending → descending → off (numbers by value, then text, then NULLs) |
| `/` | Filter rows (case-insensitive, any column); `Esc` clears |
| `c` | Edit the cell: type the new value, `Enter` saves, `Ctrl+N` sets NULL, `Esc` cancels |
| `a` | Add a row: a form of the table's columns (empty fields get their default; `NULL`; `=expr` for raw SQL) |
| `Space` / `d` | Mark rows / delete the marked rows (or the current one); always asks first, `t` reviews it in a transaction |
| `C` | Copy the marked rows (or all rows shown) as a Markdown table |
| `m` / right-click | Menu of everything you can do here, with each action's key |
| `y` / `Y` or `Ctrl+C` | Copy the selected cell / row (tab-separated); `Y` and `Ctrl+C` copy the marked rows, a line each, when rows are marked |
| `e` / `J` / `i` | Export the rows shown as CSV / JSON / SQL `INSERT` statements to `podelis-export-<timestamp>.<ext>` (in the current directory, or `export_dir`). For every row of a big result, not just those loaded, use "Export all rows" in the menu (`m`) or Ctrl+K: it writes the rest straight to the file as it arrives, without loading it into the grid; `Esc` stops it |
| `M` | Load 10× more rows (streamed results fetch the next pages; scripts are re-run with a bigger limit, read-only ones only) |
| `[` / `]`, `1` `2` `3` (or click a tab) | Switch between the RESULT, MESSAGES and QUERY PLAN tabs (`F7` opens the plan with the results focused; `1` goes back) |
| `↑`/`↓`/`PageUp`/`PageDown`, `←`/`→`, `Home` / `0` | In MESSAGES and QUERY PLAN: scroll down and across, back to the top / the left edge. A plan wider than the pane says which columns show in the footer |

Above the plan: the statement it's for (`EXPLAIN` or `EXPLAIN ANALYZE`),
its lines in the editor (`lines 12–15`; click them to go there), when the plan was made and
how long ago. Once that statement is no longer in the editor as it was, it
says `was lines 12–15 · edited since`.

A plan that comes as a table (MySQL before 8.0.16 and MariaDB: the classic
`EXPLAIN` columns) shows in QUERY PLAN as a grid like the result's, under its
notes, and takes the same keys: moving, `s` sort, `/` filter, `x` / `X`,
`y` / `Y` / `C` / `Ctrl+C` copy, `e` / `J` export, `|` and `o`. Editing, adding,
deleting, following a foreign key and loading more are for query results
only.

**Snippets** are saved queries in the `snippets` group (save one with
`Ctrl+S` as `snippets/recent orders`). Typing the start of a snippet's name
offers it in autocomplete, and choosing it inserts its SQL; `:parameters` in
it are asked for when you run.

**Menus.** Right-click a pane, or press `m` in the results or schema
(`Shift+F10` anywhere), for a small menu of what you can do there, each
item with its key — a quick way to learn the shortcuts. The editor's starts
with Copy and Cut (with text selected) and Paste; the results pane, with
messages or nothing run yet, offers its tabs and zoom.

Cells can be edited, and rows added or deleted, when the result comes from
a plain `SELECT` on one table (no joins, grouping or computed columns) that includes the table's
primary key. The generated `UPDATE … WHERE <primary key> = …` goes through
the usual checks: production connections ask first, read-only ones refuse.

**Reviewing writes.** When a write needs confirming (production
connections, or `UPDATE`/`DELETE` without `WHERE`, `DROP`…), press `t`
instead of `y`: it runs after a `BEGIN`, then shows how many rows changed —
`c` commits, `r` rolls back, `Esc` leaves the transaction open so you can
check the changes with SELECTs first (`F8` commits, `Shift+F8` rolls back).

**Watch mode.** Ctrl+K, then "Watch", runs the last query again every 2,
5, 10, 30 or 60 seconds, like `watch`: for a queue draining, replication
lag or a migration's progress. Rows new or changed since the run before
are marked with an orange `◆` (matched by the first column when it tells
rows apart), and the status bar counts them. Only reads can be watched;
`Esc` (once it has nothing else to do: a results filter goes first, and
in vim mode it's vim's), "Stop watching" in Ctrl+K, running something
else, another tab or an error stops it.

**Schema snapshots.** Ctrl+K, then "Save a schema snapshot", keeps
the connection's tables, columns, indexes and foreign keys in a file
(`snapshots/<project>/` in the config folder). "Compare the schema with
the snapshot: …" (the 20 newest are listed) later shows what changed since, as the comparison of two
connections does, with the SQL that changes it back.

**Production: whole big tables ask first.** On a production connection, a
read whose plan reads a whole table of `guard_rows` rows or more (a
million by default; 0 turns it off) asks before running, naming the table
and about how many rows. A `LIMIT` that stops the read early (no filter
or sort in the way) doesn't count.

**Connection info and activity** (command palette, or bind keys to
`connection_info` / `activity`): the info panel shows the server version,
the user and database you're really on, schemas, time zone, encoding,
isolation level, timeouts, read-only state, backend id, TLS version and
cipher, and the current latency — click the connection status in the top
bar to open it. The activity monitor lists the server's sessions (Postgres
`pg_stat_activity`, MySQL's process list) with their state, time, what
they wait on and who blocks them, refreshing every 2 s: `Enter` shows a
session's full query, `c` cancels its query, `x` ends the session (both ask
first; read-only connections refuse). The **connection heartbeat** checks
an idle connection regularly, so a drop is noticed (and reconnected) before
your next query: switch it on or off from the palette ("Connection
heartbeat", or bind `heartbeat`), with `keepalive = 60` in `config.toml`
as the interval and default (0 = off). While it runs, the status bar
shows `♥` before the connection; a failed check turns it red with
**connection lost** until the next successful check or query. "Copy diagnostics" in the palette
copies versions, terminal and settings for a bug report — no hosts, users
or SQL.

**Pipes and viewers.** `|` sends the rows shown (after filtering and
sorting) to a command's standard input as TSV with a header line — one row
per line, tabs and line breaks inside values written as a space and `\n`,
`NULL` as an empty field. Commands named `jq`, `gron`, `jaq`, `gojq` or `fx`
get JSON instead. The command runs through your shell (`sh -c`), so
pipelines work: `| grep LT | sort | uniq -c`. `o` writes
the rows to a private temporary file and opens it in the viewer; Podėlis
hands it the terminal and comes back when it exits.

**Mouse:** click a pane to focus it, a tab to switch to it, `+` for a new
tab, the editor to place the cursor (drag to select), a result cell or a
schema item to select it; double-click opens a cell or expands a table; the
wheel scrolls. Your terminal's own text selection then needs `Shift` (or
`Option` on macOS); `mouse = false` in `config.toml` turns this off.

Copying uses your system's clipboard tool (`pbcopy`, `wl-copy`, `xclip` or
`xsel`), which works in any terminal and for any size. Over SSH, or
without one, it uses the OSC 52 terminal escape sequence, which most
modern terminals support; in tmux, enable it with `set -g set-clipboard on`.
Many terminals drop an OSC 52 copy over about 100 kB, so a bigger one over
SSH says so: export it instead.

## Navigator

When focused:

| Key | Action |
|---|---|
| `↑`/`↓`, `PageUp`/`PageDown`, `Home`/`End` (`g`/`G`) | Move by a row, a page, or to the first / last |
| `/` | Filter tables, views and functions by name (or click the filter line) |
| `o` | Order the tables by name, by rows, or by size on disk (indexes included; the size then shows instead of the rows). Both are the server's estimates; SQLite has no sizes |
| `Enter` / `→` | Expand a table: its columns (with `→ table.column` for foreign keys) and indexes, scrolled into view; on a function, show its definition |
| `←` | Collapse |
| `d` | Show the definition: `CREATE VIEW`, the function source, or a table's columns, keys and indexes (SQLite: its `CREATE TABLE`). In it, `←`/`→` show the previous / next table's (the navigator's selection follows), `y` copies it |
| `s` | Add `SELECT * FROM <table> LIMIT 100;` at the end of the editor and run it (only it, as `F6` would) |
| `f` | Related tables: what the table points to and what points to it through foreign keys, two steps each way |
| `r` | Refresh (also happens automatically after DDL, `USE` and `SET`) |
| `.` | Show or hide system tables (`pg_catalog` and `information_schema`; MySQL's `information_schema`, `performance_schema`, `mysql` and `sys`; SQLite's `sqlite_schema` and `sqlite_*`) in a SYSTEM section |
| `Space` | The `Ctrl+K` command menu |
