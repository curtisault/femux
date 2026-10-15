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
    // Click in the left pane: focus moves there and the press reaches the pane. (A left
    // press on a pane that does not track the mouse starts a selection instead, 0.9.0, and a
    // right press opens the context menu, 0.11.0.)
    d.sent.clear();
    d.app
        .on_mouse(3, 4, MouseKind::Press(m::MouseButton::Middle), vec![]);
    d.app.on_mouse(3, 4, MouseKind::Release, vec![]);
    d.settle().await;
    let kinds: Vec<&str> = d.sent.iter().map(|r| r.kind()).collect();
    assert!(kinds.contains(&"layout.focus"), "{kinds:?}");
    let press = d.sent.iter().find_map(|r| match r {
        Request::PaneInput(i) if i.pane_id == pane => i.events.first().cloned(),
        _ => None,
    });
    assert!(
        matches!(&press, Some(m::InputEvent::MouseEvent(e)) if e.x == 3 && e.y == 4 && e.event == m::MouseEventKind::Press),
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
    // The tabs are in the bar, the bottom row (0.10.0).
    let (x0, _, first) = d.app.tab_hits[0].clone();
    let bar = d.app.rows - 1;
    d.sent.clear();
    d.app
        .on_mouse(x0 + 1, bar, MouseKind::Press(m::MouseButton::Left), vec![]);
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

/// The row of the pane's screen showing `needle`, and its column.
fn find(d: &Drive, pane: &str, needle: &str) -> (u16, u16) {
    let text = d.pane_text(pane);
    text.lines()
        .enumerate()
        .find_map(|(y, l)| l.find(needle).map(|x| (x as u16, y as u16)))
        .unwrap_or_else(|| panic!("{needle} not in\n{text}"))
}

/// REQ-UI-053 (0.9.0): a left drag selects and its release copies; a click selects nothing;
/// a program that tracks the mouse gets the drag unless shift is held.
#[tokio::test(flavor = "multi_thread")]
async fn ui_053_drag_selects_and_release_copies() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.type_text("echo sel-$((6*7))-done\n").await;
    d.wait_pane_text(&pane, "sel-42-done").await;
    let area = d.app.layout().panes;
    let (px, py) = find(&d, &pane, "sel-42-done");
    let (x, y) = (area.x + px, area.y + py);
    // A click alone: nothing selected, nothing copied, nothing sent to the shell.
    d.sent.clear();
    d.app
        .on_mouse(x, y, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(x, y, MouseKind::Release, vec![]);
    d.settle().await;
    assert!(d.app.selection.is_none());
    assert!(!d.sent.iter().any(|r| matches!(r, Request::PaneInput(_))));
    // Drag over `sel-42`, then a little past the end of the line: copied, trailing blanks
    // trimmed, and still highlighted.
    d.effects.clear();
    d.app
        .on_mouse(x, y, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(x + 3, y, MouseKind::Move, vec![]);
    d.app.on_mouse(x + 20, y, MouseKind::Release, vec![]);
    d.settle().await;
    assert!(
        d.effects
            .contains(&femux_client::app::Effect::Clipboard("sel-42-done".into())),
        "{:?}",
        d.effects
    );
    assert!(
        d.sent
            .iter()
            .any(|r| matches!(r, Request::ClipboardSet(c) if c.text == "sel-42-done"))
    );
    assert!(d.app.selection.as_ref().is_some_and(|s| s.done));
    let _ = d.screen();
    // The next key clears it.
    d.press("a").await;
    assert!(d.app.selection.is_none());
    d.press("backspace").await;
    // A program that tracks the mouse gets the press; shift still selects.
    d.type_text("printf '\\033[?1002h'\n").await;
    d.until("mouse mode", |a| {
        a.model.panes[&pane]
            .screen
            .as_ref()
            .is_some_and(|s| s.mouse)
    })
    .await;
    d.sent.clear();
    d.app
        .on_mouse(x, y, MouseKind::Press(m::MouseButton::Left), vec![]);
    d.app.on_mouse(x, y, MouseKind::Release, vec![]);
    d.settle().await;
    assert!(d.app.selection.is_none());
    assert!(
        d.sent
            .iter()
            .any(|r| matches!(r, Request::PaneInput(i) if i.pane_id == pane))
    );
    d.app.on_mouse(
        x,
        y,
        MouseKind::Press(m::MouseButton::Left),
        vec![m::Mod::Shift],
    );
    d.app
        .on_mouse(x + 2, y, MouseKind::Release, vec![m::Mod::Shift]);
    assert!(d.app.selection.as_ref().is_some_and(|s| s.done));
    d.type_text("printf '\\033[?1002l'\n").await;
}
