//! REQ-UI-011: floating panes render above tiles; mouse drags move and resize them.

mod common;

use common::Drive;
use femux_client::app::MouseKind;
use femux_client::config::ClientConfig;
use femux_proto::Request;
use femux_proto::msg as m;

#[tokio::test(flavor = "multi_thread")]
async fn ui_011_floats_render_above_tiles_and_follow_the_mouse() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.press_all(&["ctrl+space", "|"]).await;
    d.until("two panes", |a| a.model.panes.len() == 2).await;
    let new = d.app.model.focused_id().unwrap();
    d.press_all(&["ctrl+space", "f"]).await;
    d.until("floating", |a| {
        a.model.current_tab().unwrap().is_float(&new)
            && a.model.panes[&new]
                .screen
                .as_ref()
                .is_some_and(|s| s.rect.x > 0)
    })
    .await;
    let rect = d.app.model.panes[&new]
        .screen
        .as_ref()
        .unwrap()
        .rect
        .clone();
    // The float is drawn with a box over the tiled pane behind it.
    let screen = d.screen();
    let top = screen.lines().nth(rect.y as usize).unwrap(); // tab strip is row 0
    assert!(top.contains('┌') && top.contains('┐'), "{screen}");
    // Drag the title bar: the float moves (layout.float with a new rect).
    let (gx, gy) = (rect.x + 2, rect.y); // screen row of the top border = rect.y - 1 + 1
    d.sent.clear();
    d.app
        .on_mouse(gx, gy, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(gx + 5, gy + 2, MouseKind::Move, vec![]);
    d.app.on_mouse(gx + 5, gy + 2, MouseKind::Release, vec![]);
    d.settle().await;
    let moved = d
        .sent
        .iter()
        .find_map(|r| match r {
            Request::LayoutFloat(f) => f.rect.clone(),
            _ => None,
        })
        .expect("drag sends layout.float");
    assert_eq!((moved.x, moved.y), (rect.x + 5, rect.y + 2));
    d.until("daemon moved it", |a| {
        a.model.current_tab().unwrap().floats[0].rect.x == rect.x + 5
    })
    .await;
    let got = d.layout().await;
    assert_eq!(got.floats[0].rect.w, rect.w);
    // Drag the bottom-right corner: the float resizes.
    let rect = got.floats[0].rect.clone();
    let (cx, cy) = (rect.x + rect.w, rect.y + rect.h + 1);
    d.sent.clear();
    d.app
        .on_mouse(cx, cy, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(cx + 3, cy + 1, MouseKind::Move, vec![]);
    d.app.on_mouse(cx + 3, cy + 1, MouseKind::Release, vec![]);
    d.settle().await;
    d.until("daemon resized it", |a| {
        let f = &a.model.current_tab().unwrap().floats[0].rect;
        (f.w, f.h) == (rect.w + 3, rect.h + 1)
    })
    .await;
    // f again tiles it.
    d.press_all(&["ctrl+space", "f"]).await;
    d.until("tiled", |a| {
        a.model.current_tab().unwrap().floats.is_empty()
    })
    .await;
}
