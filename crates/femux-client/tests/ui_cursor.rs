//! REQ-VT-002 (0.9.0): the host cursor takes the focused pane's shape and blink.

mod common;

use common::Drive;
use femux_client::config::ClientConfig;
use femux_client::render::render;
use femux_client::theme::Theme;
use femux_proto::msg as m;
use termwiz::surface::{CursorShape, Surface};

fn host_shape(d: &mut Drive) -> Option<CursorShape> {
    let mut s = Surface::new(d.app.cols as usize, d.app.rows as usize);
    render(&mut d.app, &Theme::dark(), &mut s);
    s.cursor_shape()
}

#[tokio::test(flavor = "multi_thread")]
async fn vt_002_the_host_cursor_follows_the_pane() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    assert_eq!(host_shape(&mut d), Some(CursorShape::SteadyBlock));
    // neovim's insert mode: DECSCUSR 5, a blinking bar.
    d.type_text("printf '\\033[5 q'\n").await;
    d.until("bar", |a| {
        a.model.panes[&pane]
            .screen
            .as_ref()
            .is_some_and(|s| s.cursor.shape == m::CursorShape::Bar)
    })
    .await;
    assert_eq!(host_shape(&mut d), Some(CursorShape::BlinkingBar));
    // A prompt has the host's own cursor; closing it gives the pane's back.
    d.press_all(&["ctrl+space", "T"]).await;
    assert_eq!(host_shape(&mut d), Some(CursorShape::Default));
    d.press("escape").await;
    assert_eq!(host_shape(&mut d), Some(CursorShape::BlinkingBar));
    // So does copy mode.
    d.press_all(&["ctrl+space", "["]).await;
    assert_eq!(host_shape(&mut d), Some(CursorShape::Default));
}
