//! REQ-UI-010 (daemon side): layout schemes selectable per tab change the geometry.

use femux_proto::msg::*;
use femux_test::{Client, Harness};

async fn three(h: &Harness) -> (Client, String, String, Vec<String>) {
    let mut c = h.client().await;
    c.call(SessionCreate {
        name: "s".into(),
        root: None,
        file: None,
    })
    .await
    .unwrap();
    let att = c.attach("s", 81, 25).await;
    let tab = att.snapshot.tabs[0].id.clone();
    let a = att.snapshot.tabs[0].focus.clone();
    let mut panes = vec![a.clone()];
    for p in [a.clone(), a.clone()] {
        let n = c
            .call(LayoutSplit {
                pane_id: p,
                dir: SplitDir::V,
                ratio: None,
                cwd: None,
                cmd: None,
                preselect: None,
            })
            .await
            .unwrap()
            .pane_id;
        panes.push(n);
    }
    (c, att.view_id, tab, panes)
}

fn rect_of(c: &Client, pane: &str) -> Option<(u16, u16, u16, u16)> {
    c.panes
        .get(pane)
        .map(|s| (s.rect.x, s.rect.y, s.rect.w, s.rect.h))
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_010_schemes_change_the_geometry() {
    let h = Harness::start();
    let (mut c, view, tab, panes) = three(&h).await;
    // Pane order is a, c, b after splitting a twice (each new pane goes right after a).
    let order = {
        let got = c
            .call(LayoutGet {
                tab_id: tab.clone(),
            })
            .await
            .unwrap();
        let mut out = Vec::new();
        fn walk(n: &LayoutNode, out: &mut Vec<String>) {
            match n {
                LayoutNode::PaneNode(p) => out.push(p.id.clone()),
                LayoutNode::SplitNode(s) => s.children.iter().for_each(|c| walk(c, out)),
            }
        }
        walk(&got.tree, &mut out);
        out
    };
    assert_eq!(order.len(), 3);
    c.call(LayoutSetScheme {
        tab_id: tab.clone(),
        scheme: Scheme::Master,
    })
    .await
    .unwrap();
    let master = order[0].clone();
    c.wait_for("master geometry", std::time::Duration::from_secs(5), |c| {
        rect_of(c, &master) == Some((0, 0, 40, 25))
    })
    .await;
    assert_eq!(rect_of(&c, &order[1]), Some((41, 0, 40, 12)));
    assert_eq!(rect_of(&c, &order[2]), Some((41, 13, 40, 12)));

    // scroll: two panes side by side; the strip follows focus.
    c.call(LayoutSetScheme {
        tab_id: tab.clone(),
        scheme: Scheme::Scroll,
    })
    .await
    .unwrap();
    c.call(LayoutFocus {
        view_id: Some(view.clone()),
        pane_id: Some(order[0].clone()),
        dir: None,
    })
    .await
    .unwrap();
    let second = order[1].clone();
    c.wait_for("scroll geometry", std::time::Duration::from_secs(5), |c| {
        rect_of(c, &second) == Some((41, 0, 40, 25))
    })
    .await;
    assert_eq!(rect_of(&c, &order[0]), Some((0, 0, 40, 25)));
    assert!(
        !c.panes.contains_key(&order[2]),
        "third pane is off the strip"
    );
    // l twice reaches the hidden pane and scrolls it in.
    for _ in 0..2 {
        c.call(LayoutFocus {
            view_id: Some(view.clone()),
            pane_id: None,
            dir: Some(Direction::L),
        })
        .await
        .unwrap();
    }
    let third = order[2].clone();
    c.wait_for(
        "third pane visible",
        std::time::Duration::from_secs(5),
        |c| rect_of(c, &third) == Some((41, 0, 40, 25)),
    )
    .await;
    assert!(!c.panes.contains_key(&order[0]));
    // Back to bsp restores the tree geometry.
    c.call(LayoutSetScheme {
        tab_id: tab,
        scheme: Scheme::Bsp,
    })
    .await
    .unwrap();
    let a = panes[0].clone();
    // a was split twice: it keeps half of the top half.
    c.wait_for("bsp geometry", std::time::Duration::from_secs(5), |c| {
        rect_of(c, &a) == Some((0, 0, 81, 6))
    })
    .await;
}

/// `[layout] scheme` and `gaps` (SPEC §10): new tabs start in the configured scheme, every
/// separator is `1 + 2 * gaps` wide, and `server.reload` re-applies gaps to open tabs.
#[tokio::test(flavor = "multi_thread")]
async fn layout_config_sets_the_scheme_and_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("config.toml");
    std::fs::write(&conf, "[layout]\nscheme = \"master\"\ngaps = 1\n").unwrap();
    let h = Harness::start_with(|c| {
        c.config_path = Some(conf.clone());
        c.scheme = Scheme::Master;
        c.gaps = 1;
    });
    let (mut c, _view, _tab, panes) = three(&h).await;
    let (a, rest) = (panes[0].clone(), panes[1..].to_vec());
    // master: a on the left half, the others stacked on the right, 3-cell separators.
    c.wait_for(
        "gapped master layout",
        std::time::Duration::from_secs(10),
        |c| {
            rect_of(c, &a) == Some((0, 0, 39, 25))
                && rest
                    .iter()
                    .all(|p| rect_of(c, p).is_some_and(|r| r.0 == 42))
        },
    )
    .await;
    let mut ys: Vec<(u16, u16)> = rest
        .iter()
        .map(|p| rect_of(&c, p).map(|r| (r.1, r.3)).unwrap())
        .collect();
    ys.sort();
    assert_eq!(ys, [(0, 11), (14, 11)]);
    // Reload without gaps: the open tab closes them up.
    std::fs::write(&conf, "[layout]\nscheme = \"master\"\n").unwrap();
    c.call(ServerReload {}).await.unwrap();
    c.wait_for("gaps removed", std::time::Duration::from_secs(10), |c| {
        rect_of(c, &a) == Some((0, 0, 40, 25))
    })
    .await;
    assert_eq!(h.daemon().settings.lock().unwrap().gaps, 0);
}
