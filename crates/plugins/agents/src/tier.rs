//! Which detection tier wins (REQ-AGT-004): a higher tier always overrides a lower one; a lower
//! tier overrides a higher one only once the higher tier's state is `hold_ms` old, and then
//! only with a new observation (0.11.0): the same quiet process read again is not one.

/// `agent.source` values in tier order (REQ-AGT-001..003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Hook,
    Osc,
    Process,
    Heuristic,
}

impl Source {
    pub fn tier(self) -> u8 {
        match self {
            Source::Hook | Source::Osc => 1,
            Source::Process => 2,
            Source::Heuristic => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Hook => "hook",
            Source::Osc => "osc",
            Source::Process => "process",
            Source::Heuristic => "heuristic",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        Some(match s {
            "hook" => Source::Hook,
            "osc" => Source::Osc,
            "process" => Source::Process,
            "heuristic" => Source::Heuristic,
            _ => return None,
        })
    }
}

/// `agent.state` values (SPEC §9.1).
pub const STATES: &[&str] = &["idle", "working", "blocked", "done", "error", "exited"];

pub fn valid_state(s: &str) -> bool {
    STATES.contains(&s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgment {
    pub state: String,
    pub source: Source,
    /// When this state began (ms since epoch).
    pub since: u64,
    /// When this tier last confirmed it.
    pub at: u64,
    /// The pane as last observed when the state was taken ([`Tracker::observe`]).
    pub seen: (u64, u32),
}

#[derive(Debug, Clone, Default)]
pub struct Tracker {
    pub current: Option<Judgment>,
    /// The latest observation: last output time and foreground pid.
    seen: (u64, u32),
}

impl Tracker {
    /// What the pane looks like now: when it last wrote output and which process is in its
    /// foreground. A lower tier may only override a state taken before the last change.
    pub fn observe(&mut self, last_output: u64, fg_pid: u32) {
        self.seen = (last_output, fg_pid);
    }

    /// Offers a state from `source`. Returns whether it changed what the pane shows.
    pub fn propose(&mut self, state: &str, source: Source, now: u64, hold_ms: u64) -> bool {
        let seen = self.seen;
        match &mut self.current {
            Some(cur) => {
                let outranked = source.tier() > cur.source.tier();
                // 0.11.0: past the hold, only something new since the state was taken.
                let fresh = cur.seen != seen;
                if outranked && (now.saturating_sub(cur.at) < hold_ms || !fresh) {
                    return false;
                }
                if cur.state == state && cur.source == source {
                    cur.at = now;
                    return false;
                }
                let since = if cur.state == state { cur.since } else { now };
                *cur = Judgment {
                    state: state.to_owned(),
                    source,
                    since,
                    at: now,
                    seen,
                };
                true
            }
            None => {
                self.current = Some(Judgment {
                    state: state.to_owned(),
                    source,
                    since: now,
                    at: now,
                    seen,
                });
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agt_004_higher_tiers_override_and_lower_ones_wait_out_the_hold() {
        let mut t = Tracker::default();
        assert!(t.propose("working", Source::Process, 0, 5000));
        // Tier 1 overrides tier 2 at once.
        assert!(t.propose("done", Source::Hook, 100, 5000));
        // Tier 2 cannot override a fresh tier-1 state...
        assert!(!t.propose("working", Source::Process, 3000, 5000));
        assert!(!t.propose("blocked", Source::Heuristic, 5099, 5000));
        assert_eq!(t.current.as_ref().unwrap().state, "done");
        // ...nor an old one by repeating what it saw before (0.11.0)...
        assert!(!t.propose("blocked", Source::Process, 60_000, 5000));
        assert_eq!(t.current.as_ref().unwrap().source, Source::Hook);
        // ...only with something new, once the hold is over.
        t.observe(59_000, 42);
        assert!(t.propose("working", Source::Process, 60_100, 5000));
        assert_eq!(t.current.as_ref().unwrap().source, Source::Process);
        // The same tier replaces itself freely, fresh or not; repeats only refresh.
        assert!(t.propose("blocked", Source::Process, 60_200, 5000));
        assert!(!t.propose("blocked", Source::Process, 60_300, 5000));
        assert_eq!(t.current.as_ref().unwrap().since, 60_200);
        assert_eq!(t.current.as_ref().unwrap().at, 60_300);
        // Tier 1 sources (hook and osc) replace each other.
        assert!(t.propose("working", Source::Osc, 60_400, 5000));
        assert!(t.propose("idle", Source::Hook, 60_500, 5000));
    }
}
