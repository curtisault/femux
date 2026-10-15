//! REQ-LOG-003 (0.11.1): the bar says `DEBUG` while the daemon is in debug mode, following
//! `server.debug_changed`; REQ-LOG-007: the palette's `debug snapshot`.

mod common;

use common::Drive;
use femux_client::app::Effect;
use femux_client::bindings::Action;
use femux_client::config::ClientConfig;
use femux_proto::msg as m;

fn bar(d: &mut Drive) -> String {
    d.screen().lines().last().unwrap_or("").to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn log_003_the_bar_says_debug_while_the_daemon_is_in_debug_mode() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let pane = d.pane.clone();
    d.wait_pane_text(&pane, "$").await;
    assert!(!d.app.debug);
    assert!(!bar(&mut d).contains("DEBUG"), "{}", bar(&mut d));
    // Another client switches the daemon; this one follows the event.
    let r = d
        .obs
        .call(m::ServerDebug {
            level: Some(m::LogLevel::Debug),
        })
        .await
        .unwrap();
    assert_eq!(r.level, m::LogLevel::Debug);
    d.until("debug mode", |a| a.debug).await;
    let line = bar(&mut d);
    assert!(line.contains(" DEBUG "), "{line}");
    // The tabs move over for it rather than under it.
    assert!(line.find("DEBUG") < line.find(" 1 "), "{line}");
    d.obs
        .call(m::ServerDebug {
            level: Some(m::LogLevel::Warn),
        })
        .await
        .unwrap();
    d.until("debug mode off", |a| !a.debug).await;
    assert!(!bar(&mut d).contains("DEBUG"));
}

#[tokio::test(flavor = "multi_thread")]
async fn log_007_the_palette_offers_debug_snapshot() {
    let mut d = Drive::new(ClientConfig::default()).await;
    let a = Action::from_name("debug snapshot").expect("an action named debug snapshot");
    assert_eq!(a, Action::DebugSnapshot);
    assert_eq!(a.group(), "ui");
    d.app.run(a);
    d.settle().await;
    assert!(
        d.effects.iter().any(|e| matches!(e, Effect::DebugSnapshot)),
        "{:?}",
        d.effects
    );
}
