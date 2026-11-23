//! Another database on the same server, for this tab (`D` in the
//! navigator, a click on its database line, or Ctrl+K): the server's
//! databases in a list to search, and the one picked opened as the tab's
//! connection, with that database. As a connection of its own, it's the
//! same on every engine, and a reconnect keeps it; the tab's other
//! connection stays as it was for the tabs still on it.

use super::palette::{Palette, PaletteMode};
use crate::app::*;

impl App {
    /// Lists the server's databases (asked in the background).
    pub(in crate::app) fn open_database_list(&mut self) {
        let Some(db) = self.connection.database.clone() else {
            return self.set_status(t!("info.not_connected").to_string(), true);
        };
        let send = self.sender();
        tokio::spawn(async move {
            let listed = db.databases().await.map_err(|e| crate::db::error_message(&e));
            send(DbEventKind::DatabasesListed(listed));
        });
    }

    /// The list came: shown, this tab's database picked.
    pub(in crate::app) fn on_databases_listed(&mut self, listed: Result<Option<Vec<String>>, String>) {
        let names = match listed {
            Ok(Some(names)) => names,
            Ok(None) => return self.set_status(t!("palette.no_databases").to_string(), false),
            Err(e) => return self.set_status(t!("palette.databases_failed", e = e), true),
        };
        let here = names.iter().position(|n| *n == self.connection.form.database).unwrap_or(0);
        self.popups.palette = Some(Palette::listing(PaletteMode::Databases(names), here));
    }

    /// A database as its list shows it.
    pub fn database_entry(&self, i: usize) -> String {
        let Some(name) = self.popups.palette.as_ref().and_then(|p| p.database(i)) else { return String::new() };
        if *name == self.connection.form.database { format!("{name}  ({})", t!("menu.this_one")) } else { name.clone() }
    }

    /// Opens database `i` of the list as this tab's connection.
    pub(in crate::app) fn open_database(&mut self, i: usize) {
        let Some(name) = self.popups.palette.as_ref().and_then(|p| p.database(i)).cloned() else { return };
        self.popups.palette = None;
        if name == self.connection.form.database {
            return self.set_status(t!("palette.database_here", name = name), false);
        }
        // Its connection goes from this tab: not under a query or a
        // transaction, open or failed (closing it would roll that back).
        if self.tab.run.executing || self.connection.txn != TxnState::Idle {
            return self.set_status(t!("palette.database_busy").to_string(), true);
        }
        let mut form = self.connection.form.duplicate();
        // Another database than the saved connection's: not that one.
        let saved = self.profiles().iter().find(|p| p.name == form.name).map(|p| p.database.clone());
        form.other_database = saved.is_some_and(|db| db != name);
        form.database.clone_from(&name);
        let params = match form.params() {
            Ok(p) => p,
            Err(e) => return self.set_status(t!("palette.database_failed", name = name, e = e), true),
        };
        self.set_status(t!("palette.opening_database", name = name), false);
        let send = self.sender();
        let short = form.short_name();
        crate::system::connection_log::record("connecting", &short, &form.summary());
        let swap = self.new_swap();
        tokio::spawn(async move {
            match Database::connect(params).await {
                Ok(db) => send(DbEventKind::DatabaseOpened(db, Box::new(form), swap)),
                Err(e) => {
                    crate::system::connection_log::record("connect failed", &short, &crate::db::error_details(&e));
                    send(DbEventKind::DatabaseFailed { name, error: crate::db::error_message(&e), swap });
                }
            }
        });
    }
}
