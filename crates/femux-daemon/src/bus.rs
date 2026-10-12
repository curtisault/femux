//! The event bus (REQ-EVT-001..003): every event gets a daemon-wide `seq` and a `ts`, the last
//! `replay` events are retained for `since`, and each subscriber has a bounded queue. A
//! subscriber that falls behind loses events and is told so with `events_lost{from, to}`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use femux_proto::Event;
use femux_proto::msg::{EventsLost, EventsSubscribe};
use tokio::sync::Notify;

/// What an event is about, for subscription filters.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub session: Option<String>,
    pub pane: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    kinds: Option<Vec<String>>,
    session: Option<String>,
    pane: Option<String>,
}

impl Filter {
    pub fn from_request(r: &EventsSubscribe) -> Filter {
        Filter {
            kinds: r.kinds.clone().filter(|k| !k.is_empty()),
            session: r.session.clone(),
            pane: r.pane.clone(),
        }
    }

    fn matches(&self, e: &Event, scope: &Scope) -> bool {
        let kind_ok = self
            .kinds
            .as_ref()
            .is_none_or(|ks| ks.iter().any(|k| k == e.kind()));
        // Server-wide events (no session scope, e.g. clipboard.changed from clipboard.set)
        // reach every subscriber whatever its session filter.
        let session_ok = self
            .session
            .as_ref()
            .is_none_or(|s| scope.session.as_ref().is_none_or(|x| x == s));
        let pane_ok = self
            .pane
            .as_ref()
            .is_none_or(|p| scope.pane.as_ref() == Some(p));
        kind_ok && session_ok && pane_ok
    }
}

struct Retained {
    event: Event,
    scope: Scope,
}

struct Sub {
    filter: Filter,
    queue: VecDeque<Event>,
    /// Lost seq ranges not yet reported.
    lost: Vec<(u64, u64)>,
    /// The queue overflowed; everything until the next `take` is lost.
    overflowing: bool,
    notify: Arc<Notify>,
}

pub struct Bus {
    seq: u64,
    replay: VecDeque<Retained>,
    replay_cap: usize,
    buffer_cap: usize,
    subs: HashMap<u64, Sub>,
    next_sub: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Bus {
    pub fn new(buffer_cap: usize, replay_cap: usize) -> Bus {
        Bus {
            seq: 0,
            replay: VecDeque::new(),
            replay_cap,
            buffer_cap: buffer_cap.max(1),
            subs: HashMap::new(),
            next_sub: 1,
        }
    }

    /// The seq of the newest event.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn emit(&mut self, event: impl Into<Event>, scope: Scope) {
        let mut event = event.into();
        self.seq += 1;
        event.stamp(self.seq, now_ms());
        for sub in self.subs.values_mut() {
            if sub.filter.matches(&event, &scope) {
                push(sub, &event, self.buffer_cap);
            }
        }
        if self.replay_cap > 0 {
            if self.replay.len() == self.replay_cap {
                self.replay.pop_front();
            }
            self.replay.push_back(Retained { event, scope });
        }
    }

    /// Adds a subscriber; with `since`, first queues retained events newer than it (REQ-EVT-003).
    pub fn subscribe(&mut self, filter: Filter, since: Option<u64>) -> (u64, Arc<Notify>) {
        let id = self.next_sub;
        self.next_sub += 1;
        let notify = Arc::new(Notify::new());
        let mut sub = Sub {
            filter,
            queue: VecDeque::new(),
            lost: Vec::new(),
            overflowing: false,
            notify: notify.clone(),
        };
        if let Some(since) = since {
            let oldest = self.replay.front().map_or(self.seq + 1, |r| r.event.seq());
            if since + 1 < oldest && since < self.seq {
                sub.lost.push((since + 1, oldest - 1));
            }
            for r in self.replay.iter().filter(|r| r.event.seq() > since) {
                if sub.filter.matches(&r.event, &r.scope) {
                    push(&mut sub, &r.event, self.buffer_cap);
                }
            }
            if !sub.lost.is_empty() || !sub.queue.is_empty() {
                notify.notify_one();
            }
        }
        self.subs.insert(id, sub);
        (id, notify)
    }

    pub fn unsubscribe(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Drains a subscriber's queue; an `events_lost` comes first if events were dropped.
    pub fn take(&mut self, id: u64) -> Vec<Event> {
        let Some(sub) = self.subs.get_mut(&id) else {
            return Vec::new();
        };
        let mut out: Vec<Event> = sub.queue.drain(..).collect();
        sub.overflowing = false;
        // events_lost is per subscriber; it carries the seq of the last lost event and sits
        // where its gap is in seq order.
        for (from, to) in sub.lost.drain(..) {
            let lost = Event::EventsLost(EventsLost {
                seq: to,
                ts: now_ms(),
                from,
                to,
            });
            let at = out.iter().position(|e| e.seq() > to).unwrap_or(out.len());
            out.insert(at, lost);
        }
        out
    }
}

fn push(sub: &mut Sub, e: &Event, cap: usize) {
    let s = e.seq();
    if sub.overflowing {
        if let Some(last) = sub.lost.last_mut() {
            last.1 = s;
        }
    } else if sub.queue.len() >= cap {
        sub.overflowing = true;
        sub.lost.push((s, s));
    } else {
        sub.queue.push_back(e.clone());
    }
    sub.notify.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use femux_proto::msg::{PaneBell, SessionCreated};

    fn created(name: &str) -> SessionCreated {
        SessionCreated {
            name: name.into(),
            ..Default::default()
        }
    }

    fn scope(s: &str) -> Scope {
        Scope {
            session: Some(s.into()),
            pane: None,
        }
    }

    #[test]
    fn seq_is_monotonic_and_stamped() {
        let mut bus = Bus::new(10, 10);
        let (id, _) = bus.subscribe(Filter::default(), None);
        bus.emit(created("a"), scope("a"));
        bus.emit(created("b"), scope("b"));
        let evs = bus.take(id);
        assert_eq!(evs.iter().map(Event::seq).collect::<Vec<_>>(), [1, 2]);
        match &evs[0] {
            Event::SessionCreated(e) => assert!(e.ts > 1_700_000_000_000),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn filters_by_kind_session_and_pane() {
        let mut bus = Bus::new(10, 10);
        let f = |kinds: Option<Vec<&str>>, session: Option<&str>, pane: Option<&str>| {
            Filter::from_request(&EventsSubscribe {
                kinds: kinds.map(|k| k.into_iter().map(String::from).collect()),
                session: session.map(String::from),
                pane: pane.map(String::from),
                since: None,
            })
        };
        let (k, _) = bus.subscribe(f(Some(vec!["pane.bell"]), None, None), None);
        let (s, _) = bus.subscribe(f(None, Some("b"), None), None);
        let (p, _) = bus.subscribe(f(None, None, Some("P1")), None);
        bus.emit(created("a"), scope("a"));
        bus.emit(created("b"), scope("b"));
        bus.emit(
            PaneBell {
                pane_id: "P1".into(),
                ..Default::default()
            },
            Scope {
                session: Some("a".into()),
                pane: Some("P1".into()),
            },
        );
        assert_eq!(bus.take(k).iter().map(Event::seq).collect::<Vec<_>>(), [3]);
        assert_eq!(bus.take(s).iter().map(Event::seq).collect::<Vec<_>>(), [2]);
        assert_eq!(bus.take(p).iter().map(Event::seq).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn slow_subscriber_gets_events_lost_and_continues() {
        let mut bus = Bus::new(3, 100);
        let (id, _) = bus.subscribe(Filter::default(), None);
        for i in 0..10 {
            bus.emit(created(&i.to_string()), scope("x"));
        }
        let evs = bus.take(id);
        assert_eq!(
            evs[..3].iter().map(Event::seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        match &evs[3] {
            Event::EventsLost(l) => assert_eq!((l.from, l.to), (4, 10)),
            e => panic!("{e:?}"),
        }
        bus.emit(created("next"), scope("x"));
        assert_eq!(
            bus.take(id).iter().map(Event::seq).collect::<Vec<_>>(),
            [11]
        );
    }

    #[test]
    fn late_subscriber_replays_since() {
        let mut bus = Bus::new(100, 5);
        for i in 0..8 {
            bus.emit(created(&i.to_string()), scope("x"));
        }
        let (id, _) = bus.subscribe(Filter::default(), Some(5));
        assert_eq!(
            bus.take(id).iter().map(Event::seq).collect::<Vec<_>>(),
            [6, 7, 8]
        );
        // Older than the replay window: the gap is reported first.
        let (id, _) = bus.subscribe(Filter::default(), Some(1));
        let evs = bus.take(id);
        match &evs[0] {
            Event::EventsLost(l) => assert_eq!((l.from, l.to), (2, 3)),
            e => panic!("{e:?}"),
        }
        assert_eq!(
            evs[1..].iter().map(Event::seq).collect::<Vec<_>>(),
            [4, 5, 6, 7, 8]
        );
    }
}
