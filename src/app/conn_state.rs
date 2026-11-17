//! Where a connection stands ([`ConnState`], a state machine only its own
//! steps change) and keeping an idle one alive ([`KeepAlive`]: a check's
//! timing). Each connection slot (`session::Connection`) has both; the
//! status bar, the navigator, the tabs and the reopening card read them.

use std::time::{Duration, Instant};

/// Where a connection stands: the one thing the status bar, the
/// navigator, the tabs and the reopening card read, and only
/// [`ConnState`]'s own steps change. A slot is made as its connection
/// starts opening (its tabs point at it from then), so a late answer is
/// told from the one awaited by `attempt`, not by a name.
///
/// ```text
/// Opening ──ok──▶ Ready{unverified} ──tables / check / query──▶ Ready{verified}
///    │                 │  ▲                                          │
///    └──err──▶ Failed  │  └───────────── answered ◀── Lost ◀──no answer
///                      └──tables didn't load──▶ Degraded ◀─answered─┘
///                                                  (lost while its tables
///                                                   hadn't loaded: kept)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    /// Being opened: `attempt` is which opening of this slot it is.
    Opening { attempt: u64 },
    /// Open; `verified` once something answered on it since (its tables
    /// loaded, a check, a query): before that it's only logged in.
    Ready { verified: bool },
    /// Open, but its tables didn't load (why): the server let the login
    /// in, then didn't answer, or refused.
    Degraded { why: String },
    /// Open, but a check got no answer: the network or the server is gone.
    /// `tables`: why its tables hadn't loaded, if they hadn't (it's
    /// [`Degraded`](Self::Degraded) again once it answers).
    Lost { tables: Option<String> },
    /// Couldn't be opened (why); a run on its tab tries again.
    Failed { why: String },
}

impl Default for ConnState {
    fn default() -> Self {
        ConnState::Ready { verified: false }
    }
}

impl ConnState {
    pub fn is_lost(&self) -> bool {
        matches!(self, ConnState::Lost { .. })
    }

    pub fn is_opening(&self) -> bool {
        matches!(self, ConnState::Opening { .. })
    }

    /// Open: it has a database (lost, or its tables not loaded, but open).
    pub fn is_open(&self) -> bool {
        matches!(self, ConnState::Ready { .. } | ConnState::Degraded { .. } | ConnState::Lost { .. })
    }

    /// Why it couldn't be opened, if it couldn't.
    pub fn failure(&self) -> Option<&str> {
        if let ConnState::Failed { why } = self { Some(why) } else { None }
    }

    /// Something answered on it: it's back if it was lost, and verified.
    /// (Its tables not loading still stands, through a loss too.)
    pub(super) fn answered(&mut self) {
        *self = match std::mem::replace(self, ConnState::Ready { verified: true }) {
            ConnState::Ready { .. } | ConnState::Lost { tables: None } => ConnState::Ready { verified: true },
            ConnState::Lost { tables: Some(why) } => ConnState::Degraded { why },
            other => other,
        };
    }

    /// A check or a query found it gone (whether its tables had loaded is
    /// kept).
    pub(super) fn lose(&mut self) {
        *self = match std::mem::replace(self, ConnState::Lost { tables: None }) {
            ConnState::Ready { .. } => ConnState::Lost { tables: None },
            ConnState::Degraded { why } => ConnState::Lost { tables: Some(why) },
            other => other,
        };
    }

    #[cfg(test)]
    pub fn lose_for_tests(&mut self) {
        self.lose();
    }

    /// Its tables loaded (`Ok`: it answered, and they're there), or why
    /// not. A lost one stays lost (still checked until it's back), noting
    /// why its tables didn't load.
    pub(super) fn tables_loaded(&mut self, outcome: Result<(), &str>) {
        match outcome {
            Ok(()) if self.is_open() => *self = ConnState::Ready { verified: true },
            Err(why) if self.is_lost() => *self = ConnState::Lost { tables: Some(why.to_string()) },
            Err(why) if self.is_open() => *self = ConnState::Degraded { why: why.to_string() },
            _ => {}
        }
    }
}

/// Keeping an idle connection alive: when it was last used, and the check
/// running on it. Whether it's lost is its [`ConnState`]'s.
pub struct KeepAlive {
    /// When the connection was last used.
    pub(super) last_activity: Instant,
    /// A check is running.
    pinging: bool,
    /// The user explicitly asked to check a lost connection.
    reconnect_requested: bool,
    /// Checks that found it lost in a row: each waits longer for the next.
    lost_checks: u32,
}

impl KeepAlive {
    pub(super) fn pinging(&self) -> bool {
        self.pinging
    }

    pub(super) fn begin_check(&mut self) {
        self.pinging = true;
    }

    pub(super) fn begin_reconnect(&mut self) {
        self.reconnect_requested = true;
    }

    /// Completes a check: whether the user asked for it.
    pub(super) fn finish_check(&mut self) -> bool {
        self.pinging = false;
        std::mem::take(&mut self.reconnect_requested)
    }

    /// A check found it lost (`true`) or answering: the next of a lost
    /// one's checks waits longer each time.
    pub(super) fn checked(&mut self, lost: bool) {
        self.lost_checks = if lost { self.lost_checks.saturating_add(1) } else { 0 };
    }

    /// How long a lost connection waits for its next check: 5 s, then
    /// twice as long each time it's still lost, up to a minute (a network
    /// that's down isn't asked every few seconds for an hour).
    pub fn next_lost_check(&self) -> Duration {
        let first = Duration::from_secs(5);
        let factor = 1u32 << self.lost_checks.saturating_sub(1).min(4);
        (first * factor).min(Duration::from_secs(60))
    }

    /// Seconds until a lost connection's next check (0: due, or running).
    pub fn lost_check_in(&self) -> u64 {
        if self.pinging {
            return 0;
        }
        self.next_lost_check().saturating_sub(self.last_activity.elapsed()).as_secs()
    }
}

impl Default for KeepAlive {
    fn default() -> Self {
        Self { last_activity: Instant::now(), pinging: false, reconnect_requested: false, lost_checks: 0 }
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn a_connection_goes_through_its_states_by_its_own_steps() {
        let mut state = ConnState::Opening { attempt: 1 };
        assert!(!state.is_open());
        // Opening isn't open: neither lost nor back by a check.
        state.lose();
        state.answered();
        assert_eq!(state, ConnState::Opening { attempt: 1 });
        state = ConnState::default();
        assert_eq!(state, ConnState::Ready { verified: false }, "logged in, nothing answered yet");
        state.tables_loaded(Err("timed out"));
        assert_eq!(state, ConnState::Degraded { why: "timed out".into() });
        state.answered();
        assert!(matches!(state, ConnState::Degraded { .. }), "a check doesn't say its tables loaded");
        state.tables_loaded(Ok(()));
        assert_eq!(state, ConnState::Ready { verified: true });
        state.lose();
        assert!(state.is_lost() && state.is_open());
        state.answered();
        assert_eq!(state, ConnState::Ready { verified: true }, "back");
        // Lost while its tables hadn't loaded: back, they still haven't.
        state.tables_loaded(Err("timed out"));
        state.lose();
        assert_eq!(state, ConnState::Lost { tables: Some("timed out".into()) });
        state.answered();
        assert_eq!(state, ConnState::Degraded { why: "timed out".into() });
        // Its tables failing while it's lost: still lost, still checked.
        state.lose();
        state.tables_loaded(Err("no answer in 30 s"));
        assert!(state.is_lost(), "{state:?}");
        state.tables_loaded(Ok(()));
        assert_eq!(state, ConnState::Ready { verified: true }, "they loaded: it answered too");
        let failed = ConnState::Failed { why: "refused".into() };
        assert_eq!(failed.failure(), Some("refused"));
        assert!(!failed.is_open());
    }

    #[test]
    fn a_check_keeps_only_its_own_timing_and_intent() {
        let mut keepalive = KeepAlive::default();
        keepalive.begin_check();
        keepalive.begin_reconnect();
        assert!(keepalive.pinging());
        assert!(keepalive.finish_check(), "asked for");
        assert!(!keepalive.pinging());
        keepalive.begin_check();
        assert_eq!(keepalive.lost_check_in(), 0, "a check running: not a countdown");
        assert!(!keepalive.finish_check(), "the intent went with the check that had it");
    }

    #[test]
    fn a_lost_connection_is_checked_less_often_the_longer_it_stays_lost() {
        let mut keepalive = KeepAlive::default();
        let waits: Vec<u64> = (0..8)
            .map(|_| {
                keepalive.checked(true);
                keepalive.next_lost_check().as_secs()
            })
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60, 60]);
        keepalive.checked(false);
        keepalive.checked(true);
        assert_eq!(keepalive.next_lost_check().as_secs(), 5, "back, then lost again: from the start");
    }
}
