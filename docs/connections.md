# Projects and connections

[← Podėlis](../README.md)

One screen with three panes, side by side: **projects**, the open
project's **connections**, and the selected connection's **settings**.
`Ctrl+K p` or clicking the project's name in the top bar shows it
with the projects focused; `Ctrl+K o` with the connections focused.
`Tab` / `Shift+Tab` move to the next / previous pane, round all three
(out of the settings, unsaved changes are dropped). `←`/`→` move between the
lists too, and `Esc` goes back to your tabs as you left them. A tab opened to pick a connection (`Ctrl+K o`
on a connected tab), or one you disconnected, is dropped on the way back
if it's still empty and unconnected. A `●` marks the open project and the
connections open in tabs. On a narrow terminal the focused pane and the
next one show. Clicking a row selects it; clicking it again (or a double
click) opens it, and clicking a field in the settings focuses it.
Quitting from here (`q`, `Ctrl+Q`, `Ctrl+C`) asks first if a transaction is open.

## Projects

| Key | Action |
|---|---|
| `↑`/`↓` | Move selection (the open project, or the last one you opened, is preselected) |
| `Enter` | Open the project (its tabs come back and reconnect, as you left them), or create a new one. Switching saves this project's tabs and closes its connections (asks if a transaction is open). On the open project, `Enter` goes to its connections |
| `→` / `Tab` | The project's connections, opening it first if it isn't the open one (its tabs come back behind them) |
| `n` | New project |
| `r` | Rename project |
| `d` | Delete project, with all its connections and their saved passwords (asks for confirmation; not the open one) |
| `Esc` | Back to the project's tabs (with none open: to the project you left, or quit) |
| `q` | Quit (asks if a transaction is open) |

## Connections

Another project selected shows its connections, to look at; `→` opens it.

| Key | Action |
|---|---|
| `↑`/`↓` | Move selection; the settings pane shows the selected connection |
| `Enter` | Connect (in a new tab if this one is connected), or add a new connection |
| `e` / `→` | Edit the connection's settings in the settings pane (your tab keeps its connection) |
| `n` | New connection |
| `d` | Delete selected connection (asks for confirmation) |
| `m` | Move selected connection (and its saved password) to another project |
| `c` | Find running PostgreSQL and MySQL containers (Docker or Podman): `↵` fills in a new connection from one, with its published port, user and database, and a password command that reads the password from the container (`docker exec … printenv POSTGRES_PASSWORD`) rather than a copy |
| `←` | Back to the projects |
| `Esc` | Back to the project's tabs (connected or not) |
| `q` | Quit (asks if a transaction is open) |

## Settings

While editing or adding a connection:

| Key | Action |
|---|---|
| `↑`/`↓` | Move between fields (or click one) |
| `←`/`→` | Change Engine, SSL mode, Mode or Transaction when that field is focused |
| `Enter` | Connect (in a new tab if this one is connected; the tab you were on keeps its connection) |
| `Ctrl+S` | Save this connection (settings locally; password in the OS keychain, unless a password command supplies it). Changing the name and saving renames the profile. A new connection is also saved once it connects (unnamed: after its host/database or file) |
| `Ctrl+T` | Test the connection step by step: SSH, DNS, TCP, login, TLS, session, latency — each with its timing, the failing step with a hint |
| `Ctrl+Y` | Copy the focused field's value (not the password) |
| `Ctrl+V` | Paste into the focused field from the system clipboard (`pbpaste`, `wl-paste`, `xclip` or `xsel`), for terminals where `Ctrl+V` doesn't paste; the terminal's own paste works too. A pasted connection URL fills in the whole form |
| `Esc` | Cancel a connection attempt in progress, otherwise back to the connections (unsaved changes are dropped) |

- **Host** can be a hostname, an IP address (IPv6 with or without brackets)
  or a Unix socket path starting with `/`. Leave User/Password empty to use
  the driver defaults (e.g. your OS user for Postgres). For **SQLite** the
  form asks only for a file path or `:memory:`. A path that doesn't exist
  is usually a typo, so the form says so and creates a new database only
  when you press Enter again.
- **SSL mode:** `prefer` (the default) uses TLS when the server offers it.
  If the server offers TLS this client can't use (an old MySQL 5.x that
  speaks only TLS 1.0/1.1), it connects without encryption and warns: in
  the status bar and MESSAGES, under `Ctrl+K u`, and in the `Ctrl+T` test.
  `require` and the `verify-*` modes never fall back; `disable` doesn't try.
- **Passwd cmd:** optional. A command that prints the password, run each
  time the connection opens: `pass show db/prod`, `op read
  op://Ops/prod-db/password`, `vault kv get -field=password secret/db/prod`,
  `aws rds generate-db-auth-token …`. It replaces the Password field (and
  the keychain). It runs without a shell (quote words with spaces), gets no
  input (a password manager asks in its own window), and has 60 seconds.
  The first line it prints is the password. It gets the connection's
  details in `PODELIS_ENGINE`, `PODELIS_HOST`, `PODELIS_PORT`,
  `PODELIS_USER` and `PODELIS_DATABASE`, so one script can serve several
  connections. In `connections.toml` it's a list of words:
  `password_command = ["pass", "show", "db/prod"]`.

  With **`pass` and GPG**, the passphrase has to come from gpg-agent's
  cache or a graphical pinentry (`pinentry-mac`, `pinentry-gnome3`,
  `pinentry-qt`): a terminal one (`pinentry-curses`, `pinentry-tty`) can't
  ask while Podėlis has the screen. On a server over SSH, unlock the agent
  first (`pass show db/prod >/dev/null` in your shell); `default-cache-ttl`
  and `max-cache-ttl` in `~/.gnupg/gpg-agent.conf` set how long it stays
  unlocked.
- **Mode:** `read-write`, `production` or `read-only` (see Safety in the
  [features](../README.md#features)). Read-only is enforced by the server too:
  `default_transaction_read_only` on Postgres, `SET SESSION TRANSACTION
  READ ONLY` on MySQL, a read-only open on SQLite.
- **Transaction:** the transaction mode the connection starts in:
  `auto-commit`, `manual commit` or `rollback-only`. Unless you pick one,
  production connections start in manual commit and others in
  auto-commit. `Ctrl+K x` switches it while connected.
- **Timeout:** optional, in seconds. A query running longer is cancelled
  on the server (PostgreSQL, MySQL).
- **SSH via:** optional. `user@host`, `user@host:port` or a host alias from
  `~/.ssh/config`. Podėlis runs your system `ssh -N -L …`, so your keys,
  agent, `~/.ssh/config` and known_hosts all apply. It can't answer
  password or host-key prompts, so use key authentication and connect to the
  host once from a shell first. Host and Port are as seen *from* the SSH
  host (often `localhost`). Through a tunnel, `verify-full` SSL can't match
  the server's hostname; use `require`.

## Databases in Docker

A container's published port (`-p 5433:5432`) is
reached at `localhost` and that port:

- **The wrong server answering:** if a local PostgreSQL or MySQL already
  uses the port, `localhost:5432` reaches it, not the container, and the
  login fails with the container's password. Publish the container on
  another port.
- **Just started:** "connection refused" or "the database system is
  starting up" until the server is ready; try again in a few seconds.
- **SSL:** the official images don't enable TLS, so SSL mode `require`
  fails; use `prefer` or `disable`.
- **The password from the container**, without saving it, as a password
  command: `docker exec my-postgres printenv POSTGRES_PASSWORD` (or
  `MYSQL_ROOT_PASSWORD`). It needs access to Docker, which is as good as
  root on that machine: for development containers.
- **Password files:** a published container counts as this machine, so a
  `localhost` line in `~/.pgpass` and a `~/.my.cnf` that names no host are
  used for it. Running images you don't trust? Name the `host` in
  `~/.my.cnf`, so its password only goes to the server it's for.
- **From inside Compose,** use the service name (`db`) as Host; in
  `~/.my.cnf` that's `host = db`. From inside a container to a database
  on your own machine (Docker Desktop): `host.docker.internal`.
