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
| `x`, then `a` / `m` / `r` / `s` | Transaction mode: *auto-commit* (each statement is committed as it runs), *manual commit* (changes wait for `F8` COMMIT / `Shift+F8` ROLLBACK), *rollback-only* (statements run, then are always rolled back) or *sandbox* (see *The sandbox* below). Clicking the modes in the editor's footer cycles them. With a sandbox open, `Ctrl+K x` shows it |
| `c` / `r` | Commit / roll back the open transaction (also `F8` / `Shift+F8`) |
| `e` / `a` | Explain / explain analyze (also `F7` / `Shift+F7`) |
| `f` / `-` | Format the selection, or everything / comment or uncomment lines |
| `i` | Insert a snippet or saved query at the cursor |
| `n` | Show/hide the navigator |
| `z` | Zoom the focused pane to the whole screen; again or `Esc` returns (a zoom goes before a running query or watch mode: the next `Esc` stops that, as the status bar then says). Tab still moves between panes (also `z` in results/schema, or double-click a pane's title) |
| `v` | Select the statement at the cursor (a right-click puts the cursor under the pointer: *Select the statement* in its menu) |
| `b` | The statement at the cursor (or the selected lines) alone, in a taller pop-over over the editor, the results under it (also `Shift+F12`). It's the tab's own text, edited in place; moving out of the statement shows the next one, and `F5` runs the one in sight. It stays while another pane has the keys (an Explain's plan, the results under it); again, or `Esc` in the editor, shows the whole text |
| `m` | Menu of what you can do in the focused pane (also right-click) |
| `l` / `t` | Search saved queries and history / rename the tab |
| `g` | Every tab, with its connection and what runs there (see *Tabs and the query library*) |
| `o` / `d` | Pick a connection for this tab (opens a new tab if this one is connected) / disconnect this tab (asks first, and says so when that rolls back a transaction or cancels the query running here) |
| `p` | Projects and connections (or click the project's name in the top bar): see [Projects and connections](connections.md) |
| `u` / `s` / `h` | Connection info / the server's sessions / connection heartbeat on or off |
| `q` | Ask in plain language: a model writes the SQL for your question (see *Asking in plain language* below) |
| `y` | AI explain: a model says in plain words what the statement at the cursor (or the selection) does, in a pop-over; nothing runs and the editor isn't touched |
| `k` | The project's memory: what models are told about your data. `a` adds a fact (`orders.status 3 = cancelled`), `c` confirms one a model suggested (orange), `d` forgets one |
| `1`…`9` | Jump to a tab (the menu lists the open tabs by number; typing a tab's name finds it too) |
| `?` | Every key |

The menu shows when no letter follows `Ctrl+K` within half a second. Who
knows the letters can turn that off (`leader_menu = false`, or *Ctrl+K
shows its menu* in the settings): `Ctrl+K` then waits quietly, and `?`
shows the menu. `Ctrl+Shift+K` shows it at once, in terminals that tell it
apart from `Ctrl+K` (kitty, WezTerm, Ghostty, foot, and others with the
kitty keyboard protocol). In the menu, `/` or `Enter` searches every
command by name.

Keys that work directly:

| Key | Action |
|---|---|
| `F5` / `Ctrl+R` / `Ctrl+Enter`¹ | Run the selection, or everything in the editor. In a tab whose connection couldn't be opened again when Podėlis started (the server was down, the VPN off), `F5`, `F6` and `F7` try it again first and go ahead once it's open (or click **▶ F5 Run** right of the tabs; *Statement* and *Explain* are there too). The results pane switches to RESULT as it starts, with a result of its own (the one before stays: `,` in the results) |
| `F6` | Run the selection, or the statement under the cursor. With several statements in the editor, its footer says which one that is (`stmt 2/3`) |
| `F7` | Show the plan for the selection / statement under the cursor (not executed), switching to QUERY PLAN as it starts. Pressed while a query runs, PostgreSQL and MySQL make it at once on a connection of their own; where that would see something else (SQLite, an open transaction, after `USE` or `SET`, `:parameters`) it follows when the query finishes, and if that fails its error stays in view instead (`Esc` stops both). MySQL 8.0.16+ shows `FORMAT=TREE`; older MySQL and MariaDB show the classic `EXPLAIN` table |
| `Shift+F7` | `EXPLAIN ANALYZE`: runs the statement and shows actual times and rows (writes ask first and are rolled back). MySQL needs 8.0.18+, and runs only reads and UPDATE/DELETE of several tables this way (for the rest it shows the plan, `F7`'s); MariaDB runs `ANALYZE` |
| `F8` / `Shift+F8` | Commit / roll back the open transaction |
| `Esc` | Cancel the running query, in its own tab (on the connection it runs on). In another tab `Esc` doesn't stop it: it says which tab, on which connection, is running it (`Tab “Query 1” on prod is running a query: Alt+1 goes to it`). The results pane's title counts how long it has run (`running… 12 s`). When the server can't be reached to be told (a VPN or network that dropped), it says so in orange, the status bar shows *connection lost*, and `Esc` again stops waiting: the next run opens a new session (a read runs on it; anything else isn't run and says so, since a transaction that was open is gone — run it again). A cancel the server refuses (a missing privilege) is only said |
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
| `Ctrl+T` / `Ctrl+W` | New tab on the same connection / close the tab, and its text with it (asks if it has unsaved changes); also a middle click on the tab, or *Close the tab* in the editor's menu. The last tab is emptied instead |
| `Ctrl+PgUp` / `Ctrl+PgDn`, `Ctrl+K 1…9` | Previous / next tab, or jump to one |
| `Ctrl+S` | Save the tab to the library; the first time asks for `group/name` (e.g. `reports/churn by month`), later saves update it |
| `Ctrl+K t` | Rename the tab and its saved query; a different `group/` moves it |
| *Save the statement to the library* | In the editor's menu (right-click): only the statement at the cursor (or the selection), under a `group/name` asked for, as a query of its own; the tab stays as it is (bind a key with `save_statement`) |
| `Ctrl+K l` | Search saved queries (name, group and SQL) and history; `Enter` (or a double click) opens, `F4` renames/moves, `Ctrl+D` deletes. Each row says when it was saved or ran, and the selected one's first five lines show under the list. `@` words narrow it by date: `@today`, `@yesterday`, `@7d` / `@2w` / `@3m` (the last days, weeks, months), `@2026`, `@2026-10`, `@2026-10-21`, `@2026-10-01..2026-10-15` (either side may be left out). Queries saved, or run, by older versions have no date and don't match these |
| `Ctrl+K ,` | Settings: language, theme, editor keys (vim), key hints, mouse, the checks, row limit, timeouts and the rest of `config.toml`'s general ones. `←`/`→` change one, `Enter` changes it or types a number; each change applies at once and is saved (see [Configuration](configuration.md)) |
| `Ctrl+K g` | Every open tab in a list, with its connection and how that is: the query running there and for how long (`running 3:12`, `cancelling…`), a transaction left open, a lost connection, or no connection. Type to find one by its name, its connection or the SQL in it; `↑`/`↓` and `Enter` go there |

A tab's `SQL` label is orange while the tab isn't saved to the library
(its text is still kept as a draft), and a `●` on a saved query's tab means
its text has changed since it was saved. Tabs, their
connections and cursors come back after a restart (unless
`restore_session = false`); results don't, press `F5` to re-run. The active
tab shows its whole name (up to 40 characters) and the others share what's
left; when they don't all fit, the row starts later, `‹3` saying how many
tabs are before it and `›2` after it; a click on either lists every tab, to
search by name and pick from (as `Ctrl+K g` does).

While a query that reads runs, other tabs can be used: switch to one, open
a new one, read its results, and **run queries there too**: each tab has
its own query, so tabs on different connections run at the same time.
A running tab's name gets a green `…` (`Ctrl+K g` lists them, with how
long each has run), its outcome lands in it when it's done (the status bar
says so), its timeout and long-query notice count on while it's hidden,
and the rows a streamed result hadn't sent yet are loaded with `M`. `Esc`
stops a tab's query only in that tab. A connection runs one query at a
time, so a tab on the same connection as a running one waits: `F5` and
`Esc` there say which tab is running it and how to get there. A write, a
review or a comparison keeps every tab where it is until it's done.

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
| `Ctrl+K f` | Format the selection, or the statement at the cursor. Strings, quoted names and block comments stay exactly as written; if formatting would change anything but spacing, it's refused |
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
Yanks also go to the system clipboard (deletes stay in vim's register), and
`Ctrl+V` pastes from it in Normal mode as in Insert mode.

## Results

When focused:

| Key | Action |
|---|---|
| `↑`/`↓`/`PageUp`/`PageDown`/`Home`/`End` | Move between rows |
| `←`/`→`, `0` / `$` | Move between columns, or to the first / last (scrolls sideways; `Shift`+wheel or a sideways wheel too). The footer shows which columns are on screen (`◀ cols 4–9 of 30 ▶`) |
| `Enter` / `v` | Show the full value of the cell (JSON pretty-printed) |
| `x` | Show the row vertically, one `column  value` line each. `↑`/`↓` scroll it and then go on to the previous / next row; `←`/`→` go straight to the previous / next row |
| `X` | All rows one column per line, like the mysql client's `\G` (`*** 1. row ***`, then `name: value`), or back to the table. `↑`/`↓` go field by field into the next row, `PageUp`/`PageDown` a row at a time. Ending a statement with `\G` instead of `;` (`select * from users\G`) shows its result this way; the next run without it goes back to the table |
| `f` | Follow the foreign key in the selected cell: the row it points to, in a pop-over (one column per line), without leaving the result. It works from a join too, when the column is a foreign key in one of its tables. In the pop-over, `e` opens the lookup in a tab of its own and runs it, `y` copies the row |
| `b` | Chart the selected column of the rows shown: count, sum, min, max and average, the trend as a sparkline, and a bar per row labelled by another column (the first 200) |
| `\|` | Pipe the rows shown through a shell command (`grep -i vilnius`, `sort -t$'\t' -k3 -n`, `jq '.[].email'`, `mlr --itsv --opprint stats1 -a sum -f total`…) and show its output |
| `o` | Open the rows (the marked ones, or those shown) in an app: a menu of formats — CSV, JSON, INSERT statements, TSV, a text table, an ASCII table (`+--+` borders) — each with what opens it. By default the system's app for the type (`open` on macOS, `xdg-open` on Linux: a spreadsheet for CSV, an editor for JSON or SQL), in a window of its own while Podėlis runs on; without a desktop to show one (over SSH, a Linux console) `$PAGER`, else `less -S`, in this terminal; `[viewers]` in `config.toml` sets another per format (`ascii` for the ASCII table, else `txt`'s), or one that runs in this terminal (VisiData) |
| `s` | Sort by the selected column: ascending → descending → off (numbers by value, then text, then NULLs) |
| `/` | Filter rows (case-insensitive, any column); `Esc` clears |
| `c` | Edit the cell: type the new value, `Enter` saves, `Ctrl+N` sets NULL, `Esc` cancels |
| `a` | Add a row: a form of the table's columns (empty fields get their default; `NULL`; `=expr` for raw SQL) |
| `Space` / `d` | Mark rows / delete the marked rows (or the current one); always asks first, `t` reviews it in a transaction |
| `C` | Copy the marked rows (or all rows shown) as a Markdown table; as CSV, JSON, `INSERT` statements or an ASCII table (`+--+`) from the menu (`m`) or Ctrl+K ("Copy rows as…") |
| `m` / right-click | Menu of everything you can do here, with each action's key |
| `y` / `Y` or `Ctrl+C` | Copy the selected cell / row (tab-separated); `Y` and `Ctrl+C` copy the marked rows, a line each, when rows are marked |
| `e` / `J` / `i` | Export the rows shown as CSV / JSON / SQL `INSERT` statements to `podelis-export-<timestamp>.<ext>` (in the current directory, or `export_dir`); the menu (`m`) also has an ASCII table (`+--+` borders, `.txt`; long values cut at 120 columns). For every row of a big result, not just those loaded, use "Export all rows" in the menu (`m`) or Ctrl+K: it writes the rest straight to the file as it arrives, without loading it into the grid; `Esc` stops it |
| `M` | Load 10× more rows (streamed results fetch the next pages; scripts are re-run with a bigger limit, read-only ones only); the first of the new rows is selected, so they're in sight (sorted or filtered, the first place one of them is). Pressed while a page is still coming, its rows follow that page |
| `[` / `]`, `1` `2` `3` (or click a tab) | Switch between the RESULT, MESSAGES and QUERY PLAN tabs (`F7` opens the plan with the results focused; `1` goes back) |
| `,` / `.` (or click `#2`) | Show the result of an earlier / later run: each query you run keeps its own (see *Earlier results* below) |
| `p` | Pin the result shown, so it's kept however many queries run after it (again unpins) |
| `↑`/`↓`/`PageUp`/`PageDown`, `Home` / `End` | In MESSAGES: select a message (or click it). `Enter` shows all of it (or double-click), `y` / `Ctrl+C` copies it, `Y` copies every message, `Esc` clears the selection |
| `↑`/`↓`/`PageUp`/`PageDown`, `←`/`→`, `Home` / `0` | In QUERY PLAN: scroll down and across, back to the top / the left edge (`←`/`→` scroll MESSAGES across too). A plan wider than the pane says which columns show in the footer |
| click, drag, `Shift+↑`/`↓` | In QUERY PLAN: select lines (`Shift`+click extends to a line; dragging past the edge scrolls). The selection is kept by line, so scrolling keeps it whole; `y` or `Ctrl+C` copies it (else the whole plan), `Y` the whole plan, `Esc` lets go |

**Earlier results.** Each query you run (`F5`, `F6`, `s` in the navigator,
a comparison…) gets a result of its own, numbered `#1`, `#2`… on the line
above the rows, and the one before stays a key away: `,` and `.` (or a
click on its number) switch between them, and every key works on the one
shown, as on the newest: editing a cell, `M`, watch mode. That line also
says which statement the result is for (a script's result set shows the
statement that made it, not the whole script), with its lines in the editor
(`lines 12–15`; click them to select it there), and when it ran. The last
10 are kept, and the older ones go sooner when together they hold over
200,000 rows (the newest stays, however big); `p` pins one to keep it
longer (pinned numbers are green).
A run that fails says why where its rows would be, and the next run takes
its place. Re-running the result shown (`M`, after adding or deleting
rows, watch mode) replaces it. Results are kept until the tab is closed,
not over a restart.

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

**The sandbox.** In the sandbox mode (`Ctrl+K x s`) writes build on each
other: the first starts one transaction, kept open between runs, and each
write runs after a savepoint of its own, so the next step (and every read)
sees what the steps before it did. A step that fails is rolled back alone
and the sandbox goes on (on PostgreSQL a failed statement would otherwise
spoil the whole transaction). The editor's footer says `SANDBOX · N steps`
in orange. `Ctrl+K x` then shows its steps, the rows each changed and,
per table, what changed, with how long it's been open (its row locks are
held until it ends), and its keys:

| Key | Action |
|---|---|
| `u` | Undo the last step (`ROLLBACK TO SAVEPOINT`), one at a time |
| `d` | Discard it all (`ROLLBACK`); quitting or disconnecting asks, then does the same |
| `s` | Keep its steps as a script in a new tab (a comment says where it came from: `now()`, sequences and random values differ when it runs again), then discard it |
| `r` | Run it for real: discard it, then run the script again in manual commit, through the usual checks (production asks, destructive statements ask), and commit or roll back at the end |

MySQL saves schema changes at once (they commit the transaction), so in a
sandbox there it runs data changes only and refuses DDL with the reason.
On SQLite the sandbox's transaction holds the file's write lock: another
program can't write to it meanwhile.

**Long queries.** A query running 10 minutes says so once in the status
bar, with how to cancel it (`Esc` in its tab), and in a notification while
you're in another window. It never asks a question that would take the
keys you're typing. `long_query` in the settings sets when, `0` never. `query_timeout` cancels any query after that many seconds, on
connections without a Timeout of their own. A query that fails on
something the server is too old for says so after the server's error
(`EXPLAIN ANALYZE needs MySQL 8.0.18 or newer; this server is MySQL
5.7.44`), and so does the syntax check under the editor: MySQL's CTEs,
window functions, `LATERAL`, `JSON_TABLE`, `INTERSECT`, MariaDB's `ANALYZE
SELECT`; PostgreSQL's `MERGE`, `JSON_TABLE`, `ANY_VALUE`, `WITH TIES` and
others.

**AI explain.** `Ctrl+K y` sends the statement at the cursor (or the
selection) to a model with the tables it uses, as asking does; what it
says the statement does, step by step, and what looks wrong in it (with
the query it would write instead) shows in a pop-over. Nothing runs.

**Asking in plain language.** `Ctrl+K q` asks for a question about the
tab's connection ("revenue per country last month"); a model you've set
up ([AI models](configuration.md#ai-models)) writes the SQL, and it lands
at the end of the editor under the question and what the model said about
it, as comments. **It doesn't run**: read it, then `F6` runs it. The status
bar shows the answer coming (`Esc` stops waiting, once it has nothing
else to do: a running query, a filter, a selection or vim's mode come
first; stopping also stops the model's work), and how many tokens it
took. The model sees only the tables that matter for the question, never
rows; its SQL is checked against the schema first, and names still not
found are marked `-- CHECK:` above it. What the model had to assume
(`-- assumed: orders.total means 0 for free orders`) is kept in the
project's memory, not confirmed: `Ctrl+K k`, then `c` if it's right. Facts
in the memory (`podelis memory`) are shown to the model with the tables
they're about. A model that isn't on this machine
sees a project's schema only if you allow it: the first time, a question
asks whether to send it this once (`y`) or always for the project (`a`);
`n` or `Esc` say no. It may come while you type, so keys in its first
moment don't count, and it waits for any other question already open.
Asking works on saved connections, since the gateway opens them itself;
`podelis ask` does the same from the command line. The SQL goes to the
tab you asked in, even if you've moved to another meanwhile (closing that
tab, or switching project, stops the question). What's in the editor is
kept as it is, except that a last statement left without its `;` gets
one, so the two don't run together (not in a MySQL script using
`DELIMITER`).

**Snippets** are saved queries in the `snippets` group (save one with
`Ctrl+S` as `snippets/recent orders`). Typing the start of a snippet's name
offers it in autocomplete, and choosing it inserts its SQL; `:parameters` in
it are asked for when you run.

**Footers.** Each pane has its own: the editor's says what a key or click
there did (copied, nothing to run, a plan waiting for the running query…)
and holds the transaction modes; the results' says the same for keys there,
the run's progress and outcome, then the selected cell. The status bar at
the bottom is for the connection, key hints and everything else. A footer's
message goes with the next key; MESSAGES keeps it.

**Menus.** Right-click a pane, or press `m` in the results or schema
(`Ctrl+K m` or `Shift+F10` anywhere), for a small menu of what you can do
there, each item with its key — a quick way to learn the shortcuts. The
status bar shows the key next to `^K keys`, and the results pane has
`menu ▾` right of its tabs to click, with `export ▾` beside it while rows
are shown: exporting the rows shown or every row as CSV, JSON or `INSERT`s,
copying them, piping them or opening them (in an app, or a pager in the
terminal without a desktop). The editor's menu
starts with Copy and Cut (with text selected) and Paste; the results
pane's, with messages or nothing run yet, offers its tabs and zoom (and
in MESSAGES, copying them).

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
or sort in the way) doesn't count. SQLite knows a table's rows once
`ANALYZE` has counted them (in `sqlite_stat1`); before that its tables
aren't sized, and nothing asks.

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
**connection lost** until the next successful check or query. Without
it, a run after a minute with none (or after *connection lost*) first
checks that the server answers: a link that died meanwhile fails in 5 s,
with nothing run, instead of leaving the query waiting. Once it's
*connection lost*, Podėlis checks again every few seconds (and when you
do something), heartbeat or not, and says when the connection is back. "Copy diagnostics" in the palette
copies versions, terminal and settings for a bug report — no hosts, users
or SQL.

**Pipes and viewers.** `|` sends the rows shown (after filtering and
sorting) to a command's standard input as TSV with a header line — one row
per line, tabs and line breaks inside values written as a space and `\n`,
`NULL` as an empty field. Commands named `jq`, `gron`, `jaq`, `gojq` or `fx`
get JSON instead. The command runs through your shell (`sh -c`), so
pipelines work: `| grep LT | sort | uniq -c`. `o` writes
the rows to a file in the `opened` folder of Podėlis's config folder (yours
alone; files there go after a day) and opens it in its app, which Podėlis
doesn't wait for. A viewer set with `terminal = true` gets the terminal
instead, and Podėlis comes back when it exits.

**Mouse:** click a pane to focus it, a tab to switch to it, `+` for a new
tab (`‹N` / `›N` for the list of them all), the editor to place the cursor
(drag to select; past its top or bottom edge it scrolls, and keeps scrolling while held there), a result cell, a
message or a schema item to select it (clicking the navigator's tree, or
the rows, while typing their filter ends the typing: keys then go to what
was clicked), a result's `#2` to show it; double-click opens a cell or a
message, or expands a table; the wheel scrolls. Your terminal's own text selection then needs `Shift` (or
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
| `/` | Filter tables, views and functions by name (or click the filter line): the name itself first, then names starting with it, then those with a word starting with it (after `_`, `.`, `-`), then the rest, each by name |
| `o` | Order the tables by name, by rows, or by size on disk (indexes included; the size then shows instead of the rows). Both are the server's estimates; SQLite has no sizes, and rows once `ANALYZE` has counted them |
| `Enter` / `→` | Expand a table: its columns (with `→ table.column` for foreign keys) and indexes, scrolled into view; on a function, show its definition |
| `←` | Collapse |
| `d` | Show the definition: `CREATE VIEW`, the function source, or a table's columns, keys and indexes (SQLite: its `CREATE TABLE`). In it, `←`/`→` show the previous / next table's (the navigator's selection follows), `y` copies it |
| `s` | Add `SELECT * FROM <table> LIMIT 100;` at the end of the editor and run it (only it, as `F6` would). A last statement left without its `;` gets one first (before a comment after it), so the two stay apart |
| `f` | Related tables: what the table points to and what points to it through foreign keys, two steps each way |
| `y` / `Ctrl+C` | Copy the selection's name as SQL writes it: a table with its schema (`public.orders`), a column, an index or a function |
| `r` | Refresh (also happens automatically after DDL, `USE` and `SET`) |
| `.` | Show or hide system tables (`pg_catalog` and `information_schema`; MySQL's `information_schema`, `performance_schema`, `mysql` and `sys`; SQLite's `sqlite_schema` and `sqlite_*`) in a SYSTEM section |
| `Space` | The `Ctrl+K` command menu |
