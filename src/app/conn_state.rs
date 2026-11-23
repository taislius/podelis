//! Where a connection stands ([`ConnState`], a state machine only its own
//! steps change). Each connection slot (`session::Connection`) has one;
//! the status bar, the navigator, the tabs and the reopening card read it.
//! Keeping an idle connection alive is its supervisor's (podelis-core's
//! `db::supervisor`), whose reports move it.

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
}
