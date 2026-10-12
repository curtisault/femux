//! REQ-UI-001, REQ-UI-002, REQ-UI-004: modes, the prefix table, prefix-less bindings.

mod common;

use common::Drive;
use femux_client::app::{Effect, Mode};
use femux_client::config::ClientConfig;

#[tokio::test(flavor = "multi_thread")]
async fn ui_001_three_modes() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    // terminal: keys go to the focused pane.
    assert_eq!(d.app.mode, Mode::Terminal);
    d.sent_kinds();
    d.type_text("echo mo$((1+1))de\n").await;
    d.wait_pane_text(&pane, "mo2de").await;
    assert!(
        d.sent_kinds()
            .iter()
            .all(|k| *k == "pane.input" || *k == "frame.ack")
    );
    // prefix esc: sticky normal; vim motions act on the layout, not the pane.
    d.press_all(&["ctrl+space", "escape"]).await;
    assert_eq!(d.app.mode, Mode::Normal);
    d.sent_kinds();
    d.press("v").await;
    assert!(d.sent_kinds().contains(&"layout.split"));
    d.press("h").await;
    assert!(d.sent_kinds().contains(&"layout.focus"));
    assert_eq!(d.app.mode, Mode::Normal, "normal mode is sticky");
    d.press("escape").await;
    assert_eq!(d.app.mode, Mode::Terminal);
    // copy mode reads scrollback and leaves with q.
    d.press_all(&["ctrl+space", "["]).await;
    assert_eq!(d.app.mode, Mode::Copy);
    assert!(d.sent_kinds().contains(&"scrollback.read"));
    d.press("q").await;
    assert_eq!(d.app.mode, Mode::Terminal);
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_002_prefix_is_one_shot_and_configurable() {
    let cfg = ClientConfig {
        prefix: vec!["ctrl+space".into(), "ctrl+b".into()],
        ..Default::default()
    };
    let mut d = Drive::new(cfg).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.type_text("cat -v\n").await;
    // prefix prefix sends the prefix to the pane: cat -v shows the NUL byte as ^@.
    d.press_all(&["ctrl+space", "ctrl+space"]).await;
    d.wait_pane_text(&pane, "^@").await;
    assert_eq!(d.app.mode_name(), "terminal", "the table is one-shot");
    // Either configured prefix arms the table; an unbound key is swallowed with a message.
    d.press("ctrl+b").await;
    assert_eq!(d.app.mode_name(), "prefix");
    d.sent_kinds();
    d.press("%").await;
    assert_eq!(d.app.mode_name(), "terminal");
    assert!(!d.sent_kinds().contains(&"pane.input"));
    assert!(d.app.message.as_deref().unwrap_or("").contains("not bound"));
    // The plain key still reaches the pane without the prefix.
    d.type_text("plain").await;
    d.wait_pane_text(&pane, "plain").await;
    // prefix d detaches.
    d.press_all(&["ctrl+space", "d"]).await;
    assert!(d.effects.contains(&Effect::Exit));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_004_prefixless_nav_is_off_by_default() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.sent_kinds();
    d.press("alt+h").await;
    let kinds = d.sent_kinds();
    assert!(kinds.contains(&"pane.input") && !kinds.contains(&"layout.focus"));

    let on = ClientConfig {
        prefixless_nav: true,
        ..Default::default()
    };
    let mut d = Drive::new(on).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.sent_kinds();
    d.press("alt+l").await;
    let kinds = d.sent_kinds();
    assert!(kinds.contains(&"layout.focus") && !kinds.contains(&"pane.input"));
}
