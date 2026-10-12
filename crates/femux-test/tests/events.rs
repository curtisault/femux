//! REQ-EVT-*: sequence numbers, filters, loss reporting, replay and pane metadata.

use std::time::Duration;

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::Harness;

#[tokio::test(flavor = "multi_thread")]
async fn evt_001_seq_ts_kind() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(EventsSubscribe::default()).await.unwrap();
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    c.call(SessionRename {
        name: "s".into(),
        new: "t".into(),
    })
    .await
    .unwrap();
    c.wait_event("session.renamed").await;
    let seqs: Vec<u64> = c.events.iter().map(Event::seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "{seqs:?}");
    for e in &c.events {
        let v = serde_json::to_value(match e {
            Event::SessionCreated(b) => serde_json::to_value(b).unwrap(),
            Event::SessionRenamed(b) => serde_json::to_value(b).unwrap(),
            _ => continue,
        })
        .unwrap();
        assert!(v["ts"].as_u64().unwrap() > 1_700_000_000_000);
    }
    assert_eq!(
        c.events
            .iter()
            .map(Event::kind)
            .filter(|k| k.starts_with("session."))
            .collect::<Vec<_>>(),
        ["session.created", "session.renamed"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn evt_002_filters_and_loss() {
    // Loss reporting with a stalled reader is covered by protocol `proto_041_full_frame_after_attach_and_events_lost`
    // and femux-daemon `bus::tests::slow_subscriber_gets_events_lost_and_continues`.
    let h = Harness::start();
    let mut all = h.client().await;
    all.call(SessionCreate {
        name: "a".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    all.call(SessionCreate {
        name: "b".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let pa = all
        .call(SessionAttach {
            name: "a".into(),
            tab: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot
        .tabs[0]
        .focus
        .clone();
    let mut by_kind = h.client().await;
    by_kind
        .call(EventsSubscribe {
            kinds: Some(vec!["pane.meta_changed".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut by_session = h.client().await;
    by_session
        .call(EventsSubscribe {
            session: Some("b".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut by_pane = h.client().await;
    by_pane
        .call(EventsSubscribe {
            pane: Some(pa.clone()),
            ..Default::default()
        })
        .await
        .unwrap();

    all.call(PaneSetMeta {
        pane_id: pa.clone(),
        set: Some([("x".into(), "1".into())].into()),
        unset: None,
    })
    .await
    .unwrap();
    all.call(TabCreate {
        session: "b".into(),
        cwd: None,
        cmd: None,
        title: None,
        index: None,
    })
    .await
    .unwrap();
    for c in [&mut by_kind, &mut by_session, &mut by_pane] {
        while c.recv(Duration::from_millis(300)).await {}
    }
    let kinds = |c: &femux_test::Client| {
        c.events
            .iter()
            .map(|e| e.kind().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&by_kind), ["pane.meta_changed"]);
    let b = kinds(&by_session);
    assert!(
        b.contains(&"tab.created".to_owned()) && b.contains(&"pane.spawned".to_owned()),
        "{b:?}"
    );
    assert!(!b.contains(&"pane.meta_changed".to_owned()), "{b:?}");
    assert!(kinds(&by_pane).contains(&"pane.meta_changed".to_owned()));
    assert!(kinds(&by_pane).iter().all(|k| k.starts_with("pane.")));
}

#[tokio::test(flavor = "multi_thread")]
async fn evt_003_replay_since() {
    let h = Harness::start_with(|c| c.events_replay = 5);
    let mut c = h.client().await;
    let seq0 = c.hello_ok.as_ref().unwrap().seq;
    for i in 0..4 {
        c.call(SessionCreate {
            name: format!("s{i}"),
            root: None,
            file: None,
        })
        .await
        .unwrap();
    }
    let mut late = h.client().await;
    let now = late.hello_ok.as_ref().unwrap().seq;
    late.call(EventsSubscribe {
        since: Some(now - 2),
        ..Default::default()
    })
    .await
    .unwrap();
    while late.recv(Duration::from_millis(300)).await {}
    // Panes may emit more events (e.g. cwd_changed) after `now`; the replay starts after `since`.
    let seqs: Vec<u64> = late.events.iter().map(Event::seq).collect();
    assert_eq!(seqs[..2], [now - 1, now], "{seqs:?}");
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    // Asking for more than is retained reports the gap first.
    let mut later = h.client().await;
    later
        .call(EventsSubscribe {
            since: Some(seq0),
            ..Default::default()
        })
        .await
        .unwrap();
    while later.recv(Duration::from_millis(300)).await {}
    // The gap reaches up to the oldest retained event; the replay continues from there.
    let Event::EventsLost(l) = &later.events[0] else {
        panic!("{:?}", later.events[0])
    };
    assert_eq!(l.from, seq0 + 1);
    assert!(l.to >= now - 5, "{l:?}");
    let seqs: Vec<u64> = later.events[1..].iter().map(Event::seq).collect();
    assert_eq!(seqs.len(), 5, "replay window is 5: {seqs:?}");
    assert_eq!(seqs[0], l.to + 1);
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn evt_004_pane_metadata() {
    let h = Harness::start();
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let p = c
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot
        .tabs[0]
        .focus
        .clone();
    c.call(EventsSubscribe::default()).await.unwrap();
    c.call(PaneSetMeta {
        pane_id: p.clone(),
        set: Some(
            [
                ("agent.state".into(), "working".into()),
                ("x".into(), "\u{1b}[31m".into()),
            ]
            .into(),
        ),
        unset: None,
    })
    .await
    .unwrap();
    c.call(PaneSetMeta {
        pane_id: p.clone(),
        set: None,
        unset: Some(vec!["x".into()]),
    })
    .await
    .unwrap();
    c.wait_for("two meta events", Duration::from_secs(5), |c| {
        c.events
            .iter()
            .filter(|e| e.kind() == "pane.meta_changed")
            .count()
            == 2
    })
    .await;
    let changes: Vec<_> = c
        .events
        .iter()
        .filter_map(|e| {
            if let Event::PaneMetaChanged(m) = e {
                Some(m.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(changes[0].set["agent.state"], "working");
    assert_eq!(changes[1].unset, ["x"]);
    let meta = c.call(PaneInfo { pane_id: p }).await.unwrap().meta;
    assert_eq!(
        meta,
        [("agent.state".to_string(), "working".to_string())].into()
    );
}
