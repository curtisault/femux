//! REQ-SESS-*: hierarchy, layout tree, one event per mutation, marks, zoom.

use std::time::Duration;

use femux_proto::Event;
use femux_proto::msg::*;
use femux_test::{Client, Harness};

async fn setup(h: &Harness) -> (Client, String, String, String) {
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: Some(h.dir.path().to_string_lossy().into()),
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach("s", 81, 25).await;
    let tab = &att.snapshot.tabs[0];
    c.wait_text(&tab.focus, "$").await;
    (c, att.view_id.clone(), tab.id.clone(), tab.focus.clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn sess_001_hierarchy_and_unique_names() {
    let h = Harness::start();
    let (mut c, _, tab, _) = setup(&h).await;
    let dup = c
        .call(SessionCreate {
            name: "s".into(),
            root: None,
            file: None,
        })
        .await
        .unwrap_err();
    assert_eq!(dup.code, femux_proto::ErrorCode::Conflict);
    let t2 = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: Some("two".into()),
            index: None,
        })
        .await
        .unwrap();
    c.call(SessionCreate {
        name: "other".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let list = c.call(SessionList {}).await.unwrap().sessions;
    assert_eq!(
        list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["other", "s"]
    );
    let s = &list[1];
    assert_eq!((s.tabs, s.clients), (2, 1));
    assert_eq!(s.root, h.dir.path().to_string_lossy());
    assert!(s.created > 1_700_000_000_000);
    // server -> sessions -> tabs -> layout tree -> panes
    let snap = c
        .call(SessionAttach {
            name: "s".into(),
            tab: Some(t2.tab_id.clone()),
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(
        snap.tabs.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
        [tab, t2.tab_id.clone()]
    );
    assert_eq!(snap.current_tab, t2.tab_id);
    assert_eq!(
        snap.tabs[1].tree,
        LayoutNode::PaneNode(PaneNode { id: t2.pane_id })
    );
    let r = c
        .call(SessionRename {
            name: "other".into(),
            new: "s".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(r.code, femux_proto::ErrorCode::Conflict);
}

#[tokio::test(flavor = "multi_thread")]
async fn sess_002_layout_tree_nodes() {
    let h = Harness::start();
    let (mut c, _, tab, a) = setup(&h).await;
    let ratio = Ratio { num: 2, den: 3 };
    let b = c
        .call(LayoutSplit {
            pane_id: a.clone(),
            dir: SplitDir::H,
            ratio: Some(ratio.clone()),
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    let cc = c
        .call(LayoutSplit {
            pane_id: b.clone(),
            dir: SplitDir::V,
            ratio: None,
            cwd: None,
            cmd: None,
            preselect: None,
        })
        .await
        .unwrap()
        .pane_id;
    c.call(LayoutFloat {
        pane_id: cc.clone(),
        rect: Some(Rect {
            x: 5,
            y: 5,
            w: 20,
            h: 8,
        }),
    })
    .await
    .unwrap();
    let got = c
        .call(LayoutGet {
            tab_id: tab.clone(),
        })
        .await
        .unwrap();
    let pane = |id: &str| LayoutNode::PaneNode(PaneNode { id: id.into() });
    assert_eq!(
        got.tree,
        LayoutNode::SplitNode(SplitNode {
            dir: SplitDir::H,
            ratio,
            children: vec![pane(&a), pane(&b)]
        })
    );
    assert_eq!(
        got.floats,
        [FloatNode {
            id: cc.clone(),
            rect: Rect {
                x: 5,
                y: 5,
                w: 20,
                h: 8
            }
        }]
    );
    // layout.apply accepts a permutation of the same panes and rejects anything else.
    let swapped = LayoutNode::SplitNode(SplitNode {
        dir: SplitDir::V,
        ratio: Ratio { num: 1, den: 4 },
        children: vec![pane(&b), pane(&a)],
    });
    let floats = Some(got.floats.clone());
    c.call(LayoutApply {
        tab_id: tab.clone(),
        tree: swapped.clone(),
        floats: floats.clone(),
    })
    .await
    .unwrap();
    assert_eq!(
        c.call(LayoutGet {
            tab_id: tab.clone()
        })
        .await
        .unwrap()
        .tree,
        swapped
    );
    let bad = c
        .call(LayoutApply {
            tab_id: tab.clone(),
            tree: pane(&a),
            floats,
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code, femux_proto::ErrorCode::Invalid);
    let bad_ratio = LayoutNode::SplitNode(SplitNode {
        dir: SplitDir::V,
        ratio: Ratio { num: 5, den: 4 },
        children: vec![pane(&b), pane(&a)],
    });
    assert!(
        c.call(LayoutApply {
            tab_id: tab,
            tree: bad_ratio,
            floats: None
        })
        .await
        .is_err()
    );
}

/// Each mutation, and the one event kind it must produce exactly once.
#[tokio::test(flavor = "multi_thread")]
async fn sess_003_one_event_per_mutation() {
    let h = Harness::start();
    let (mut c, view, tab, a) = setup(&h).await;
    c.call(EventsSubscribe::default()).await.unwrap();

    async fn check(c: &mut Client, kind: &str, req: impl Into<femux_proto::Request>) {
        let start = c.events.len();
        let id = c.send(req).await;
        c.response(id)
            .await
            .unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        // Events are emitted before the response; give the pump a moment to deliver them.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
        while c
            .recv(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await
        {}
        let n = c.events[start..]
            .iter()
            .filter(|e| e.kind() == kind)
            .count();
        assert_eq!(
            n,
            1,
            "{kind}: {:?}",
            c.events[start..]
                .iter()
                .map(Event::kind)
                .collect::<Vec<_>>()
        );
    }

    let b = {
        let id = c
            .send(LayoutSplit {
                pane_id: a.clone(),
                dir: SplitDir::H,
                ratio: None,
                cwd: None,
                cmd: None,
                preselect: None,
            })
            .await;
        match c.response(id).await.unwrap() {
            femux_proto::Response::LayoutSplit(ok) => ok.pane_id,
            r => panic!("{r:?}"),
        }
    };
    // Drain the split's own events before checking one mutation at a time.
    while c.recv(Duration::from_millis(300)).await {}
    check(
        &mut c,
        "layout.focused",
        LayoutFocus {
            view_id: Some(view.clone()),
            pane_id: Some(a.clone()),
            dir: None,
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutResize {
            pane_id: a.clone(),
            dir: Direction::L,
            delta_cells: 3,
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutSwap {
            a: a.clone(),
            b: b.clone(),
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutEqualize {
            tab_id: tab.clone(),
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutRotate { pane_id: a.clone() },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutZoom {
            tab_id: tab.clone(),
            on: Some(true),
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutZoom {
            tab_id: tab.clone(),
            on: Some(false),
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        LayoutFloat {
            pane_id: b.clone(),
            rect: None,
        },
    )
    .await;
    check(
        &mut c,
        "layout.changed",
        PaneResize {
            pane_id: b.clone(),
            cols: 30,
            rows: 10,
        },
    )
    .await;
    check(&mut c, "layout.changed", LayoutTile { pane_id: b.clone() }).await;
    check(
        &mut c,
        "layout.changed",
        LayoutSetScheme {
            tab_id: tab.clone(),
            scheme: Scheme::Master,
        },
    )
    .await;
    check(
        &mut c,
        "pane.meta_changed",
        PaneSetMeta {
            pane_id: a.clone(),
            set: Some([("k".into(), "v".into())].into()),
            unset: None,
        },
    )
    .await;
    check(
        &mut c,
        "pane.marked",
        PaneMark {
            pane_id: a.clone(),
            mark: "q".into(),
        },
    )
    .await;
    check(
        &mut c,
        "pane.spawned",
        PaneRespawn {
            pane_id: b.clone(),
            cmd: None,
        },
    )
    .await;
    check(&mut c, "layout.changed", LayoutClose { pane_id: b.clone() }).await;
    check(
        &mut c,
        "tab.created",
        TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: None,
            index: None,
        },
    )
    .await;
    let t2 = c
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: None,
            index: None,
        })
        .await
        .unwrap()
        .tab_id;
    check(
        &mut c,
        "tab.renamed",
        TabRename {
            tab_id: t2.clone(),
            title: "x".into(),
        },
    )
    .await;
    check(
        &mut c,
        "tab.moved",
        TabMove {
            tab_id: t2.clone(),
            index: 0,
        },
    )
    .await;
    check(
        &mut c,
        "tab.selected",
        TabSelect {
            view_id: Some(view.clone()),
            tab_id: t2.clone(),
        },
    )
    .await;
    check(
        &mut c,
        "view.resized",
        ViewResize {
            view_id: view.clone(),
            cols: 70,
            rows: 20,
        },
    )
    .await;
    check(&mut c, "tab.closed", TabClose { tab_id: t2 }).await;
    check(
        &mut c,
        "session.created",
        SessionCreate {
            name: "t".into(),
            root: None,
            file: None,
        },
    )
    .await;
    check(
        &mut c,
        "session.renamed",
        SessionRename {
            name: "t".into(),
            new: "u".into(),
        },
    )
    .await;
    check(&mut c, "view.detached", SessionDetach { view_id: view }).await;
    check(&mut c, "session.closed", SessionKill { name: "u".into() }).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sess_004_marks() {
    let h = Harness::start();
    let (mut c, _, _, a) = setup(&h).await;
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
    for (i, ch) in ('a'..='z').enumerate() {
        let p = if i % 2 == 0 { &a } else { &b };
        c.call(PaneMark {
            pane_id: p.clone(),
            mark: ch.to_string(),
        })
        .await
        .unwrap();
    }
    for bad in ["A", "1", "ab", ""] {
        assert!(
            c.call(PaneMark {
                pane_id: a.clone(),
                mark: bad.into()
            })
            .await
            .is_err(),
            "{bad:?}"
        );
    }
    // Re-marking moves the letter.
    c.call(PaneMark {
        pane_id: b.clone(),
        mark: "a".into(),
    })
    .await
    .unwrap();
    let snap = c
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snap.marks.len(), 26);
    assert_eq!(snap.marks["a"], b);
    assert_eq!(snap.marks["c"], a);
    // Closing a pane drops its marks.
    c.call(LayoutClose { pane_id: b }).await.unwrap();
    let snap = c
        .call(SessionAttach {
            name: "s".into(),
            tab: None,
            cols: None,
            rows: None,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(snap.marks.len(), 12);
}

#[tokio::test(flavor = "multi_thread")]
async fn sess_005_zoom_shows_only_focused_pane() {
    let h = Harness::start();
    let (mut c, _, tab, a) = setup(&h).await;
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
    c.wait_text(&b, "$").await;
    assert!(
        c.call(LayoutZoom {
            tab_id: tab.clone(),
            on: None
        })
        .await
        .unwrap()
        .zoomed
    );
    let bb = b.clone();
    c.wait_for("zoomed frame", Duration::from_secs(5), |c| {
        c.frames.last().is_some_and(|f| {
            f.full
                && f.panes.len() == 1
                && f.panes[0].pane_id == bb
                && f.panes[0].rect
                    == Rect {
                        x: 0,
                        y: 0,
                        w: 81,
                        h: 25,
                    }
        })
    })
    .await;
    assert_eq!(c.call(PaneInfo { pane_id: b.clone() }).await.unwrap().id, b);
    assert!(
        !c.call(LayoutZoom {
            tab_id: tab,
            on: None
        })
        .await
        .unwrap()
        .zoomed
    );
    c.wait_for("unzoomed frame", Duration::from_secs(5), |c| {
        c.frames.last().is_some_and(|f| f.panes.len() == 2)
    })
    .await;
}
