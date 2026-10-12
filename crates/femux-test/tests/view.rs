//! REQ-VIEW-*: several clients on one session, sizing by the smallest viewer of a tab.

use std::time::Duration;

use femux_proto::msg::*;
use femux_test::{Client, Harness};

async fn rect_of(c: &mut Client, pane: &str, want: Rect) {
    let p = pane.to_owned();
    c.wait_for(
        &format!("{pane} at {want:?}"),
        Duration::from_secs(5),
        |c| c.panes.get(&p).is_some_and(|s| s.rect == want),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_001_each_client_has_its_own_tab_and_size() {
    let h = Harness::start();
    let mut a = h.client().await;
    a.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let t2 = a
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: None,
            index: None,
            restore: None,
        })
        .await
        .unwrap();
    let va = a.attach("s", 100, 30).await;
    let mut b = h.client().await;
    let vb = b.attach("s", 70, 20).await;
    b.call(TabSelect {
        view_id: Some(vb.view_id.clone()),
        tab_id: t2.tab_id.clone(),
    })
    .await
    .unwrap();
    b.wait_for("tab 2 on b", Duration::from_secs(5), |c| {
        c.tab.as_ref().is_some_and(|t| t.index == 1)
    })
    .await;
    // a still shows tab 1 at its own size.
    let p1 = va.snapshot.tabs[0].focus.clone();
    rect_of(
        &mut a,
        &p1,
        Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        },
    )
    .await;
    assert_eq!(a.tab.as_ref().unwrap().index, 0);
    rect_of(
        &mut b,
        &t2.pane_id,
        Rect {
            x: 0,
            y: 0,
            w: 70,
            h: 20,
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_002_tab_size_is_minimum_of_viewers() {
    let h = Harness::start();
    let mut a = h.client().await;
    a.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let va = a.attach("s", 100, 30).await;
    let pane = va.snapshot.tabs[0].focus.clone();
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        },
    )
    .await;
    let mut b = h.client().await;
    let vb = b.attach("s", 80, 40).await;
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 30,
        },
    )
    .await;
    // The smaller viewer leaving restores the larger size.
    b.call(SessionDetach {
        view_id: vb.view_id,
    })
    .await
    .unwrap();
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        },
    )
    .await;
    a.type_text(&pane, "stty size\n").await;
    a.wait_text(&pane, "30 100").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_003_other_tabs_do_not_constrain() {
    let h = Harness::start();
    let mut a = h.client().await;
    a.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let t2 = a
        .call(TabCreate {
            session: "s".into(),
            cwd: None,
            cmd: None,
            title: None,
            index: None,
            restore: None,
        })
        .await
        .unwrap();
    let va = a.attach("s", 100, 30).await;
    let pane = va.snapshot.tabs[0].focus.clone();
    let mut b = h.client().await;
    let vb = b
        .call(SessionAttach {
            name: "s".into(),
            tab: Some(t2.tab_id.clone()),
            cols: Some(60),
            rows: Some(20),
        })
        .await
        .unwrap();
    b.call(FrameSubscribe {
        view_id: vb.view_id,
    })
    .await
    .unwrap();
    rect_of(
        &mut b,
        &t2.pane_id,
        Rect {
            x: 0,
            y: 0,
            w: 60,
            h: 20,
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn view_004_resizes_are_debounced() {
    let h = Harness::start();
    let mut a = h.client().await;
    a.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let va = a.attach("s", 100, 30).await;
    let pane = va.snapshot.tabs[0].focus.clone();
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 30,
        },
    )
    .await;
    let before = h.daemon().pane_resizes(&pane).unwrap();
    for i in 0..20u16 {
        a.call(ViewResize {
            view_id: va.view_id.clone(),
            cols: 60 + i,
            rows: 20,
        })
        .await
        .unwrap();
    }
    rect_of(
        &mut a,
        &pane,
        Rect {
            x: 0,
            y: 0,
            w: 79,
            h: 20,
        },
    )
    .await;
    let after = h.daemon().pane_resizes(&pane).unwrap();
    assert!(
        after - before <= 2,
        "{} resizes for 20 view changes",
        after - before
    );
}
