//! SQLite: a file has no password to look for (see `db::passwords`).

use crate::db::ConnectParams;
use crate::db::passwords::Found;

pub(in crate::db) fn from_environment(_params: &ConnectParams, _host: &str) -> Option<Found> {
    None
}
