//! MongoDB: where its own tools keep a password (see `db::passwords`):
//! nowhere Podėlis reads (mongosh takes it in the URL or asks).

use crate::db::ConnectParams;
use crate::db::passwords::Found;

pub(in crate::db) fn from_environment(_params: &ConnectParams, _host: &str) -> Option<Found> {
    None
}
