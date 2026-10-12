//! 0.5.0 acceptance (`chime plays on done`) and REQ-NOTIFY-001..003 on the client: the notify
//! plugin turns `notify.posted` into sound, bell and desktop effects on the client's machine.

mod common;

use std::time::Duration;

use common::Drive;
use femux_client::app::Effect;
use femux_client::config::ClientConfig;
use femux_plugin::run_daemon_plugin;
use femux_plugin_notify::{Client, Daemon};
use femux_proto::msg as m;

const ACTIONS: &str = r#"
[notify]
agent_done    = { sound = "/tmp/chime.wav", desktop = true }
agent_blocked = { sound = "bell", only_when_hidden = true }
"#;

#[tokio::test(flavor = "multi_thread")]
async fn chime_plays_when_the_hook_reports_done() {
    let actions = femux_plugin_notify::parse(ACTIONS).unwrap();
    let mut d = Drive::with(
        ClientConfig::default(),
        vec![Box::new(Client::fixed(actions.clone()))],
        80,
        24,
    )
    .await;
    let socket = d.h.socket.clone();
    let task = tokio::spawn(async move {
        run_daemon_plugin(
            &socket,
            Box::new(Daemon::with(actions, Box::new(|_, _| {}))),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pane = d.pane.clone();
    // What `femux integration hook claude-code` writes on Stop.
    for state in ["working", "done"] {
        d.obs
            .call(m::PaneSetMeta {
                pane_id: pane.clone(),
                set: Some(
                    [
                        ("agent.state", state),
                        ("agent.source", "hook"),
                        ("agent.name", "claude-code"),
                    ]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
                ),
                unset: None,
            })
            .await
            .unwrap();
    }
    effects_until(&mut d, |fx| {
        fx.contains(&Effect::Sound("/tmp/chime.wav".into()))
    })
    .await;
    assert!(d.effects.contains(&Effect::Desktop {
        title: "claude-code done".into(),
        body: format!("pane {pane}"),
    }));
    task.abort();
}

async fn effects_until(d: &mut Drive, mut f: impl FnMut(&[Effect]) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !f(&d.effects) {
        assert!(std::time::Instant::now() < deadline, "{:?}", d.effects);
        d.settle().await;
    }
}

/// REQ-NOTIFY-002/003: whichever daemon posted it, the client runs the action, and an
/// `only_when_hidden` entry stays quiet for a pane this view shows.
#[tokio::test(flavor = "multi_thread")]
async fn notify_002_003_the_client_acts_on_posted_events_unless_shown() {
    let actions = femux_plugin_notify::parse(ACTIONS).unwrap();
    let mut d = Drive::with(
        ClientConfig::default(),
        vec![Box::new(Client::fixed(actions))],
        80,
        24,
    )
    .await;
    let pane = d.pane.clone();
    let post = |pane_id: Option<String>, title: &str| m::NotifyPost {
        pane_id,
        title: title.into(),
        level: Some(m::NotifyLevel::Warn),
        event: Some("agent_blocked".into()),
    };
    d.obs.call(post(Some(pane.clone()), "shown")).await.unwrap();
    d.settle().await;
    assert!(!d.effects.contains(&Effect::Bell), "{:?}", d.effects);
    d.obs.call(post(None, "hidden")).await.unwrap();
    effects_until(&mut d, |fx| fx.contains(&Effect::Bell)).await;
}
