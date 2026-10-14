//! `femux reload` (ADR-013): both sides of the notify plugin re-read `[notify]` on
//! `config.reloaded`, so changing it needs no daemon restart. Its own test binary, since it
//! points `FEMUX_CONFIG` at a file of its own.

use femux_plugin::{ClientPlugin, DaemonPlugin, HostEffect};
use femux_plugin_notify::{Actions, Client, Daemon};
use femux_proto::msg as m;
use femux_proto::{Event, Request};

fn blocked(pane: &str) -> Event {
    Event::PaneMetaChanged(m::PaneMetaChanged {
        pane_id: pane.into(),
        set: [("agent.state".to_owned(), "blocked".to_owned())].into(),
        ..Default::default()
    })
}

fn posted_blocked() -> Event {
    Event::NotifyPosted(m::NotifyPosted {
        title: "codex blocked".into(),
        event: Some("agent_blocked".into()),
        ..Default::default()
    })
}

#[test]
fn notify_actions_follow_a_reload() {
    let dir = std::env::temp_dir().join(format!("femux-notify-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("config.toml");
    std::fs::write(&file, "").unwrap();
    // SAFETY: the only test in this binary, set before anything reads the environment.
    unsafe { std::env::set_var("FEMUX_CONFIG", &file) };

    let mut daemon = Daemon::new(Actions::new());
    let mut client = Client::new(Actions::new());
    assert!(
        daemon.on_event(&blocked("P1")).is_empty(),
        "no agent_blocked entry yet"
    );
    assert!(client.on_event(&posted_blocked()).is_empty());

    std::fs::write(&file, "[notify]\nagent_blocked = { sound = \"bell\" }\n").unwrap();
    let reloaded = Event::ConfigReloaded(m::ConfigReloaded::default());
    daemon.on_event(&reloaded);
    client.on_event(&reloaded);

    let raised = daemon.on_event(&blocked("P2"));
    assert!(
        matches!(raised.as_slice(), [Request::NotifyPost(p)] if p.event.as_deref() == Some("agent_blocked")),
        "{raised:?}"
    );
    assert!(matches!(
        client.on_event(&posted_blocked()).as_slice(),
        [HostEffect::Bell]
    ));
    std::fs::remove_dir_all(&dir).ok();
}
