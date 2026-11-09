//! What's done once a second while something runs: each tab's query (its
//! time limit, saying how to cancel it), watch mode, keepalives, agents'
//! approvals, the sandbox, and noticing a background task that panicked.

use super::*;

impl App {
    /// Called every second while a query runs: enforces the connection's
    /// timeout and notices background tasks that panicked.
    pub fn tick_running(&mut self) {
        self.offer_export_question();
        self.watch_tick();
        self.keepalive_tick();
        self.activity_tick();
        if self.watching_agents() {
            self.approvals_tick();
            self.agents_tick();
        }
        self.share_tick();
        self.sandbox_tick();
        // Each running tab's query: the shown one's, and those in other tabs.
        self.run_tick();
        let shown = self.active_tab;
        for i in self.running_tabs().into_iter().filter(|&i| i != shown) {
            self.in_tab(i, Self::run_tick);
        }
        self.check_background_panic();
    }

    /// The shown tab's running query, once a second: its timeout, and
    /// saying how to cancel it once it's run long.
    fn run_tick(&mut self) {
        if let Some((deadline, limit)) = self.tab.run.extra.deadline
            && self.tab.run.executing
            && Instant::now() >= deadline
        {
            self.tab.run.extra.deadline = None;
            self.tab.run.extra.timed_out = Some(limit);
            self.cancel_query();
        }
        self.offer_cancel_tick();
    }

    /// A query running past `long_query` says, once, how to cancel it: in
    /// the status bar (and a notification while you're in another window),
    /// never as a question that takes the keys being typed.
    fn offer_cancel_tick(&mut self) {
        if !self.tab.run.executing {
            self.tab.run.extra.offered_cancel = false;
            return;
        }
        let after = self.settings.long_query;
        let long = self.tab.run.since.is_some_and(|s| s.elapsed() >= Duration::from_secs(after));
        if after == 0 || self.tab.run.cancelling || self.tab.run.extra.offered_cancel || !long {
            return;
        }
        self.tab.run.extra.offered_cancel = true;
        let secs = self.tab.run.since.map_or(0, |s| s.elapsed().as_secs());
        let tab = self.run_tab().unwrap_or(self.active_tab);
        let name = self.tabs.get(tab).map(|t| t.name.clone()).unwrap_or_default();
        let on = self.tabs.get(tab).and_then(|t| t.conn).and_then(|id| self.conn_name(id)).unwrap_or_default();
        let said = t!("run.long_running", time = mm_ss(secs), tab = name, on = on);
        self.set_status(said.clone(), false);
        if self.terminal_focused != Some(true) {
            self.for_terminal.notification = Some(said);
        }
    }

    /// Reports a background task that panicked (it never reports back).
    fn check_background_panic(&mut self) {
        if let Some(message) = crate::system::crash::take_background() {
            self.tab.run.executing = false;
            self.tab.run.task = None;
            self.tab.run.set_stream(None);
            let log = crate::system::crash::log_path();
            self.set_status(t!("run.internal_error", message = message, log = log.display()), true);
        }
    }
}
