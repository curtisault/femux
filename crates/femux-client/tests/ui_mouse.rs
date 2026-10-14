//! REQ-UI-053: mouse on by default, fully disableable.

mod common;

use common::Drive;
use femux_client::app::MouseKind;
use femux_client::config::ClientConfig;
use femux_proto::Request;
use femux_proto::msg as m;

#[tokio::test(flavor = "multi_thread")]
async fn ui_053_mouse_focuses_forwards_and_can_be_disabled() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.press_all(&["ctrl+space", "|"]).await;
    d.until("two panes", |a| a.model.panes.len() == 2).await;
    let right = d.app.model.focused_id().unwrap();
    assert_ne!(right, pane);
    // Click in the left pane: focus moves there and the press reaches the pane.
    d.sent.clear();
    d.app
        .on_mouse(3, 4, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(3, 4, MouseKind::Release, vec![]);
    d.settle().await;
    let kinds: Vec<&str> = d.sent.iter().map(|r| r.kind()).collect();
    assert!(kinds.contains(&"layout.focus"), "{kinds:?}");
    let press = d.sent.iter().find_map(|r| match r {
        Request::PaneInput(i) if i.pane_id == pane => i.events.first().cloned(),
        _ => None,
    });
    assert!(
        matches!(&press, Some(m::InputEvent::MouseEvent(e)) if e.x == 3 && e.y == 3 && e.event == m::MouseEventKind::Press),
        "{press:?}"
    );
    d.until("focus moved", |a| {
        a.model.focused_id().as_deref() == Some(pane.as_str())
    })
    .await;
    // Wheel events go to the pane under the pointer.
    d.sent.clear();
    d.app.on_mouse(60, 4, MouseKind::WheelUp, vec![]);
    d.settle().await;
    assert!(
        d.sent
            .iter()
            .any(|r| matches!(r, Request::PaneInput(i) if i.pane_id == right))
    );
    // Clicking the tab strip selects a tab.
    d.press_all(&["ctrl+space", "t"]).await;
    d.until("two tabs", |a| a.model.tabs.len() == 2).await;
    let _ = d.screen();
    let (x0, _, first) = d.app.tab_hits[0].clone();
    d.sent.clear();
    d.app
        .on_mouse(x0 + 1, 0, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.settle().await;
    assert!(
        d.sent
            .iter()
            .any(|r| matches!(r, Request::TabSelect(t) if t.tab_id == first))
    );

    // mouse = false: nothing is sent for mouse input.
    let off = ClientConfig {
        mouse: false,
        ..Default::default()
    };
    let mut d = Drive::new(off).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.sent.clear();
    d.app
        .on_mouse(3, 4, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(3, 4, MouseKind::WheelDown, vec![]);
    d.settle().await;
    assert!(
        d.sent.iter().all(|r| r.kind() == "frame.ack"),
        "{:?}",
        d.sent
    );
}
