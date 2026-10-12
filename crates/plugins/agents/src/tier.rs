//! Which detection tier wins (REQ-AGT-004): a higher tier always overrides a lower one; a lower
//! tier overrides a higher one only once the higher tier's state is `hold_ms` old.

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
}

#[derive(Debug, Clone, Default)]
pub struct Tracker {
    pub current: Option<Judgment>,
}

impl Tracker {
    /// Offers a state from `source`. Returns whether it changed what the pane shows.
    pub fn propose(&mut self, state: &str, source: Source, now: u64, hold_ms: u64) -> bool {
        match &mut self.current {
            Some(cur) => {
                let outranked = source.tier() > cur.source.tier();
                if outranked && now.saturating_sub(cur.at) < hold_ms {
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
                };
                true
            }
            None => {
                self.current = Some(Judgment {
                    state: state.to_owned(),
                    source,
                    since: now,
                    at: now,
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
        // ...until it is hold_ms old.
        assert!(t.propose("working", Source::Process, 5100, 5000));
        assert_eq!(t.current.as_ref().unwrap().source, Source::Process);
        // The same tier replaces itself freely; repeats only refresh.
        assert!(t.propose("blocked", Source::Process, 5200, 5000));
        assert!(!t.propose("blocked", Source::Process, 5300, 5000));
        assert_eq!(t.current.as_ref().unwrap().since, 5200);
        assert_eq!(t.current.as_ref().unwrap().at, 5300);
        // Tier 1 sources (hook and osc) replace each other.
        assert!(t.propose("working", Source::Osc, 5400, 5000));
        assert!(t.propose("idle", Source::Hook, 5500, 5000));
    }
}
