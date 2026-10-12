//! REQ-UI-003 (which-key after the delay) and REQ-UI-043 (command palette).

mod common;

use std::time::{Duration, Instant};

use common::Drive;
use femux_client::app::Overlay;
use femux_client::config::ClientConfig;
use femux_client::keys::key;
use femux_proto::Request;
use femux_proto::msg as m;

#[tokio::test(flavor = "multi_thread")]
async fn ui_003_which_key_shows_after_the_delay() {
    let cfg = ClientConfig {
        which_key_delay: Duration::from_millis(50),
        ..Default::default()
    };
    let plugin = femux_plugin_which_key::plugin().client.unwrap();
    let mut d = Drive::with(cfg, vec![plugin], 120, 30).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    let t0 = Instant::now();
    d.app.on_key(key("ctrl+space"), t0);
    assert!(!d.app.tick(t0 + Duration::from_millis(10)));
    assert!(
        !d.screen().contains("split below"),
        "nothing before the delay"
    );
    assert!(d.app.tick(t0 + Duration::from_millis(60)));
    assert!(matches!(d.app.overlay, Overlay::WhichKey));
    let screen = d.screen();
    assert!(screen.contains("prefix  ctrl+space pressed"), "{screen}");
    for chord in [
        "split below",
        "split right",
        "zoom",
        "new tab",
        "detach",
        "copy mode",
    ] {
        assert!(screen.contains(chord), "which-key lacks {chord}:\n{screen}");
    }
    // The next key runs its action and closes the panel.
    d.sent_kinds();
    d.press("s").await;
    assert!(matches!(d.app.overlay, Overlay::None));
    assert!(d.sent_kinds().contains(&"layout.split"));
    assert!(!d.screen().contains("split below"));

    // Without the plugin there is no panel, and the table still works.
    let cfg = ClientConfig {
        which_key_delay: Duration::from_millis(50),
        ..Default::default()
    };
    let mut d = Drive::new(cfg).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.app.on_key(key("ctrl+space"), t0);
    d.app.tick(t0 + Duration::from_millis(60));
    assert!(!d.screen().contains("split below"));
    d.press("z").await;
    assert!(d.sent_kinds().contains(&"layout.zoom"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_043_palette_fuzzy_searches_actions_and_cli_commands() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    d.press_all(&["ctrl+space", ":"]).await;
    assert!(matches!(d.app.overlay, Overlay::Palette(_)));
    d.type_text("splrig").await;
    let Overlay::Palette(p) = &d.app.overlay else {
        panic!("palette closed")
    };
    assert_eq!(p.matches()[0].label, "split right");
    assert!(d.screen().contains("> splrig"), "{}", d.screen());
    d.sent_kinds();
    d.press("enter").await;
    assert!(matches!(d.app.overlay, Overlay::None));
    let split = d
        .sent
        .iter()
        .find_map(|r| match r {
            Request::LayoutSplit(s) => Some(s.dir),
            _ => None,
        })
        .expect("palette ran split right");
    assert_eq!(split, m::SplitDir::H);
    // ctrl+p at the root opens it; CLI subcommands are listed and typed into the pane.
    d.press("ctrl+p").await;
    d.type_text("femux ls").await;
    let Overlay::Palette(p) = &d.app.overlay else {
        panic!("palette closed")
    };
    assert_eq!(p.matches()[0].label, "femux ls");
    d.sent.clear();
    d.press("enter").await;
    let pasted = d.sent.iter().any(|r| {
        matches!(r, Request::PaneInput(i) if i.events.iter().any(|e| matches!(e, m::InputEvent::Paste(p) if p.text == "femux ls ")))
    });
    assert!(pasted);
    // Escape closes without running anything.
    d.press("ctrl+p").await;
    d.press("escape").await;
    assert!(matches!(d.app.overlay, Overlay::None));
}
