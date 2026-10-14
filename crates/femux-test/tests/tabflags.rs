//! ADR-022 (daemon side): broadcast input, hidden floats, mark removal and raw pane output.

use std::time::Duration;

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::{Client, Harness};

/// A session with one tab of two tiled panes; returns (client, view, tab, a, b).
async fn two(h: &Harness) -> (Client, String, String, String, String) {
    let mut c = h.client().await;
    let (_, a) = c.new_session("s").await;
    let att = c.attach("s", 80, 24).await;
    let tab = att.snapshot.tabs[0].id.clone();
    let b = c
        .call(LayoutSplit {
            pane_id: a.clone(),
            dir: SplitDir::H,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.wait_text(&a, "$").await;
    c.wait_text(&b, "$").await;
    (c, att.view_id, tab, a, b)
}

fn key(ch: char) -> InputEvent {
    InputEvent::KeyEvent(KeyEvent {
        key: if ch == '\n' {
            "enter".into()
        } else {
            ch.to_string()
        },
        mods: vec![],
        text: (ch != '\n').then(|| ch.to_string()),
        event: KeyEventKind::Press,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn broadcast_types_into_every_pane_of_the_tab() {
    let h = Harness::start();
    let (mut c, _view, tab, a, b) = two(&h).await;
    c.call(EventsSubscribe {
        kinds: Some(vec!["tab.flags".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    // The event can arrive before the reply, while `call` still waits; count it too.
    let start = c.events.len();
    c.call(TabSet {
        tab_id: tab.clone(),
        broadcast: Some(true),
        floats_hidden: None,
    })
    .await
    .unwrap();
    let Event::TabFlags(f) = c.wait_event_from("tab.flags", start).await else {
        panic!()
    };
    assert!(f.broadcast && !f.floats_hidden);
    // Typed into A, shown in both.
    c.call(PaneInput {
        pane_id: a.clone(),
        events: "echo both-$((6*7))\n".chars().map(key).collect(),
    })
    .await
    .unwrap();
    c.wait_text(&a, "both-42").await;
    c.wait_text(&b, "both-42").await;
    // Off again: only A.
    c.call(TabSet {
        tab_id: tab.clone(),
        broadcast: Some(false),
        floats_hidden: None,
    })
    .await
    .unwrap();
    c.call(PaneInput {
        pane_id: a.clone(),
        events: "echo one-$((6*7))\n".chars().map(key).collect(),
    })
    .await
    .unwrap();
    c.wait_text(&a, "one-42").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    while c.recv(Duration::from_millis(50)).await {}
    assert!(!c.screen_text(&b).contains("one-42"));
    let snap = c.attach("s", 80, 24).await.snapshot;
    assert_eq!(snap.tabs[0].broadcast, Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn hidden_floats_leave_the_frame_and_the_focus() {
    let h = Harness::start();
    let (mut c, _view, tab, _a, b) = two(&h).await;
    c.call(LayoutFloat {
        pane_id: b.clone(),
        rect: Some(Rect {
            x: 5,
            y: 5,
            w: 30,
            h: 8,
        }),
    })
    .await
    .unwrap();
    c.call(LayoutFocus {
        view_id: None,
        pane_id: Some(b.clone()),
        dir: None,
    })
    .await
    .unwrap();
    c.wait_for("float shown", Duration::from_secs(10), |c| {
        c.frames
            .last()
            .is_some_and(|f| f.panes.iter().any(|p| p.pane_id == b))
    })
    .await;
    c.call(TabSet {
        tab_id: tab.clone(),
        broadcast: None,
        floats_hidden: Some(true),
    })
    .await
    .unwrap();
    // A full frame without the float; focus moved to the tiled pane.
    c.wait_for("float hidden", Duration::from_secs(10), |c| {
        c.frames
            .last()
            .is_some_and(|f| f.full && !f.panes.iter().any(|p| p.pane_id == b))
    })
    .await;
    let snap = c.attach("s", 80, 24).await.snapshot;
    assert_eq!(snap.tabs[0].floats_hidden, Some(true));
    assert_ne!(snap.tabs[0].focus, b);
    // Still running.
    let info = c.call(PaneInfo { pane_id: b.clone() }).await.unwrap();
    assert!(info.pid > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_can_be_cleared() {
    let h = Harness::start();
    let (mut c, _view, _tab, a, b) = two(&h).await;
    c.call(PaneMark {
        pane_id: a.clone(),
        mark: "q".into(),
        clear: None,
    })
    .await
    .unwrap();
    // Only the marked pane can clear it.
    assert!(
        c.call(PaneMark {
            pane_id: b.clone(),
            mark: "q".into(),
            clear: Some(true),
        })
        .await
        .is_err()
    );
    c.call(EventsSubscribe::default()).await.unwrap();
    let start = c.events.len();
    c.call(PaneMark {
        pane_id: a.clone(),
        mark: "q".into(),
        clear: Some(true),
    })
    .await
    .unwrap();
    let Event::PaneMarked(m) = c.wait_event_from("pane.marked", start).await else {
        panic!()
    };
    assert_eq!((m.mark.as_str(), m.cleared), ("q", Some(true)));
    let snap = c.attach("s", 80, 24).await.snapshot;
    assert!(!snap.marks.contains_key("q"));
}

#[tokio::test(flavor = "multi_thread")]
async fn pane_output_only_for_those_who_ask() {
    use base64::Engine;
    let h = Harness::start();
    let (mut c, _view, _tab, a, _b) = two(&h).await;
    let mut all = h.client().await;
    all.call(EventsSubscribe::default()).await.unwrap();
    let mut rec = h.client().await;
    rec.call(EventsSubscribe {
        kinds: Some(vec!["pane.output".into()]),
        pane: Some(a.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    c.type_text(&a, "echo raw-$((6*7))\n").await;
    rec.wait_for("raw output", Duration::from_secs(10), |c| {
        let bytes: Vec<u8> = c
            .events
            .iter()
            .filter_map(|e| match e {
                Event::PaneOutput(o) => base64::engine::general_purpose::STANDARD
                    .decode(&o.data)
                    .ok(),
                _ => None,
            })
            .flatten()
            .collect();
        String::from_utf8_lossy(&bytes).contains("raw-42")
    })
    .await;
    // An everything-subscriber never sees it, nor does replay.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while all.recv(Duration::from_millis(50)).await {}
    assert!(all.events.iter().all(|e| e.kind() != "pane.output"));
    let mut late = h.client().await;
    late.call(EventsSubscribe {
        kinds: Some(vec!["pane.output".into()]),
        since: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    while late.recv(Duration::from_millis(50)).await {}
    assert!(late.events.iter().all(|e| e.kind() != "pane.output"));
}
