//! REQ-UI-010: BSP preselection and the per-tab layout schemes.

mod common;

use common::Drive;
use femux_client::config::ClientConfig;
use femux_proto::Request;
use femux_proto::msg as m;

#[tokio::test(flavor = "multi_thread")]
async fn ui_010_preselection_places_the_next_split() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // prefix ctrl+h: the next split goes left, whatever s/v would do.
    d.press_all(&["ctrl+space", "ctrl+h"]).await;
    assert_eq!(d.app.preselect, Some(m::Direction::H));
    assert!(d.screen().contains("next split"), "{}", d.screen());
    d.press_all(&["ctrl+space", "s"]).await;
    let split = d
        .sent
        .iter()
        .find_map(|r| match r {
            Request::LayoutSplit(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(split.preselect, Some(m::Direction::H));
    assert_eq!(d.app.preselect, None, "preselection is consumed");
    let got = d.layout().await;
    let m::LayoutNode::SplitNode(s) = got.tree else {
        panic!("no split")
    };
    assert_eq!(s.dir, m::SplitDir::H);
    assert_eq!(
        s.children[1],
        m::LayoutNode::PaneNode(m::PaneNode { id: pane.clone() }),
        "the old pane is on the right, the new one left"
    );
    // Schemes are selectable per tab through the palette.
    d.press_all(&["ctrl+space", ":"]).await;
    d.type_text("scheme master").await;
    d.press("enter").await;
    d.until("master scheme", |a| {
        a.model.current_tab().unwrap().scheme == m::Scheme::Master
    })
    .await;
    d.press_all(&["ctrl+space", ":"]).await;
    d.type_text("scheme scroll").await;
    d.press("enter").await;
    d.until("scroll scheme", |a| {
        a.model.current_tab().unwrap().scheme == m::Scheme::Scroll
    })
    .await;
}
