//! PostgreSQL's own places for a password: `PGPASSWORD` and `~/.pgpass`
//! (see `db::passwords`).

use sqlx::postgres::PgConnectOptions;

use crate::db::ConnectParams;
use crate::db::passwords::{Found, env, found, shown};

pub(in crate::db) fn from_environment(params: &ConnectParams, host: &str) -> Option<Found> {
    if let Some(password) = env("PGPASSWORD") {
        return Some(found(password, "PGPASSWORD"));
    }
    // Unset fields are the driver's defaults (PGUSER, the OS user;
    // PGDATABASE, else the user's name), as libpq matches them.
    let defaults = PgConnectOptions::new_without_pgpass();
    let user = if params.user.is_empty() { defaults.get_username() } else { &params.user };
    let database = if params.database.is_empty() { defaults.get_database().unwrap_or(user) } else { &params.database };
    let (password, path) = super::pgpass::password(host, params.port, user, database)?;
    Some(found(password, &shown(&path)))
}
