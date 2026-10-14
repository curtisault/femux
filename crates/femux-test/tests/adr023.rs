//! ADR-023 (daemon side): the frame cursor's blink and the pane's mouse mode.

use std::time::{Duration, Instant};

use femux_proto::msg as m;
use femux_test::{Client, Harness};

/// The newest frame entry for `pane`.
fn last(c: &Client, pane: &str) -> Option<m::PaneFrame> {
    c.frames
        .iter()
        .rev()
        .flat_map(|f| f.panes.iter())
        .find(|p| p.pane_id == pane)
        .cloned()
}

async fn until(c: &mut Client, pane: &str, what: &str, ok: impl Fn(&m::PaneFrame) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !last(c, pane).is_some_and(|p| ok(&p)) {
        assert!(Instant::now() < deadline, "{what}: {:?}", last(c, pane));
        c.recv(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_carry_cursor_blink_and_mouse_mode() {
    let h = Harness::start();
    let mut c = h.client().await;
    let (_, pane) = c.new_session("s").await;
    c.attach("s", 80, 24).await;
    c.wait_text(&pane, "$").await;
    // DECSCUSR 5: blinking bar; 2: steady block.
    c.type_text(&pane, "printf '\\033[5 q'\n").await;
    until(&mut c, &pane, "blinking bar", |p| {
        p.cursor.shape == m::CursorShape::Bar && p.cursor.blink == Some(true)
    })
    .await;
    c.type_text(&pane, "printf '\\033[2 q'\n").await;
    until(&mut c, &pane, "steady block", |p| {
        p.cursor.shape == m::CursorShape::Block && p.cursor.blink == Some(false)
    })
    .await;
    // Mouse tracking on and off. Turning it off changes no cell after the prompt settles,
    // yet the frame still says so.
    c.type_text(&pane, "printf '\\033[?1002h'\n").await;
    until(&mut c, &pane, "mouse on", |p| p.mouse == Some(true)).await;
    c.type_text(&pane, "printf '\\033[?1002l'\n").await;
    until(&mut c, &pane, "mouse off", |p| p.mouse == Some(false)).await;
}
